//! Контракт конструктора «Схема расположения участников» (`vks_grid_*`,
//! `vks_pin_*`): число окон, геометрия внутри канваса/полей, отсутствие
//! пересечений, равные ячейки галереи, 16:9 у идеальных галерей и миниатюр PIN.
//! Покрытие канваса больше не инвариант: последний неполный ряд галереи
//! центрирован (модель `meeting-grid-layout`), а зазоры/поля — намеренные
//! непокрашенные области.

use vks_layout::grids::{self, GridOptions, GridScheme, PinSide};

const W: f64 = 1280.0;
const H: f64 = 720.0;

fn variants(count: usize, options: GridOptions) -> Vec<GridScheme> {
    let mut list = vec![
        GridScheme { count, pin: None, is_dual: false, options },
        GridScheme { count, pin: None, is_dual: true, options },
    ];
    for pin in [PinSide::Left, PinSide::Right, PinSide::Top, PinSide::Bottom] {
        list.push(GridScheme { count, pin: Some(pin), is_dual: false, options });
    }
    list
}

#[test]
fn каждая_схема_даёт_ровно_n_окон() {
    for count in 1..=100 {
        let id = format!("vks_grid_{count}");
        assert_eq!(grids::geometry(&id, W, H).expect("раскладка существует").len(), count, "{id}");
        for scheme in variants(count, GridOptions::default()) {
            let slots = grids::geometry_for(scheme, W, H);
            assert_eq!(slots.len(), count, "{}", scheme.id());
        }
    }
}

/// Все окна внутри канваса, не пересекаются, координаты конечны.
#[test]
fn окна_внутри_канваса_и_не_пересекаются() {
    for count in 1..=100 {
        for options in [GridOptions::default(), GridOptions { gap: 12, margin: 24 }] {
            for scheme in variants(count, options) {
                let id = scheme.id();
                let slots = grids::geometry(&id, W, H).expect("раскладка существует");
                for slot in &slots {
                    let rect = &slot.rect;
                    for value in [rect.x, rect.y, rect.width, rect.height] {
                        assert!(value.is_finite(), "{id}: неконечные координаты");
                    }
                    assert!(rect.width > 0.0 && rect.height > 0.0, "{id}: пустое окно");
                    assert!(rect.x >= 0.0 && rect.y >= 0.0, "{id}: окно вне канваса");
                    assert!(
                        rect.x + rect.width <= W + 0.5 && rect.y + rect.height <= H + 0.5,
                        "{id}: окно выходит за канвас"
                    );
                }
                for (index, first) in slots.iter().enumerate() {
                    for second in slots.iter().skip(index + 1) {
                        let overlap_x = (first.rect.x + first.rect.width).min(second.rect.x + second.rect.width)
                            - first.rect.x.max(second.rect.x);
                        let overlap_y = (first.rect.y + first.rect.height).min(second.rect.y + second.rect.height)
                            - first.rect.y.max(second.rect.y);
                        assert!(overlap_x <= 0.5 || overlap_y <= 0.5, "{id}: окна пересекаются");
                    }
                }
            }
        }
    }
}

#[test]
fn идеальные_галереи_строго_16_9() {
    for count in [4usize, 9, 16, 25, 36, 64, 100] {
        let id = format!("vks_grid_{count}");
        for slot in grids::geometry(&id, W, H).expect("раскладка существует") {
            let ratio = slot.rect.width / slot.rect.height;
            assert!((ratio - 16.0 / 9.0).abs() < 0.02, "{id}: ячейка {}×{}", slot.rect.width, slot.rect.height);
        }
    }
}

/// Галерея — равные ячейки; последний неполный ряд не растягивается.
#[test]
fn галерея_даёт_равные_ячейки() {
    for count in 2..=100 {
        let slots = grids::geometry(&format!("vks_grid_{count}"), W, H).expect("раскладка существует");
        let widths: Vec<f64> = slots.iter().map(|slot| slot.rect.width).collect();
        let heights: Vec<f64> = slots.iter().map(|slot| slot.rect.height).collect();
        let (min_w, max_w) =
            (widths.iter().cloned().fold(f64::MAX, f64::min), widths.iter().cloned().fold(0.0, f64::max));
        let (min_h, max_h) =
            (heights.iter().cloned().fold(f64::MAX, f64::min), heights.iter().cloned().fold(0.0, f64::max));
        assert!(max_w - min_w <= 1.5, "vks_grid_{count}: разброс ширин {min_w}..{max_w}");
        assert!(max_h - min_h <= 1.5, "vks_grid_{count}: разброс высот {min_h}..{max_h}");
    }
}

/// Точные rect'ы порта библиотеки: `vks_grid_g8_m16_5` на 1920×1080.
#[test]
fn gapped_галерея_даёт_точные_пиксели() {
    let slots = grids::geometry("vks_grid_g8_m16_5", 1920.0, 1080.0).expect("раскладка существует");
    let rects: Vec<(f64, f64, f64, f64)> =
        slots.iter().map(|slot| (slot.rect.x, slot.rect.y, slot.rect.width, slot.rect.height)).collect();
    let expected = vec![
        (24.0, 24.0, 616.0, 510.0),
        (652.0, 24.0, 616.0, 510.0),
        (1280.0, 24.0, 616.0, 510.0),
        (338.0, 546.0, 616.0, 510.0),
        (966.0, 546.0, 616.0, 510.0),
    ];
    assert_eq!(rects, expected);
}

