//! Контракт раскладок Codian MCU 4200.
//!
//! Оракул — спецификация `Codian_MCU4200_Video_Layouts_Specification.docx`:
//! число окон каждого макета и его топология. Геометрия чертежей вендора
//! (52 PNG из того же документа) сверена с текстом спецификации: совпадение
//! 52/52 по числу окон и по карте крупных окон.
//!
//! Проверяются инварианты:
//! * число окон совпадает со спецификацией;
//! * основные окна ровно покрывают канвас (без щелей и наложений);
//! * наложенные PIP идут последними и имеют `z_order = 1`;
//! * роли кастомных раскладок соответствуют числу окон;
//! * на эталонном холсте 1280×720 все раскладки (включая кастомные) заполняют
//!   16:9 без «сплющивания», PIP строго 16:9, исправленные раскладки совпадают
//!   с эталоном ТЗ владельца.

use vks_layout::codian;

/// Число окон каждого макета #1 … #52 (из спецификации).
const EXPECTED: [(&str, usize); 52] = [
    ("01", 1),
    ("02", 4),
    ("03", 9),
    ("04", 16),
    ("05", 6),
    ("06", 8),
    ("07", 13),
    ("08", 9),
    ("09", 16),
    ("10", 6),
    ("11", 6),
    ("12", 2),
    ("13", 3),
    ("14", 4),
    ("15", 5),
    ("16", 2),
    ("17", 3),
    ("18", 4),
    ("19", 5),
    ("20", 9),
    ("21", 9),
    ("22", 9),
    ("23", 9),
    ("24", 9),
    ("25", 10),
    ("26", 9),
    ("27", 10),
    ("28", 10),
    ("29", 10),
    ("30", 10),
    ("31", 10),
    ("32", 13),
    ("33", 13),
    ("34", 7),
    ("35", 7),
    ("36", 7),
    ("37", 7),
    ("38", 4),
    ("39", 5),
    ("40", 13),
    ("41", 13),
    ("42", 13),
    ("43", 20),
    ("44", 17),
    ("45", 17),
    ("46", 12),
    ("47", 17),
    ("48", 25),
    ("49", 14),
    ("50", 7),
    ("51", 2),
    ("52", 2),
];

const WIDTH: f64 = 1920.0;
const HEIGHT: f64 = 1080.0;

#[test]
fn справочник_содержит_52_пресета_и_2_кастомные() {
    assert_eq!(codian::codian_preset_ids().count(), 54);
    assert!(codian::is_codian_preset("codian_01"));
    assert!(codian::is_codian_preset("codian_52"));
    assert!(codian::is_codian_preset("codian_custom_22"));
    assert!(codian::is_codian_preset("codian_custom_14"));
    assert!(!codian::is_codian_preset("codian_53"));
    assert!(!codian::is_codian_preset("preset_equal_25"));
}

#[test]
fn число_окон_совпадает_со_спецификацией() {
    for (index, count) in EXPECTED {
        let id = format!("codian_{index}");
        let slots = codian::geometry(&id, WIDTH, HEIGHT).expect("пресет существует");
        assert_eq!(slots.len(), count, "{id}: окон в раскладке");
        assert_eq!(codian::slot_count(&id), Some(count), "{id}: slot_count");
    }
}

#[test]
fn основные_окна_ровно_покрывают_канвас() {
    for (index, _) in EXPECTED {
        let id = format!("codian_{index}");
        let scheme = codian::scheme(&id).expect("пресет существует");
        let slots = codian::geometry(&id, WIDTH, HEIGHT).expect("пресет существует");
        let base: Vec<_> = slots.iter().filter(|slot| slot.z_order == 0).collect();
        assert!(!base.is_empty(), "{id}: нет основных окон");
        // Сумма площадей равна площади канваса.
        let area: f64 = base.iter().map(|slot| slot.rect.width * slot.rect.height).sum();
        assert!((area - WIDTH * HEIGHT).abs() < 1.0, "{id}: сумма площадей {area} != {}", WIDTH * HEIGHT);
        // Внутри основных окон наложений нет: у каждой точки канваса ровно
        // одно окно. Проверяем попарным пересечением.
        for (i, left) in base.iter().enumerate() {
            for right in base.iter().skip(i + 1) {
                let overlap_x = (left.rect.x + left.rect.width).min(right.rect.x + right.rect.width)
                    - left.rect.x.max(right.rect.x);
                let overlap_y = (left.rect.y + left.rect.height).min(right.rect.y + right.rect.height)
                    - left.rect.y.max(right.rect.y);
                assert!(
                    overlap_x <= 0.0 || overlap_y <= 0.0,
                    "{id}: окна пересекаются ({:?} и {:?})",
                    left.rect,
                    right.rect
                );
            }
        }
        // PIP-раскладки: наложенные окна идут после основных и лежат в канвасе.
        if !scheme.pips.is_empty() {
            for slot in slots.iter().filter(|slot| slot.z_order == 1) {
                assert!(slot.rect.x >= 0.0 && slot.rect.y >= 0.0, "{id}: PIP вне канваса");
                assert!(slot.rect.x + slot.rect.width <= WIDTH + 0.5, "{id}: PIP выходит за правый край");
                assert!(slot.rect.y + slot.rect.height <= HEIGHT + 0.5, "{id}: PIP выходит за нижний край");
            }
        }
    }
}

