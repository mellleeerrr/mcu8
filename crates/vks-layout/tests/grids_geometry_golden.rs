//! Golden геометрии параметрических раскладок конструктора (`vks_grid_*`,
//! `vks_pin_*`) на эталонном холсте 1280×720.
//!
//! **Честно о природе эталона:** это снимок ТЕКУЩЕЙ реализации
//! (`grids::geometry`), а не независимый оракул. Так же устроен
//! `codian_geometry_golden.rs`. Смысл — зафиксировать точные пиксели перед
//! сокращением геометрии (r6-elegance): рефакторинг, меняющий границу окна
//! на 1 px или порядок окон, обязан упасть здесь и потребовать осознанного
//! обновления эталона. Независимые инварианты (число окон, границы, 16:9,
//! отсутствие пересечений) живут в `tests/grids.rs`.
//!
//! Покрытие: `vks_grid_1..9,13,16,25,100`; `vks_pin_{l,r,t,b}_{2,5,9,25}`;
//! каждая базовая схема — с опциями по умолчанию, `g8`, `g12`, `m24`, `m64`,
//! `g12_m24` (174 id). Опции в id заданы в пикселях 720p и масштабируются по
//! высоте холста.
//!
//! Воспроизведение/обновление — только вручную:
//! `python3 docs/progress/evidence/codian-layouts/gen_grids_golden.py`
//! (запускает ignored-тест `regenerate_golden`). В обычном `cargo test` эталон
//! не перезаписывается.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use vks_layout::grids::{self, GridOptions, GridScheme, PinSide};

const GOLDEN: &str = include_str!("fixtures/grids_geometry_1280x720.json");
const W: f64 = 1280.0;
const H: f64 = 720.0;

/// `{ "<id>": [[x, y, width, height, z_order], …], … }`.
type Golden = BTreeMap<String, Vec<[f64; 5]>>;

const OPTIONS: [GridOptions; 6] = [
    GridOptions { gap: 0, margin: 0 },
    GridOptions { gap: 8, margin: 0 },
    GridOptions { gap: 12, margin: 0 },
    GridOptions { gap: 0, margin: 24 },
    GridOptions { gap: 0, margin: 64 },
    GridOptions { gap: 12, margin: 24 },
];

fn golden() -> Golden {
    serde_json::from_str(GOLDEN).expect("эталон — валидный JSON")
}

/// Базовые схемы без опций: репрезентативные галереи и все стороны PIN.
fn base_schemes() -> Vec<GridScheme> {
    let mut schemes = Vec::new();
    for count in [1usize, 2, 3, 4, 5, 6, 7, 8, 9, 13, 16, 25, 100] {
        schemes.push(GridScheme { count, pin: None, is_dual: false, options: GridOptions { gap: 0, margin: 0 } });
    }
    for count in [2usize, 5, 9, 25] {
        for pin in [PinSide::Left, PinSide::Right, PinSide::Top, PinSide::Bottom] {
            schemes.push(GridScheme {
                count,
                pin: Some(pin),
                is_dual: false,
                options: GridOptions { gap: 0, margin: 0 },
            });
        }
    }
    schemes
}

/// Все id эталона: каждая базовая схема × каждая опция (`options` в id).
fn expected_ids() -> Vec<String> {
    let mut ids = Vec::new();
    for base in base_schemes() {
        for options in OPTIONS {
            ids.push(GridScheme { options, ..base }.id());
        }
    }
    ids
}

fn actual_cells(id: &str, width: f64, height: f64) -> Vec<[f64; 5]> {
    grids::geometry(id, width, height)
        .unwrap_or_else(|| panic!("{id}: раскладка существует"))
        .into_iter()
        .map(|slot| [slot.rect.x, slot.rect.y, slot.rect.width, slot.rect.height, f64::from(slot.z_order)])
        .collect()
}

#[test]
fn эталон_покрывает_ключевые_id_и_непустые_окна() {
    let golden = golden();
    let expected: BTreeSet<String> = expected_ids().into_iter().collect();
    let actual: BTreeSet<String> = golden.keys().cloned().collect();
    assert_eq!(expected.len(), 174, "решётка id: 29 базовых схем × 6 вариантов опций");
    assert_eq!(actual, expected, "эталон содержит ровно ожидаемые id (без лишних/без пропусков)");
    for id in &expected {
        let cells = &golden[id];
        assert!(!cells.is_empty(), "{id}: пустой эталон");
        assert_eq!(cells.len(), grids::slot_count(id).expect("id разбирается"), "{id}: число окон");
        for cell in cells {
            assert!(cell.iter().all(|value| value.is_finite()), "{id}: нечисловая координата {cell:?}");
            assert!(cell[2] > 0.0 && cell[3] > 0.0, "{id}: пустое окно {cell:?}");
            assert_eq!(cell[4], 0.0, "{id}: у конструктора нет PIP/z>0");
            assert!(cell[0] >= 0.0 && cell[1] >= 0.0, "{id}: окно вне холста {cell:?}");
            assert!(cell[0] + cell[2] <= W + 0.5 && cell[1] + cell[3] <= H + 0.5, "{id}: окно за холстом {cell:?}");
        }
    }
}

#[test]
fn геометрия_всех_схем_конструктора_совпадает_с_эталоном() {
    let golden = golden();
    for (id, expected) in &golden {
        let actual = actual_cells(id, W, H);
        assert_eq!(actual.len(), expected.len(), "{id}: число окон");
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            for axis in 0..5 {
                // Допуск 1e-6 px: serde_json без фичи `float_roundtrip` может
                // сдвинуть последний бит f64 при разборе; визуально это ноль.
                assert!(
                    (actual[axis] - expected[axis]).abs() <= 1e-6,
                    "{id}: окно {index}, ось {axis}: {} != {}",
                    actual[axis],
                    expected[axis]
                );
            }
        }
    }
}

#[test]
fn эталон_масштабируется_на_1920x1080() {
    let golden = golden();
    for id in golden.keys() {
        let base = actual_cells(id, W, H);
        let scaled = actual_cells(id, 1920.0, 1080.0);
        assert_eq!(scaled.len(), base.len(), "{id}: число окон при масштабе");
        for (small, big) in base.iter().zip(&scaled) {
            // Пикселизация round/floor допускает расхождение до ~2 px на край.
            for axis in 0..4 {
                assert!((big[axis] - small[axis] * 1.5).abs() <= 2.0, "{id}: ось {axis} масштабируется");
            }
            assert!(big[0] + big[2] <= 1920.0 + 0.5 && big[1] + big[3] <= 1080.0 + 0.5, "{id}: 1080p за холстом");
        }
    }
}

/// Перезапись эталона текущей реализацией. Запускается только вручную:
/// `python3 docs/progress/evidence/codian-layouts/gen_grids_golden.py`.
#[test]
#[ignore = "обновляет golden; запускать через gen_grids_golden.py"]
fn regenerate_golden() {
    let mut out = String::from("{\n");
    let ids = expected_ids();
    for (index, id) in ids.iter().enumerate() {
        let cells = actual_cells(id, W, H);
        let body = serde_json::to_string(&cells).expect("окна сериализуются");
        let comma = if index + 1 == ids.len() { "" } else { "," };
        writeln!(out, "  {:?}: {}{}", id, body, comma).expect("запись в String");
    }
    out.push_str("}\n");
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/grids_geometry_1280x720.json");
    std::fs::write(path, out).expect("эталон записывается");
}
