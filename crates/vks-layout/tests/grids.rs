//! Контракт конструктора «Схема расположения участников» (`vks_grid_*`,
//! `vks_pin_*`): число окон, полное покрытие канваса без щелей, отсутствие
//! пересечений, строгие 16:9 у идеальных галерей.

use vks_layout::grids::{self, GridScheme, PinSide};

const W: f64 = 1280.0;
const H: f64 = 720.0;

fn variants(count: usize) -> Vec<GridScheme> {
    let mut list = vec![GridScheme { count, pin: None }];
    for pin in [PinSide::Left, PinSide::Right, PinSide::Top, PinSide::Bottom] {
        list.push(GridScheme { count, pin: Some(pin) });
    }
    list
}

#[test]
fn каждая_схема_даёт_ровно_n_окон() {
    for count in 1..=36 {
        let id = format!("vks_grid_{count}");
        assert_eq!(grids::geometry(&id, W, H).expect("раскладка существует").len(), count, "{id}");
        for scheme in variants(count) {
            let slots = grids::geometry_for(scheme, W, H);
            assert_eq!(slots.len(), count, "{}", scheme.id());
        }
    }
}

#[test]
fn окна_ровно_покрывают_канвас_без_щелей_и_пересечений() {
    for count in 1..=36 {
        for scheme in variants(count) {
            let id = scheme.id();
            let slots = grids::geometry(&id, W, H).expect("раскладка существует");
            let area: f64 = slots.iter().map(|slot| slot.rect.width * slot.rect.height).sum();
            assert!((area - W * H).abs() < 2.0, "{id}: площадь {area} != {}", W * H);
            let (mut left, mut top, mut right, mut bottom) = (f64::MAX, f64::MAX, 0.0_f64, 0.0_f64);
            for slot in &slots {
                let rect = &slot.rect;
                for value in [rect.x, rect.y, rect.width, rect.height] {
                    assert!(value.is_finite(), "{id}: неконечные координаты");
                }
                assert!(rect.x >= 0.0 && rect.y >= 0.0, "{id}: окно вне канваса");
                assert!(
                    rect.x + rect.width <= W + 0.5 && rect.y + rect.height <= H + 0.5,
                    "{id}: окно выходит за канвас"
                );
                left = left.min(rect.x);
                top = top.min(rect.y);
                right = right.max(rect.x + rect.width);
                bottom = bottom.max(rect.y + rect.height);
            }
            assert!((left - 0.0).abs() < 0.5 && (top - 0.0).abs() < 0.5, "{id}: левый/верхний край {left}/{top}");
            assert!((right - W).abs() < 0.5 && (bottom - H).abs() < 0.5, "{id}: правый/нижний край {right}/{bottom}");
            for (index, first) in slots.iter().enumerate() {
                for second in slots.iter().skip(index + 1) {
                    let overlap_x = (first.rect.x + first.rect.width).min(second.rect.x + second.rect.width)
                        - first.rect.x.max(second.rect.x);
                    let overlap_y = (first.rect.y + first.rect.height).min(second.rect.y + second.rect.height)
                        - first.rect.y.max(second.rect.y);
                    assert!(overlap_x <= 0.0 || overlap_y <= 0.0, "{id}: окна пересекаются");
                }
            }
        }
    }
}

#[test]
fn идеальные_галереи_строго_16_9() {
    for count in [4usize, 9, 16, 25, 36] {
        let id = format!("vks_grid_{count}");
        for slot in grids::geometry(&id, W, H).expect("раскладка существует") {
            let ratio = slot.rect.width / slot.rect.height;
            assert!((ratio - 16.0 / 9.0).abs() < 0.01, "{id}: ячейка {}×{}", slot.rect.width, slot.rect.height);
        }
    }
}

#[test]
fn pin_отдаёт_крупному_участнику_больше_половины_канваса() {
    for count in 2..=25 {
        for pin in [PinSide::Left, PinSide::Right, PinSide::Top, PinSide::Bottom] {
            let scheme = GridScheme { count, pin: Some(pin) };
            let slots = grids::geometry_for(scheme, W, H);
            let main_area = slots[0].rect.width * slots[0].rect.height;
            assert!(main_area > W * H / 2.0, "{}: крупное окно меньше половины канваса", scheme.id());
        }
    }
}

#[test]
fn чужие_id_не_дают_геометрии() {
    assert!(grids::geometry("codian_01", W, H).is_none());
    assert!(grids::geometry("preset_equal_9", W, H).is_none());
    assert!(grids::geometry("vks_grid_0", W, H).is_none());
    assert!(grids::geometry("vks_grid_9x9", W, H).is_none());
}
