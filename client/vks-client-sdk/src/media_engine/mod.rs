//! Media engine SDK: выбор renderer (SFU vs MCU) + composition через vks-layout.
//!
//! Основание: `docs/architecture/ARCHITECTURE.md` §241-242
//! (media_engine/sfu_renderer.rs, mcu_renderer.rs), `docs/DEVELOPMENT_PLAN.md`
//! Фаза 2 (vks-client-sdk: media_engine/).
//!
//! SFU: клиент рендерит N отдельных треков через `vks_layout` (grid/focus).
//! MCU: клиент рендерит ОДИН композитный трек (сервер уже смешал) —
//! `sfu_renderer` и `mcu_renderer` — два, renderer.

mod mcu_renderer;
mod sfu_renderer;

pub use mcu_renderer::McuRenderer;
pub use sfu_renderer::SfuRenderer;

use crate::render_surface::{DrawCommand, RenderSurface};

/// Режим комнаты (SFU / MCU).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoomMode {
    /// SFU: клиент получает N отдельных треков.
    #[default]
    Sfu,
    /// MCU: клиент получает 1 композитный трек.
    Mcu,
}

impl RoomMode {
    /// Строковое имя режима (совпадает с `vks_proto::media::RoomMode`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sfu => "sfu",
            Self::Mcu => "mcu",
        }
    }

    /// Разбор строки режима (`sfu` | `mcu`).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sfu" => Some(Self::Sfu),
            "mcu" => Some(Self::Mcu),
            _ => None,
        }
    }
}

/// Media engine SDK: хранит текущий режим + renderer.
#[derive(Debug, Clone)]
pub struct MediaEngine {
    mode: RoomMode,
    /// Раскладка комнаты, пришедшая от сервера (`LayoutChanged`).
    ///
    /// `None` — сервер ещё не прислал раскладку: клиент рисует авто-сетку.
    /// Как только раскладка приходит, она становится единственным источником
    /// геометрии плиток.
    layout: Option<vks_layout::NormalizedLayout>,
    sfu: SfuRenderer,
    mcu: McuRenderer,
}

impl MediaEngine {
    #[must_use]
    pub fn new() -> Self {
        Self { mode: RoomMode::Sfu, layout: None, sfu: SfuRenderer::new(), mcu: McuRenderer::new() }
    }

    /// Текущий режим.
    #[must_use]
    pub fn mode(&self) -> RoomMode {
        self.mode
    }

    /// Переключает режим (по RoomModeChanged signaling).
    pub fn switch_mode(&mut self, mode: RoomMode) {
        self.mode = mode;
    }

    /// Применяет раскладку комнаты (по LayoutChanged signaling).
    pub fn set_layout(&mut self, layout: vks_layout::NormalizedLayout) {
        self.layout = Some(layout);
    }

    /// Текущая раскладка комнаты, если сервер её присылал.
    #[must_use]
    pub fn layout(&self) -> Option<&vks_layout::NormalizedLayout> {
        self.layout.as_ref()
    }

    /// Рендерит кадр: SFU → N треков через layout; MCU → 1 композитный.
    pub fn render(&self, participants: &[String], surface: &dyn RenderSurface) {
        match self.mode {
            RoomMode::Sfu => {
                // Раскладка сервера важнее авто-сетки: только она описывает
                // фактические позиции плиток (в т.ч. схемы Codian). Пустая
                // раскладка (режим auto — геометрию строит MCU) игнорируется:
                // иначе клиент нарисовал бы пустой экран.
                let cmds = match self.layout.as_ref().filter(|layout| !layout.slots.is_empty()) {
                    Some(layout) => self.sfu.compose_from_normalized(layout, surface.size()),
                    None => self.sfu.compose(participants, surface.size()),
                };
                surface.render(&cmds);
            }
            RoomMode::Mcu => {
                let cmds = self.mcu.compose(surface.size());
                surface.render(&cmds);
            }
        }
    }
}

