//! Планировщик раскладки зрителя (ADR-0029, фаза 0–2).
//!
//! Спецификация Codian MCU 4200, §2 «Архитектура и управление раскладками»:
//!
//! * «Continuous Presence» — MCU формирует **индивидуальную раскладку для
//!   каждого подключённого терминала**;
//! * при подсчёте числа участников **не учитывается собственный видеопоток
//!   смотрящего** («чтобы не дробить экран на более мелкие ячейки»);
//! * **самопросмотр — низший приоритет** при заполнении ячеек;
//! * остальные ячейки заполняются по убыванию речевой активности.
//!
//! Модель (вариант C ADR-0029): **геометрия окон общая, заселение —
//! персональное**. Планировщик — чистая функция:
//!
//! ```text
//! plan_viewer(&ViewerContext, viewer) -> ViewerPlan
//! ```
//!
//! Ни состояния, ни пикселей ⇒ план сравним и пригоден как ключ кэша
//! персональных кадров. Размер семейства (сколько ячеек) в плане не
//! вычисляется: ячейки приходят готовыми, решает вызывающая сторона
//! (движок, фаза 3 ADR-0029 — семейство по числу *остальных* участников).
//!
//! Правило заселения:
//!
//! * зритель, публикующий видео, **не занимает** общую ячейку: его камера
//!   идёт в ячейку **низшего приоритета** (последнюю в списке ячеек) — это
//!   самопросмотр;
//! * остальные ячейки занимают остальные участники в порядке прихода;
//! * зритель без видео (наблюдатель, рекордер) самопросмотра не получает и
//!   видит общую сетку без изменений.

use crate::core::ParticipantId;
use crate::rect::Rect;

/// Окно плана зрителя: геометрия общая, заселение персональное.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanCell {
    /// Номер слота (общий с базовой сеткой).
    pub slot: u32,
    /// Участник в окне (`None` — окно пустое).
    pub participant: Option<ParticipantId>,
    /// Геометрия окна в пикселях канваса.
    pub rect: Rect,
    /// Порядок наложения (общий с базовой сеткой).
    pub z: i32,
}

impl PlanCell {
    /// Создаёт окно плана.
    pub const fn new(slot: u32, participant: Option<ParticipantId>, rect: Rect, z: i32) -> Self {
        Self { slot, participant, rect, z }
    }
}

/// План раскладки для конкретного зрителя.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerPlan {
    /// Версия плана (из контекста; монотонная).
    pub version: u64,
    /// Зритель, для которого построен план.
    pub viewer: ParticipantId,
    /// Окна общей сетки с персональным заселением.
    pub cells: Vec<PlanCell>,
    /// Окно самопросмотра (ячейка низшего приоритета) — `None`, если
    /// самопросмотр выключен или зритель не публикует видео.
    pub self_view: Option<PlanCell>,
}

/// Политика самопросмотра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelfViewPolicy {
    /// Самопросмотр выключен: зритель занимает общую ячейку как все.
    #[default]
    Disabled,
    /// Самопросмотр занимает ячейку низшего приоритета (последнюю).
    LowestPriorityCell,
}

/// Вход планировщика: общая геометрия + состав комнаты.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewerContext {
    /// Версия плана (монотонная; движок инкрементирует при смене раскладки).
    pub version: u64,
    /// Окна общей сетки в порядке приоритета (последнее — низший приоритет).
    pub cells: Vec<PlanCell>,
    /// Участники комнаты в порядке прихода (S12).
    pub participants: Vec<ParticipantId>,
    /// Политика самопросмотра.
    pub self_view: SelfViewPolicy,
}

impl ViewerContext {
    /// Создаёт контекст планирования.
    pub fn new(
        version: u64,
        cells: Vec<PlanCell>,
        participants: Vec<ParticipantId>,
        self_view: SelfViewPolicy,
    ) -> Self {
        Self { version, cells, participants, self_view }
    }
}

