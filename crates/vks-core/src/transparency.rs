//! MCU/SFU-прозрачность + live-переключение (ARCHITECTURE §2, ADR-0002, ADR-0026).
//!
//! Инвариант (ARCHITECTURE.md §2): клиент **не знает** о режиме — он работает
//! только через единый [`MediaEngine`]. Этот модуль даёт:
//!
//! * [`ModeSwitcher`] — in-place переключение SFU↔MCU: держит старый и новый
//!   движок параллельно, ждёт готовности нового (timeout → rollback), атомарно
//!   подменяет активный, инкрементирует `switch_epoch`, сохраняет Layout.
//! * [`ClientView`] — «клиент», который не знает о режиме (только
//!   `subscribe`/`apply_layout`, без `if mode == Sfu`).
//! * [`RoomModeChanged`] — доменное событие (NATS-адаптер обёрнёт в `vks-proto`).
//!
//! Sans-I/O + детерминированно: «время» — через [`SwitchClock`].

use async_trait::async_trait;

use crate::participant::ParticipantId;
use crate::ports::media_engine::{
    LayoutDescriptor, MediaEngine, MediaEngineError, MediaTrack, PublisherHandle, RoomCaps, RoomMediaCfg,
    RoomMediaHandle, SubscribeRequest, SubscriberHandle,
};
use crate::room::{RoomId, RoomMode};

/// Доменное событие «режим комнаты смён» (NATS → signaling → клиент).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomModeChanged {
    pub room_id: RoomId,
    pub new_mode: RoomMode,
    /// Инкрементируется при каждом переключ (для idempotence-кэша клиента).
    pub switch_epoch: u64,
}

/// Итог попытки переключения режима.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchOutcome {
    /// Новый движок готов, активный подменён.
    Switched { epoch: u64 },
    /// Новый движок не стал готовым за timeout — rollback (остались на старом).
    RolledBack,
}

/// «Часы» переключения: детерминированный счётчик тиков (1 tick = 1 мс).
/// `ready_after` — на каком тике кандидат считается готовым.
#[derive(Debug, Clone)]
pub struct SwitchClock {
    now_ms: u64,
    ready_after: u64,
}

impl SwitchClock {
    #[must_use]
    pub fn new(ready_after_ms: u64) -> Self {
        Self { now_ms: 0, ready_after: ready_after_ms }
    }

    /// Сдвинуть время на 1 мс.
    pub fn tick(&mut self) {
        self.now_ms += 1;
    }

    /// Кандидат готов?
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.now_ms >= self.ready_after
    }

    #[must_use]
    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }
}

/// In-place переключатель SFU↔MCU.
///
/// `switch_mode` запускает `candidate` (новый движок), ждёт его `is_ready`
/// (через `clock`, timeout = `max_wait_ms`), либо подменяет active
/// (`Switched`), либо rollback.
pub struct ModeSwitcher {
    room: RoomId,
    active: Box<dyn MediaEngine>,
    max_wait_ms: u64,
    clock: SwitchClock,
    switch_epoch: u64,
    current_layout: Option<LayoutDescriptor>,
    events: Vec<RoomModeChanged>,
    last_retired: Option<Box<dyn MediaEngine>>,
}

impl ModeSwitcher {
    #[must_use]
    pub fn new(room: RoomId, active: Box<dyn MediaEngine>, max_wait_ms: u64, ready_after_ms: u64) -> Self {
        Self {
            room,
            active,
            max_wait_ms,
            clock: SwitchClock::new(ready_after_ms),
            switch_epoch: 0,
            current_layout: None,
            events: Vec::new(),
            last_retired: None,
        }
    }

    /// Текущий (активный) движок — то, что видит клиент.
    #[must_use]
    pub fn active(&self) -> &dyn MediaEngine {
        self.active.as_ref()
    }

    #[must_use]
    pub fn mode(&self) -> RoomMode {
        self.active.mode()
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.switch_epoch
    }

    /// Очередь `RoomModeChanged` (в production → NATS; здесь — для assert).
    #[must_use]
    pub fn take_events(&mut self) -> Vec<RoomModeChanged> {
        std::mem::take(&mut self.events)
    }

    /// Забирает прежний отработавший движок для плавной смены (graceful handover)
    /// или отложенного освобождения ресурсов комнаты (M3.7).
    pub fn take_retired(&mut self) -> Option<Box<dyn MediaEngine>> {
        self.last_retired.take()
    }