impl Default for MediaEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Заглушка DrawCommand re-export (для внешнего использования).
pub type Dc = DrawCommand;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_surface::RecordingSurface;

    #[test]
    fn default_mode_sfu() {
        let e = MediaEngine::default();
        assert_eq!(e.mode(), RoomMode::Sfu);
    }

    #[test]
    fn switch_to_mcu() {
        let mut e = MediaEngine::default();
        e.switch_mode(RoomMode::Mcu);
        assert_eq!(e.mode(), RoomMode::Mcu);
    }

    #[test]
    fn sfu_render_n_participants() {
        let e = MediaEngine::default();
        let surface = RecordingSurface::new();
        e.render(&["a".into(), "b".into(), "c".into(), "d".into()], &surface);
        // SFU: 4 трека → 4 DrawCommand (grid 2x2)
        let cmds = surface.take();
        assert_eq!(cmds.len(), 4);
    }

    #[test]
    fn mcu_render_single_composite() {
        let mut e = MediaEngine::default();
        e.switch_mode(RoomMode::Mcu);
        let surface = RecordingSurface::new();
        e.render(&["a".into(), "b".into()], &surface);
        // MCU: 1 композитный трек (независимо от N участников)
        let cmds = surface.take();
        assert_eq!(cmds.len(), 1);
    }

    #[test]
    fn mode_switch_changes_render_count() {
        let mut e = MediaEngine::default();
        let surface = RecordingSurface::new();
        // SFU
        e.render(&["a".into(), "b".into()], &surface);
        assert_eq!(surface.take().len(), 2);
        // MCU
        e.switch_mode(RoomMode::Mcu);
        e.render(&["a".into(), "b".into()], &surface);
        assert_eq!(surface.take().len(), 1);
    }

    #[test]
    fn server_layout_replaces_auto_grid() {
        let mut e = MediaEngine::default();
        let surface = RecordingSurface::new();
        // Авто-сетка: 3 участника → 3 плитки.
        e.render(&["a".into(), "b".into(), "c".into()], &surface);
        assert_eq!(surface.take().len(), 3);

        // Раскладка сервера: две полосы (второй слот — половина по высоте).
        let layout = vks_layout::NormalizedLayout {
            version: 1,
            kind: vks_layout::LayoutKind::Custom,
            slots: vec![
                vks_layout::NormalizedSlot {
                    id: vks_layout::SlotId::new(0),
                    participant_id: Some(vks_layout::ParticipantId::new(1)),
                    rect: vks_layout::NormalizedRect::new(0.0, 0.0, 1.0, 0.5),
                    z_index: 0,
                    pinned: false,
                    is_focus: false,
                },
                vks_layout::NormalizedSlot {
                    id: vks_layout::SlotId::new(1),
                    participant_id: Some(vks_layout::ParticipantId::new(2)),
                    rect: vks_layout::NormalizedRect::new(0.0, 0.5, 1.0, 0.5),
                    z_index: 0,
                    pinned: false,
                    is_focus: false,
                },
            ],
        };
        e.set_layout(layout);
        e.render(&["a".into(), "b".into(), "c".into()], &surface);
        let cmds = surface.take();
        assert_eq!(cmds.len(), 2, "плитки берутся из раскладки сервера, а не из авто-сетки");
    }

    /// Участник доезжает от прото-раскладки до команды отрисовки: uuid →
    /// u128 → uuid не должен терять идентичность.
    #[test]
    fn participant_id_survives_the_proto_round_trip() {
        let mut e = MediaEngine::default();
        let surface = RecordingSurface::new();
        let participant = uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
        let proto = vks_proto::signaling::Layout {
            version: 1,
            kind: "grid".into(),
            slots: vec![vks_proto::signaling::Slot {
                id: 0,
                rect: Some(vks_proto::signaling::Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
                z: 0,
                assignment: vks_proto::signaling::SlotAssignment::Fixed as i32,
                participant_id: Some(participant.to_string()),
            }],
        };
        e.set_layout(crate::layout::normalized_layout_from_proto(&proto));
        e.render(&[], &surface);
        let cmds = surface.take();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].participant_id, participant.to_string());
    }
}