/// Строит план раскладки для зрителя (чистая функция, ADR-0029).
///
/// Зритель без видео и при выключенной политике самопросмотра получает
/// общую сетку: ячейки заполняются участниками в порядке прихода.
pub fn plan_viewer(context: &ViewerContext, viewer: ParticipantId) -> ViewerPlan {
    let publishes = context.participants.contains(&viewer);
    let self_view_enabled = publishes && context.self_view == SelfViewPolicy::LowestPriorityCell;

    // Ячейка самопросмотра — низший приоритет, то есть последняя.
    let self_view_slot = self_view_enabled.then(|| context.cells.len().saturating_sub(1)).filter(|_| !context.cells.is_empty());
    let shared_len = if self_view_slot.is_some() { context.cells.len() - 1 } else { context.cells.len() };

    // Участники, занимающие общие ячейки, — по приходу. Зритель исключён
    // только когда его камера уходит в самопросмотр: иначе он занимает
    // обычную ячейку, как все.
    let occupants: Vec<ParticipantId> = context
        .participants
        .iter()
        .copied()
        .filter(|pid| !self_view_enabled || *pid != viewer)
        .collect();

    let mut cells: Vec<PlanCell> = Vec::with_capacity(shared_len);
    for (index, cell) in context.cells.iter().take(shared_len).enumerate() {
        let participant = occupants.get(index).copied();
        cells.push(PlanCell::new(cell.slot, participant, cell.rect, cell.z));
    }

    let self_view = self_view_slot.map(|index| {
        let cell = &context.cells[index];
        PlanCell::new(cell.slot, Some(viewer), cell.rect, cell.z)
    });

    ViewerPlan { version: context.version, viewer, cells, self_view }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u128) -> ParticipantId {
        ParticipantId::new(n)
    }

    fn cell(slot: u32) -> PlanCell {
        PlanCell::new(slot, None, Rect::new(0, 0, 100, 100), 0)
    }

    fn context(participants: Vec<ParticipantId>, policy: SelfViewPolicy) -> ViewerContext {
        ViewerContext::new(7, (0..3).map(cell).collect(), participants, policy)
    }

    #[test]
    fn самопросмотр_занимает_ячейку_низшего_приоритета() {
        let ctx = context(vec![pid(1), pid(2), pid(3)], SelfViewPolicy::LowestPriorityCell);
        let plan = plan_viewer(&ctx, pid(2));

        // Зрителя нет в общих ячейках, остальные — в порядке прихода.
        assert_eq!(plan.cells.len(), 2);
        assert_eq!(plan.cells[0].participant, Some(pid(1)));
        assert_eq!(plan.cells[1].participant, Some(pid(3)));
        assert!(plan.cells.iter().all(|c| c.participant != Some(pid(2))));

        // Самопросмотр — последняя ячейка (слот 2).
        let self_view = plan.self_view.expect("самопросмотр");
        assert_eq!(self_view.slot, 2);
        assert_eq!(self_view.participant, Some(pid(2)));
        assert_eq!(plan.version, 7);
    }

    #[test]
    fn без_видео_зритель_видит_общую_сетку() {
        let ctx = context(vec![pid(1), pid(2)], SelfViewPolicy::LowestPriorityCell);
        let plan = plan_viewer(&ctx, pid(99));

        assert!(plan.self_view.is_none(), "у наблюдателя нет самопросмотра");
        assert_eq!(plan.cells.len(), 3, "общие ячейки не тронуты");
        assert_eq!(plan.cells[0].participant, Some(pid(1)));
        assert_eq!(plan.cells[1].participant, Some(pid(2)));
        assert_eq!(plan.cells[2].participant, None);
    }

    #[test]
    fn выключенная_политика_оставляет_зрителя_в_общей_сетке() {
        let ctx = context(vec![pid(1), pid(2), pid(3)], SelfViewPolicy::Disabled);
        let plan = plan_viewer(&ctx, pid(2));

        assert!(plan.self_view.is_none());
        assert_eq!(plan.cells.len(), 3);
        assert_eq!(plan.cells[0].participant, Some(pid(1)));
        assert_eq!(plan.cells[1].participant, Some(pid(2)));
        assert_eq!(plan.cells[2].participant, Some(pid(3)));
    }

    #[test]
    fn план_детерминирован_и_не_зависит_от_порядка_вызовов() {
        let ctx = context(vec![pid(1), pid(2), pid(3)], SelfViewPolicy::LowestPriorityCell);
        let first = plan_viewer(&ctx, pid(1));
        let second = plan_viewer(&ctx, pid(1));
        assert_eq!(first, second);

        // Другой зритель — другой план (персональное заселение).
        let other = plan_viewer(&ctx, pid(3));
        assert_ne!(first, other);
    }

    #[test]
    fn пустая_сетка_даёт_пустой_план() {
        let ctx = ViewerContext::new(1, Vec::new(), vec![pid(1)], SelfViewPolicy::LowestPriorityCell);
        let plan = plan_viewer(&ctx, pid(1));
        assert!(plan.cells.is_empty());
        assert!(plan.self_view.is_none());
    }
}