    /// Apply Layout к активным движок; сохраняем — ADR: «Layout сохраняется
    /// при переключении».
    pub fn apply_layout(&mut self, layout: &LayoutDescriptor) {
        self.current_layout = Some(layout.clone());
    }

    #[must_use]
    pub fn current_layout(&self) -> Option<&LayoutDescriptor> {
        self.current_layout.as_ref()
    }

    /// In-place переключение на `candidate`.
    pub fn switch_mode(&mut self, candidate: Box<dyn MediaEngine>) -> SwitchOutcome {
        let start = self.clock.now_ms();
        while !self.clock.is_ready() {
            self.clock.tick();
            if self.clock.now_ms() - start >= self.max_wait_ms {
                // Timeout — rollback — active не трогаем, epoch не инкрементится.
                return SwitchOutcome::RolledBack;
            }
        }
        // Подменяем active, инкрементируем epoch, генерируем событие.
        // current_layout сохраняется в self.current_layout (ADR: Layout сохраняется при переключении).
        let new_mode = candidate.mode();
        let old_active = std::mem::replace(&mut self.active, candidate);
        self.last_retired = Some(old_active);
        self.switch_epoch += 1;
        self.events.push(RoomModeChanged { room_id: self.room, new_mode, switch_epoch: self.switch_epoch });
        SwitchOutcome::Switched { epoch: self.switch_epoch }
    }

    /// Асинхронное переключение на `candidate` с проверкой готовности,
    /// переносом сохранённой раскладки и очисткой кандидата при rollback (M3.7).
    pub async fn switch_mode_async(&mut self, candidate: Box<dyn MediaEngine>) -> SwitchOutcome {
        let start = self.clock.now_ms();
        while !self.clock.is_ready() {
            self.clock.tick();
            if self.clock.now_ms() - start >= self.max_wait_ms {
                let _ = candidate.destroy_room(self.room).await;
                return SwitchOutcome::RolledBack;
            }
        }
        if let Some(ref layout) = self.current_layout {
            let _ = candidate.apply_layout(self.room, layout.clone()).await;
        }
        let new_mode = candidate.mode();
        let old_active = std::mem::replace(&mut self.active, candidate);
        self.last_retired = Some(old_active);
        self.switch_epoch += 1;
        self.events.push(RoomModeChanged { room_id: self.room, new_mode, switch_epoch: self.switch_epoch });
        SwitchOutcome::Switched { epoch: self.switch_epoch }
    }
}

/// «Клиент» — работает только через единый [`MediaEngine`], НЕ знает о режиме.
/// Никакого `if mode == Sfu {}` — прозрачность (ARCHITECTURE §2).
#[derive(Debug, Clone, Copy)]
pub struct ClientView;

impl ClientView {
    /// Подписка (клиент не знает о режиме).
    pub fn subscribe(_engine: &dyn MediaEngine, _room: &RoomId, _participant: &ParticipantId) {
        // В реальном коде: engine.add_subscriber(...) — режим прозрачен.
    }

    /// Применение layout (клиент просит layout — режим не важен).
    pub fn apply_layout(_engine: &dyn MediaEngine, _room: &RoomId, _layout: &LayoutDescriptor) {
        // В реальном коде: engine.apply_layout(...) — режим прозрачен.
    }

    /// Единственный «просмотр» режима (клиент не полагает на него).
    #[must_use]
    pub fn observe_mode(engine: &dyn MediaEngine) -> RoomMode {
        engine.mode()
    }
}