#[test]
fn области_делятся_на_целые_ячейки() {
    for id in codian::codian_preset_ids() {
        let scheme = codian::scheme(id).expect("пресет существует");
        for region in scheme.regions {
            assert!(region.cols > 0 && region.rows > 0, "{id}: пустая область");
            assert!(region.x1 > region.x0 && region.y1 > region.y0, "{id}: вырожденная область");
            assert_eq!((region.x1 - region.x0) % region.cols, 0, "{id}: область не делится на {} колонок", region.cols);
            assert_eq!((region.y1 - region.y0) % region.rows, 0, "{id}: область не делится на {} рядов", region.rows);
        }
    }
}

#[test]
fn кастомные_раскладки_имеют_роли_по_числу_окон() {
    for (id, count) in [("codian_custom_22", 22usize), ("codian_custom_14", 14usize)] {
        let roles = codian::roles(id).expect("у раскладки есть роли");
        assert_eq!(roles.len(), count, "{id}: ролей столько же, сколько окон");
        let slots = codian::geometry(id, WIDTH, HEIGHT).expect("раскладка существует");
        assert_eq!(slots.len(), count);
    }
}

#[test]
fn неизвестный_id_не_даёт_геометрии() {
    assert!(codian::geometry("codian_99", WIDTH, HEIGHT).is_none());
    assert!(codian::geometry("preset_equal_25", WIDTH, HEIGHT).is_none());
}

/// Эталонный холст ТЗ владельца: строго 16:9, 1280×720.
const CW: f64 = 1280.0;
const CH: f64 = 720.0;

/// Окно эталона: (x, y, width, height) в пикселях 1280×720.
type Cell = (i32, i32, i32, i32);

/// Строка эталона: id, окна (основные затем PIP), число основных окон.
type GoldenRow = (&'static str, &'static [Cell], usize);

/// Геометрия слотов в пикселях эталонного холста.
fn rects(id: &str) -> Vec<(i32, i32, i32, i32)> {
    codian::geometry(id, CW, CH)
        .expect("раскладка существует")
        .into_iter()
        .map(|slot| (slot.rect.x as i32, slot.rect.y as i32, slot.rect.width as i32, slot.rect.height as i32))
        .collect()
}