/// PIN: крупное окно больше половины канваса, миниатюры 16:9 и меньше него.
#[test]
fn pin_отдаёт_крупному_больше_половины_а_миниатюры_16_9() {
    for count in 2..=16 {
        for pin in [PinSide::Left, PinSide::Right, PinSide::Top, PinSide::Bottom] {
            let scheme = GridScheme { count, pin: Some(pin), is_dual: false, options: GridOptions::default() };
            let slots = grids::geometry_for(scheme, W, H);
            let main = &slots[0].rect;
            for thumb in &slots[1..] {
                assert!(
                    thumb.rect.width <= main.width && thumb.rect.height <= main.height,
                    "{}: миниатюра не меньше крупного",
                    scheme.id()
                );
                assert!(
                    main.width * main.height >= thumb.rect.width * thumb.rect.height * 3.5,
                    "{}: крупное окно недостаточно больше миниатюры",
                    scheme.id()
                );
            }
        }
    }
}

/// Счётчик UI доходит до 100; опции `g`/`m` по-прежнему ограничены 64.
#[test]
fn сто_окон_и_потолок_опций() {
    for pin in [None, Some(PinSide::Right)] {
        let scheme = GridScheme { count: 100, pin, is_dual: false, options: GridOptions::default() };
        let id = scheme.id();
        assert_eq!(grids::parse(&id), Some(scheme), "id на 100 окон разбирается: {id}");
        assert_eq!(grids::geometry(&id, W, H).expect("раскладка существует").len(), 100);
    }
    assert!(grids::parse("vks_grid_101").is_none());
    assert!(grids::parse("vks_grid_g64_100").is_some());
    assert!(grids::parse("vks_grid_g65_100").is_none());
    assert!(grids::parse("vks_grid_m64_100").is_some());
}

#[test]
fn чужие_id_не_дают_геометрии() {
    assert!(grids::geometry("codian_01", W, H).is_none());
    assert!(grids::geometry("preset_equal_9", W, H).is_none());
    assert!(grids::geometry("vks_grid_0", W, H).is_none());
    assert!(grids::geometry("vks_grid_9x9", W, H).is_none());
    assert!(grids::geometry("vks_grid_9_g12", W, H).is_none());
}

/// Зазоры/поля (порт meeting-grid-layout): окна внутри полей, между ними зазор,
/// пересечений нет, число окон верное.
#[test]
fn зазоры_и_поля_сохраняют_структуру() {
    for (count, gap, margin) in [(9usize, 12u32, 24u32), (4, 8, 16), (13, 24, 32), (25, 4, 8)] {
        for pin in [None, Some(PinSide::Left), Some(PinSide::Right), Some(PinSide::Top), Some(PinSide::Bottom)] {
            let scheme = GridScheme { count, pin, is_dual: false, options: GridOptions { gap, margin } };
            let id = scheme.id();
            assert_eq!(grids::parse(&id), Some(scheme), "id с опциями разбирается: {id}");
            let slots = grids::geometry(&id, W, H).expect("раскладка существует");
            assert_eq!(slots.len(), count, "{id}: число окон");
            let left = slots.iter().map(|s| s.rect.x).fold(f64::MAX, f64::min);
            let top = slots.iter().map(|s| s.rect.y).fold(f64::MAX, f64::min);
            let right = slots.iter().map(|s| s.rect.x + s.rect.width).fold(0.0_f64, f64::max);
            let bottom = slots.iter().map(|s| s.rect.y + s.rect.height).fold(0.0_f64, f64::max);
            assert!(left >= margin as f64 - 1.0, "{id}: левое поле");
            assert!(top >= margin as f64 - 1.0, "{id}: верхнее поле");
            assert!(right <= W - margin as f64 + 1.0, "{id}: правое поле");
            assert!(bottom <= H - margin as f64 + 1.0, "{id}: нижнее поле");
            for (index, first) in slots.iter().enumerate() {
                for second in slots.iter().skip(index + 1) {
                    let overlap_x = (first.rect.x + first.rect.width).min(second.rect.x + second.rect.width)
                        - first.rect.x.max(second.rect.x);
                    let overlap_y = (first.rect.y + first.rect.height).min(second.rect.y + second.rect.height)
                        - first.rect.y.max(second.rect.y);
                    assert!(overlap_x <= 0.5 || overlap_y <= 0.5, "{id}: окна пересекаются");
                }
            }
        }
    }
}

/// Граничные сочетания «много окон × большой зазор»: геометрия остаётся
/// валидной (зазор обнуляется), окна внутри холста и не пересекаются.
#[test]
fn большой_зазор_при_многих_окнах_не_ломает_геометрию() {
    for (count, gap) in [(64usize, 64u32), (36, 64), (64, 32), (64, 8), (100, 64), (100, 8)] {
        for pin in [None, Some(PinSide::Left), Some(PinSide::Top)] {
            let scheme = GridScheme { count, pin, is_dual: false, options: GridOptions { gap, margin: 0 } };
            let id = scheme.id();
            let slots = grids::geometry(&id, W, H).expect("раскладка существует");
            assert_eq!(slots.len(), count, "{id}: число окон");
            for slot in &slots {
                let rect = &slot.rect;
                assert!(rect.width >= 1.0 && rect.height >= 1.0, "{id}: пустое окно");
                assert!(
                    rect.x >= 0.0 && rect.y >= 0.0 && rect.x + rect.width <= W + 0.5 && rect.y + rect.height <= H + 0.5,
                    "{id}: окно вне холста"
                );
            }
            for (index, first) in slots.iter().enumerate() {
                for second in slots.iter().skip(index + 1) {
                    let overlap_x = (first.rect.x + first.rect.width).min(second.rect.x + second.rect.width)
                        - first.rect.x.max(second.rect.x);
                    let overlap_y = (first.rect.y + first.rect.height).min(second.rect.y + second.rect.height)
                        - first.rect.y.max(second.rect.y);
                    assert!(overlap_x <= 0.5 || overlap_y <= 0.5, "{id}: окна пересекаются");
                }
            }
        }
    }
}