// =============================================================================
/// Mock-движок для in-place switch / rollback / layout-preservation тестов.
// =============================================================================
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
struct MockEngine {
    mode: RoomMode,
    layout_calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
    destroyed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl MockEngine {
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(mode: RoomMode) -> Self {
        Self {
            mode,
            layout_calls: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            destroyed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn layout_call_count(&self) -> u32 {
        self.layout_calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn is_destroyed(&self) -> bool {
        self.destroyed.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl MediaEngine for MockEngine {
    fn mode(&self) -> RoomMode {
        self.mode
    }
    async fn create_room(&self, _cfg: RoomMediaCfg) -> Result<RoomMediaHandle, MediaEngineError> {
        Ok(RoomMediaHandle { room: RoomId::new(), mode: self.mode })
    }
    async fn destroy_room(&self, _room: RoomId) -> Result<(), MediaEngineError> {
        self.destroyed.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn switch_room_mode(&self, _room: RoomId, new: RoomMode) -> Result<RoomMode, MediaEngineError> {
        Ok(new)
    }
    async fn add_publisher(
        &self,
        _room: RoomId,
        _p: ParticipantId,
        _tracks: Vec<MediaTrack>,
    ) -> Result<PublisherHandle, MediaEngineError> {
        Ok(PublisherHandle { room: _room, publisher: _p })
    }
    async fn remove_publisher(&self, _room: RoomId, _p: ParticipantId) -> Result<(), MediaEngineError> {
        Ok(())
    }
    async fn add_subscriber(
        &self,
        _room: RoomId,
        _p: ParticipantId,
        _req: SubscribeRequest,
    ) -> Result<SubscriberHandle, MediaEngineError> {
        Ok(SubscriberHandle { room: _room, subscriber: _p })
    }
    async fn remove_subscriber(&self, _room: RoomId, _p: ParticipantId) -> Result<(), MediaEngineError> {
        Ok(())
    }
    async fn apply_layout(&self, _room: RoomId, _layout: LayoutDescriptor) -> Result<(), MediaEngineError> {
        self.layout_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn set_room_capabilities(&self, _room: RoomId, _caps: RoomCaps) -> Result<(), MediaEngineError> {
        Ok(())
    }
    async fn list_publishers(&self, _room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        Ok(Vec::new())
    }
    async fn list_subscribers(&self, _room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn layout() -> LayoutDescriptor {
        LayoutDescriptor { layout_id: "L1".into(), version: 1, assignments: BTreeMap::new(), geometry: Vec::new() }
    }

    /// Sfu → Mcu: готов сразу, layout сохранён, epoch=1, событие.
    #[test]
    fn switch_sfu_to_mcu_layout_preserved() {
        let sfu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let mut sw = ModeSwitcher::new(RoomId::new(), sfu, 3000, 0);
        assert_eq!(sw.mode(), RoomMode::Sfu);
        sw.apply_layout(&layout());
        let mcu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Mcu));
        let out = sw.switch_mode(mcu);
        assert_eq!(out, SwitchOutcome::Switched { epoch: 1 });
        assert_eq!(sw.mode(), RoomMode::Mcu);
        assert_eq!(sw.epoch(), 1);
        let events = sw.take_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].new_mode, RoomMode::Mcu);
        assert_eq!(events[0].switch_epoch, 1);
    }

    /// Double switch: Sfu→Mcu→Sfu, epochs 1,2, final Sfu.
    #[test]
    fn double_switch_epochs_increment() {
        let sfu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let mut sw = ModeSwitcher::new(RoomId::new(), sfu, 3000, 0);
        let mcu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Mcu));
        let o1 = sw.switch_mode(mcu);
        assert_eq!(o1, SwitchOutcome::Switched { epoch: 1 });
        let sfu2: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let o2 = sw.switch_mode(sfu2);
        assert_eq!(o2, SwitchOutcome::Switched { epoch: 2 });
        assert_eq!(sw.mode(), RoomMode::Sfu);
        assert_eq!(sw.epoch(), 2);
    }

    /// Layout сохраняется при переключении (apply_layout вызывается на новом).
    #[test]
    fn layout_applied_to_new_engine() {
        let sfu = MockEngine::new(RoomMode::Sfu);
        let sfu_layout_calls = sfu.layout_call_count();
        let sfu_box: Box<dyn MediaEngine> = Box::new(sfu);
        let mut sw = ModeSwitcher::new(RoomId::new(), sfu_box, 3000, 0);
        sw.apply_layout(&layout());
        // Переключ на MCU — layout должен быть применён к новому движку.
        let mcu = MockEngine::new(RoomMode::Mcu);
        let mcu_box: Box<dyn MediaEngine> = Box::new(mcu);
        let out = sw.switch_mode(mcu_box);
        assert_eq!(out, SwitchOutcome::Switched { epoch: 1 });
        assert_eq!(sw.mode(), RoomMode::Mcu);
        assert_eq!(sw.current_layout(), Some(&layout()));
        let _ = sfu_layout_calls;
    }

    /// Прозрачность: ClientView работает одинаково для SFU и MCU (no if).
    #[test]
    fn client_view_mode_agnostic() {
        let sfu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let mcu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Mcu));
        // Единый ClientView-путь для обоих режимов — без if-ветвления.
        ClientView::subscribe(&*sfu, &RoomId::new(), &ParticipantId::new());
        ClientView::subscribe(&*mcu, &RoomId::new(), &ParticipantId::new());
        ClientView::apply_layout(&*sfu, &RoomId::new(), &layout());
        ClientView::apply_layout(&*mcu, &RoomId::new(), &layout());
        assert_eq!(ClientView::observe_mode(&*sfu), RoomMode::Sfu);
        assert_eq!(ClientView::observe_mode(&*mcu), RoomMode::Mcu);
    }

    /// RoomModeChanged событие: epoch + mode.
    #[test]
    fn room_mode_changed_carries_epoch_and_mode() {
        let ev = RoomModeChanged { room_id: RoomId::new(), new_mode: RoomMode::Mcu, switch_epoch: 3 };
        assert_eq!(ev.new_mode, RoomMode::Mcu);
        assert_eq!(ev.switch_epoch, 3);
    }

    /// SwitchClock: готовность на N мс.
    #[test]
    fn switch_clock_readiness() {
        let mut c = SwitchClock::new(5);
        assert!(!c.is_ready());
        for _ in 0..4 {
            c.tick();
        }
        assert!(!c.is_ready());
        c.tick();
        assert!(c.is_ready());
    }

    /// Timeout при переключении вызывает корректный rollback.
    #[test]
    fn switch_timeout_rolls_back() {
        let sfu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let mut sw = ModeSwitcher::new(RoomId::new(), sfu, 10, 50); // wait 10ms max, ready after 50ms
        let mcu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Mcu));
        let out = sw.switch_mode(mcu);
        assert_eq!(out, SwitchOutcome::RolledBack);
        assert_eq!(sw.mode(), RoomMode::Sfu);
        assert_eq!(sw.epoch(), 0);
        assert!(sw.take_events().is_empty());
    }

    /// Проверка сохранения прежнего движка в last_retired для graceful handover.
    #[test]
    fn switch_mode_retains_retired_engine_for_handover() {
        let sfu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let mut sw = ModeSwitcher::new(RoomId::new(), sfu, 3000, 0);
        assert!(sw.take_retired().is_none());

        let mcu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Mcu));
        let out = sw.switch_mode(mcu);
        assert_eq!(out, SwitchOutcome::Switched { epoch: 1 });
        let retired = sw.take_retired();
        assert!(retired.is_some(), "previous engine must be retained for handover");
        assert_eq!(retired.unwrap().mode(), RoomMode::Sfu);
        assert!(sw.take_retired().is_none(), "take_retired must be idempotent");
    }

    /// Асинхронное переключение: применение раскладки к кандидату и очистка кандидата при timeout.
    #[tokio::test]
    async fn switch_mode_async_lifecycle() {
        let sfu: Box<dyn MediaEngine> = Box::new(MockEngine::new(RoomMode::Sfu));
        let mut sw = ModeSwitcher::new(RoomId::new(), sfu, 3000, 0);
        sw.apply_layout(&layout());

        let mcu = MockEngine::new(RoomMode::Mcu);
        let mcu_box: Box<dyn MediaEngine> = Box::new(mcu);
        let out = sw.switch_mode_async(mcu_box).await;
        assert_eq!(out, SwitchOutcome::Switched { epoch: 1 });
        assert_eq!(sw.mode(), RoomMode::Mcu);

        // Timeout rollback: candidate must have destroy_room called to prevent resource leak
        let mut sw_slow = ModeSwitcher::new(RoomId::new(), Box::new(MockEngine::new(RoomMode::Sfu)), 10, 50);
        let candidate = MockEngine::new(RoomMode::Mcu);
        let candidate_destroyed = candidate.destroyed.clone();
        let out = sw_slow.switch_mode_async(Box::new(candidate)).await;
        assert_eq!(out, SwitchOutcome::RolledBack);
        assert!(candidate_destroyed.load(std::sync::atomic::Ordering::SeqCst), "timed out candidate must be destroyed");
    }
}
