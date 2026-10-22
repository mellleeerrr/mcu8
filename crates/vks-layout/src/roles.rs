//! Роли окон в комбинированных раскладках (спецификация Codian MCU 4200, §1–§2).
//!
//! Раскладка Codian различает четыре роли окна:
//!
//! * **default** — обычное окно, участники назначаются в порядке прихода;
//! * **fixed** — окно закреплено за конкретным участником;
//! * **active** — окно активного докладчика;
//! * **rolling** — окно ротации: участники ходят по кругу.
//!
//! Назначение — чистая функция: на входе роли окон, закрепления, активный
//! докладчик и список участников в порядке прихода; на выходе — участник в
//! каждом окне. Никакого состояния и никаких блокировок: движок MCU
//! пересчитывает назначение при изменении состава или ролей.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::core::ParticipantId;

/// Роль окна в комбинированной раскладке.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SlotRole {
    /// Обычное окно: участники назначаются в порядке прихода.
    #[default]
    Default,
    /// Окно закреплено за конкретным участником.
    Fixed,
    /// Окно активного докладчика.
    Active,
    /// Окно ротации: участники ходят по кругу.
    Rolling,
}

impl SlotRole {
    /// Каноническое имя роли (как в спецификации).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Fixed => "fixed",
            Self::Active => "active",
            Self::Rolling => "rolling",
        }
    }
}

/// Состояние ротации: участники и позиция, с которой начнётся следующий круг.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RotationState {
    /// Участники ротации в порядке текущего круга.
    pub participants: Vec<ParticipantId>,
    /// Позиция в `participants`, с которой начнётся следующий круг.
    pub cursor: usize,
}

/// Назначает участников на окна по ролям.
///
/// * `fixed` — закреплённые окна (номер окна → участник); имеют приоритет над всеми ролями;
/// * `active` — активный докладчик (попадает в первое свободное окно роли `active`);
/// * `participants` — участники в порядке прихода, из них формируются ротация и обычные окна.
///
/// Возвращает вектор по числу окон: `None` — окно без участника (участников
/// не хватило или окно закреплено за участником, которого нет в комнате).
pub fn assign_by_role(
    roles: &[SlotRole],
    fixed: &BTreeMap<usize, ParticipantId>,
    active: Option<ParticipantId>,
    participants: &[ParticipantId],
) -> Vec<Option<ParticipantId>> {
    let mut slots: Vec<Option<ParticipantId>> = vec![None; roles.len()];
    let mut taken: Vec<ParticipantId> = Vec::with_capacity(roles.len());

    // 1. Закреплённые окна — приоритет №1.
    for (index, participant) in fixed {
        if let Some(slot) = slots.get_mut(*index) {
            if roles.get(*index) == Some(&SlotRole::Fixed) {
                *slot = Some(*participant);
                taken.push(*participant);
            }
        }
    }

    // 2. Активный докладчик — приоритет №2.
    if let Some(speaker) = active {
        if !taken.contains(&speaker) {
            if let Some(index) = roles.iter().position(|role| *role == SlotRole::Active) {
                slots[index] = Some(speaker);
                taken.push(speaker);
            }
        }
    }

    // 3. Окна ротации: участники по кругу, начиная с первого незанятого.
    let rolling: Vec<usize> =
        roles.iter().enumerate().filter(|(_, role)| **role == SlotRole::Rolling).map(|(index, _)| index).collect();
    let rolling_pool: Vec<ParticipantId> =
        participants.iter().copied().filter(|participant| !taken.contains(participant)).collect();
    for (step, index) in rolling.iter().enumerate() {
        if let Some(participant) = rolling_pool.get(step) {
            slots[*index] = Some(*participant);
            taken.push(*participant);
        }
    }

    // 4. Обычные окна — оставшиеся участники в порядке прихода.
    let mut next = 0;
    for (index, role) in roles.iter().enumerate() {
        if *role != SlotRole::Default || slots[index].is_some() {
            continue;
        }
        while next < participants.len() && taken.contains(&participants[next]) {
            next += 1;
        }
        if let Some(participant) = participants.get(next) {
            slots[index] = Some(*participant);
            taken.push(*participant);
            next += 1;
        }
    }

    slots
}

/// Собирает состояние ротации по участникам, попавшим в окна роли `rolling`.
pub fn rotation_state(roles: &[SlotRole], slots: &[Option<ParticipantId>]) -> RotationState {
    let mut participants = Vec::new();
    for (index, role) in roles.iter().enumerate() {
        if *role != SlotRole::Rolling {
            continue;
        }
        if let Some(Some(participant)) = slots.get(index) {
            participants.push(*participant);
        }
    }
    RotationState { participants, cursor: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(value: u128) -> ParticipantId {
        ParticipantId::new(value)
    }

    #[test]
    fn fixed_wins_over_every_other_role() {
        let roles = [SlotRole::Fixed, SlotRole::Active, SlotRole::Default];
        let mut fixed = BTreeMap::new();
        fixed.insert(0usize, pid(7));
        let participants = [pid(1), pid(2), pid(3)];
        let slots = assign_by_role(&roles, &fixed, Some(pid(2)), &participants);
        assert_eq!(slots[0], Some(pid(7)));
        assert_eq!(slots[1], Some(pid(2)), "активный докладчик занимает окно active");
        assert_eq!(slots[2], Some(pid(1)), "обычное окно получает первого свободного");
    }

    #[test]
    fn rolling_windows_rotate_in_join_order() {
        let roles = [SlotRole::Rolling, SlotRole::Rolling, SlotRole::Rolling];
        let participants = [pid(1), pid(2), pid(3), pid(4)];
        let slots = assign_by_role(&roles, &BTreeMap::new(), None, &participants);
        assert_eq!(slots, vec![Some(pid(1)), Some(pid(2)), Some(pid(3))]);
        assert!(!slots.contains(&Some(pid(4))), "окон меньше, чем участников — лишний участник не показывается");
    }

    #[test]
    fn active_speaker_is_not_duplicated_in_rolling() {
        let roles = [SlotRole::Active, SlotRole::Rolling, SlotRole::Rolling];
        let participants = [pid(1), pid(2), pid(3)];
        let slots = assign_by_role(&roles, &BTreeMap::new(), Some(pid(1)), &participants);
        assert_eq!(slots[0], Some(pid(1)));
        assert_eq!(slots[1], Some(pid(2)));
        assert_eq!(slots[2], Some(pid(3)));
    }

    #[test]
    fn empty_room_leaves_all_windows_free() {
        let roles = [SlotRole::Fixed, SlotRole::Active, SlotRole::Rolling, SlotRole::Default];
        let slots = assign_by_role(&roles, &BTreeMap::new(), None, &[]);
        assert_eq!(slots, vec![None; 4]);
    }

    #[test]
    fn rotation_state_collects_only_rolling_windows() {
        let roles = [SlotRole::Rolling, SlotRole::Default, SlotRole::Rolling];
        let slots = [Some(pid(5)), Some(pid(6)), Some(pid(7))];
        let state = rotation_state(&roles, &slots);
        assert_eq!(state.participants, vec![pid(5), pid(7)]);
    }
}
