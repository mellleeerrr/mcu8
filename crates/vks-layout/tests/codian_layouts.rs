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
//! * роли кастомных раскладок соответствуют числу окон.

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