/// Исправленные раскладки (ТЗ владельца): точная геометрия 16:9, без «сплющивания»
/// холста, с одинаковыми ячейками и симметричным центром.
#[test]
fn исправленные_раскладки_совпадают_с_эталоном() {
    // (id, все окна построчно: основные затем PIP, число основных окон)
    let expected: [GoldenRow; 11] = [
        (
            "codian_43",
            &[
                (0, 0, 320, 144),
                (320, 0, 320, 144),
                (640, 0, 320, 144),
                (960, 0, 320, 144),
                (0, 144, 320, 144),
                (320, 144, 320, 144),
                (640, 144, 320, 144),
                (960, 144, 320, 144),
                (0, 288, 320, 144),
                (320, 288, 320, 144),
                (640, 288, 320, 144),
                (960, 288, 320, 144),
                (0, 432, 320, 144),
                (320, 432, 320, 144),
                (640, 432, 320, 144),
                (960, 432, 320, 144),
                (0, 576, 320, 144),
                (320, 576, 320, 144),
                (640, 576, 320, 144),
                (960, 576, 320, 144),
            ],
            20,
        ),
        (
            "codian_46",
            &[
                (0, 0, 320, 180),
                (320, 0, 320, 180),
                (640, 0, 320, 180),
                (960, 0, 320, 180),
                (0, 180, 480, 360),
                (480, 180, 320, 180),
                (480, 360, 320, 180),
                (800, 180, 480, 360),
                (0, 540, 320, 180),
                (320, 540, 320, 180),
                (640, 540, 320, 180),
                (960, 540, 320, 180),
            ],
            12,
        ),
        (
            "codian_49",
            &[
                (0, 0, 640, 288),
                (640, 0, 640, 288),
                (0, 288, 320, 144),
                (320, 288, 320, 144),
                (640, 288, 320, 144),
                (960, 288, 320, 144),
                (0, 432, 320, 144),
                (320, 432, 320, 144),
                (640, 432, 320, 144),
                (960, 432, 320, 144),
                (0, 576, 320, 144),
                (320, 576, 320, 144),
                (640, 576, 320, 144),
                (960, 576, 320, 144),
            ],
            14,
        ),
        (
            "codian_50",
            &[
                (0, 0, 1280, 600),
                (0, 600, 213, 120),
                (213, 600, 213, 120),
                (426, 600, 214, 120),
                (640, 600, 213, 120),
                (853, 600, 213, 120),
                (1066, 600, 214, 120),
            ],
            7,
        ),
        (
            "codian_custom_22",
            &[
                (0, 0, 533, 288),
                (533, 0, 533, 288),
                (1066, 0, 214, 144),
                (1066, 144, 214, 144),
                (0, 288, 213, 144),
                (213, 288, 213, 144),
                (426, 288, 214, 144),
                (640, 288, 213, 144),
                (853, 288, 213, 144),
                (1066, 288, 214, 144),
                (0, 432, 213, 144),
                (213, 432, 213, 144),
                (426, 432, 214, 144),
                (640, 432, 213, 144),
                (853, 432, 213, 144),
                (1066, 432, 214, 144),
                (0, 576, 213, 144),
                (213, 576, 213, 144),
                (426, 576, 214, 144),
                (640, 576, 213, 144),
                (853, 576, 213, 144),
                (1066, 576, 214, 144),
            ],
            22,
        ),
        (
            "codian_custom_14",
            &[
                (0, 0, 640, 360),
                (640, 0, 640, 360),
                (0, 360, 213, 180),
                (213, 360, 213, 180),
                (426, 360, 214, 180),
                (640, 360, 213, 180),
                (853, 360, 213, 180),
                (1066, 360, 214, 180),
                (0, 540, 213, 180),
                (213, 540, 213, 180),
                (426, 540, 214, 180),
                (640, 540, 213, 180),
                (853, 540, 213, 180),
                (1066, 540, 214, 180),
            ],
            14,
        ),
        (
            "codian_33",
            &[
                (0, 0, 1280, 720),
                (51, 28, 256, 144),
                (358, 28, 256, 144),
                (665, 28, 256, 144),
                (972, 28, 256, 144),
                (51, 201, 256, 144),
                (972, 201, 256, 144),
                (51, 374, 256, 144),
                (972, 374, 256, 144),
                (51, 547, 256, 144),
                (358, 547, 256, 144),
                (665, 547, 256, 144),
                (972, 547, 256, 144),
            ],
            1,
        ),
        ("codian_38", &[(0, 0, 1280, 720), (79, 511, 320, 180), (480, 511, 320, 180), (880, 511, 320, 180)], 1),
        (
            "codian_39",
            &[(0, 0, 1280, 720), (972, 28, 256, 144), (972, 201, 256, 144), (972, 374, 256, 144), (972, 547, 256, 144)],
            1,
        ),
        ("codian_51", &[(0, 0, 1280, 720), (844, 475, 384, 216)], 1),
        ("codian_52", &[(0, 0, 1280, 720), (1004, 565, 224, 126)], 1),
    ];
    for (id, cells, base_count) in expected {
        let actual = rects(id);
        assert_eq!(actual.len(), cells.len(), "{id}: число окон");
        // PIP идут после основных в общем списке.
        let actual_base: Vec<_> = codian::geometry(id, CW, CH)
            .expect("раскладка существует")
            .into_iter()
            .filter(|slot| slot.z_order == 0)
            .map(|slot| (slot.rect.x as i32, slot.rect.y as i32, slot.rect.width as i32, slot.rect.height as i32))
            .collect();
        assert_eq!(actual_base.len(), base_count, "{id}: число основных окон");
        assert_eq!(actual, cells.to_vec(), "{id}: геометрия 1280×720");
    }
}

/// Любая раскладка (включая кастомные) заполняет весь холст 16:9: сумма площадей
/// основных окон равна площади канваса, границы — ровно (0, 0, 1280, 720).
#[test]
fn все_раскладки_заполняют_канвас_16_9() {
    for id in codian::codian_preset_ids() {
        let slots = codian::geometry(id, CW, CH).expect("раскладка существует");
        let base: Vec<_> = slots.iter().filter(|slot| slot.z_order == 0).collect();
        let area: f64 = base.iter().map(|slot| slot.rect.width * slot.rect.height).sum();
        assert!((area - CW * CH).abs() < 1.0, "{id}: площадь {area} != {}", CW * CH);
        let max_x = base.iter().map(|slot| slot.rect.x + slot.rect.width).fold(0.0_f64, f64::max);
        let max_y = base.iter().map(|slot| slot.rect.y + slot.rect.height).fold(0.0_f64, f64::max);
        assert!((max_x - CW).abs() < 0.5, "{id}: правый край {max_x}");
        assert!((max_y - CH).abs() < 0.5, "{id}: нижний край {max_y}");
    }
}

/// Наложенные PIP-окна — строго 16:9 (ТЗ владельца: искажённые PIP недопустимы).
#[test]
fn наложенные_pip_окна_строго_16_9() {
    let ratio = 16.0 / 9.0;
    for id in codian::codian_preset_ids() {
        let slots = codian::geometry(id, CW, CH).expect("раскладка существует");
        for slot in slots.iter().filter(|slot| slot.z_order == 1) {
            let actual = slot.rect.width / slot.rect.height;
            assert!((actual - ratio).abs() < 0.02, "{id}: PIP {}×{} ({actual:.3})", slot.rect.width, slot.rect.height);
        }
    }
}
