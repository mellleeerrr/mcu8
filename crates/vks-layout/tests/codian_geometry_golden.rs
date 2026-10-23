//! Золотой файл геометрии всех 54 раскладок Codian (`fixtures/codian_geometry_1280x720.json`).
//!
//! Зачем: инварианты «покрытие/без пересечений» ловят не всё — сдвиг границы
//! яруса на 12 px или перенос крупного блока проходят их незамеченными
//! (поймано код-ревью тестов R4). Точные координаты фиксируют топологию:
//! любое изменение геометрии требует осознанного обновления эталона.
//!
//! Формат: `{ "<id>": [[x, y, width, height, z_order], …], … }`, холст 1280×720,
//! порядок окон = порядок `codian::geometry` (позиции 1-based по индексу).

use std::collections::BTreeMap;

use vks_layout::codian;

const GOLDEN: &str = include_str!("fixtures/codian_geometry_1280x720.json");
const W: f64 = 1280.0;
const H: f64 = 720.0;

type Golden = BTreeMap<String, Vec<[i64; 5]>>;

fn golden() -> Golden {
    serde_json::from_str(GOLDEN).expect("золотой файл — валидный JSON")
}

#[test]
fn геометрия_всех_54_раскладок_совпадает_с_эталоном() {
    let expected = golden();
    assert_eq!(expected.len(), 54, "в эталоне 54 раскладки");
    for id in codian::codian_preset_ids() {
        let cells = expected.get(id).unwrap_or_else(|| panic!("{id}: нет в эталоне"));
        let slots = codian::geometry(id, W, H).expect("раскладка существует");
        assert_eq!(slots.len(), cells.len(), "{id}: число окон");
        let actual: Vec<[i64; 5]> = slots
            .iter()
            .map(|slot| {
                [
                    slot.rect.x as i64,
                    slot.rect.y as i64,
                    slot.rect.width as i64,
                    slot.rect.height as i64,
                    i64::from(slot.z_order),
                ]
            })
            .collect();
        assert_eq!(&actual, cells, "{id}: геометрия 1280×720");
    }
}

#[test]
fn эталон_содержит_все_id_и_непустые_окна() {
    let expected = golden();
    for id in codian::codian_preset_ids() {
        let cells = expected.get(id).expect("id в эталоне");
        assert!(!cells.is_empty(), "{id}: пустой эталон");
        for cell in cells {
            assert!(cell[2] > 0 && cell[3] > 0, "{id}: пустое окно {cell:?}");
            assert!(cell[4] == 0 || cell[4] == 1, "{id}: z_order вне 0/1");
        }
    }
}

#[test]
fn геометрия_масштабируется_пропорционально() {
    for id in codian::codian_preset_ids() {
        let base = codian::geometry(id, W, H).expect("раскладка существует");
        let scaled = codian::geometry(id, 1920.0, 1080.0).expect("раскладка существует");
        assert_eq!(base.len(), scaled.len(), "{id}: число окон при масштабе");
        for (small, big) in base.iter().zip(scaled.iter()) {
            // floor-пикселизация допускает расхождение до 1 px на край.
            let expected_x = small.rect.x * 1.5;
            assert!((big.rect.x - expected_x).abs() <= 1.0, "{id}: x масштабируется");
            let expected_width = small.rect.width * 1.5;
            assert!((big.rect.width - expected_width).abs() <= 1.0, "{id}: ширина масштабируется");
        }
    }
}

#[test]
fn битые_размеры_дают_эталонный_холст() {
    // Fallback геометрии: NaN/Infinity/0/отрицательные размеры → 1280×720.
    for id in ["codian_01", "codian_33", "codian_custom_22"] {
        for (width, height) in [(f64::NAN, f64::NAN), (f64::INFINITY, f64::NEG_INFINITY), (0.0, -5.0)] {
            let slots = codian::geometry(id, width, height).expect("раскладка существует");
            let max_x = slots.iter().map(|slot| slot.rect.x + slot.rect.width).fold(0.0_f64, f64::max);
            let max_y = slots.iter().map(|slot| slot.rect.y + slot.rect.height).fold(0.0_f64, f64::max);
            assert!((max_x - W).abs() < 0.5 && (max_y - H).abs() < 0.5, "{id}: холст после fallback");
        }
    }
}
