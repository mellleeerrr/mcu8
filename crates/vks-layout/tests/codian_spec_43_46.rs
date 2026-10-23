//! Независимый fixture геометрии макетов Codian #43 и #46.
//!
//! Источник координат — **только текст спецификации владельца**
//! (`docs/progress/evidence/codian-layouts/docx_text.txt`):
//! * #43 [199-200]: «Матрица из 4 колонок и 5 рядов: 2 верхних ряда из 8 малых
//!   ячеек (4×2) и 3 нижних ряда из 12 ячеек (4×3)» ⇒ равная сетка 4×5,
//!   окно `1280/4 × 720/5 = 320×144`;
//! * #46 [208-209]: «Сверху и снизу ряды по 4 малых окна; в среднем ярусе слева
//!   и справа 2 крупных окна, разделенных посередине вертикальной парой из 2
//!   малых окон» + «Симметр. центр» ⇒ 4 ряда: 1/4 + 1/2 + 1/4; малые окна
//!   ровно одного размера `320×180` (верх, низ, центральная пара), крупные —
//!   `480×360` (по `3/8` ширины).
//!
//! `codian_table.json` (строки 1227/1381) и чертежи вендора при составлении
//! fixture **не использовались**: обе записи таблицы содержат измеренные
//! «дрожащие» пропорции и признаны устаревшими (`SUPERSEDED.md`).
//!
//! Тест — обычный (не `#[ignore]`): если production-геометрия разойдётся с
//! текстом владельца, сборка обязана упасть и потребовать решения, а не
//! молча зафиксировать дрейф.

use std::collections::BTreeMap;

use serde::Deserialize;
use vks_layout::codian;

const FIXTURE: &str = include_str!("fixtures/codian_spec_43_46_1280x720.json");
const W: f64 = 1280.0;
const H: f64 = 720.0;

#[derive(Debug, Deserialize)]
struct Fixture {
    canvas: [f64; 2],
    source: String,
    layouts: BTreeMap<String, SpecLayout>,
}

#[derive(Debug, Deserialize)]
struct SpecLayout {
    derivation: String,
    windows: Vec<[i64; 4]>,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("fixture — валидный JSON")
}

/// Окна `codian::geometry` в пикселях эталонного холста (x, y, w, h).
fn rects(id: &str) -> Vec<[i64; 4]> {
    codian::geometry(id, W, H)
        .expect("раскладка существует")
        .into_iter()
        .map(|slot| {
            assert_eq!(slot.z_order, 0, "{id}: у макетов 43/46 нет PIP-окон");
            [slot.rect.x as i64, slot.rect.y as i64, slot.rect.width as i64, slot.rect.height as i64]
        })
        .collect()
}

#[test]
fn фикстура_описывает_только_43_и_46_и_выведена_из_текста() {
    let fixture = fixture();
    assert_eq!(fixture.canvas, [W, H], "эталонный холст — 1280×720");
    assert!(fixture.source.contains("docx_text.txt"), "источник — текст владельца");
    assert!(fixture.source.contains("НЕ использованы"), "таблица/чертежи не источник fixture");
    assert_eq!(fixture.layouts.keys().map(String::as_str).collect::<Vec<_>>(), ["codian_43", "codian_46"]);
    assert!(fixture.layouts["codian_43"].derivation.contains("4 колонки x 5 рядов"));
    assert!(fixture.layouts["codian_46"].derivation.contains("3/8"));
    assert_eq!(fixture.layouts["codian_43"].windows.len(), 20);
    assert_eq!(fixture.layouts["codian_46"].windows.len(), 12);
}

#[test]
fn fixture_содержит_равную_матрицу_43_и_симметричный_центр_46() {
    let fixture = fixture();

    let w43 = &fixture.layouts["codian_43"].windows;
    assert!(w43.iter().all(|[_, _, w, h]| *w == 320 && *h == 144), "#43: все ячейки 320×144");
    let xs: BTreeMap<i64, ()> = w43.iter().map(|[x, ..]| (*x, ())).collect();
    let ys: BTreeMap<i64, ()> = w43.iter().map(|[_, y, ..]| (*y, ())).collect();
    assert_eq!(xs.keys().copied().collect::<Vec<_>>(), [0, 320, 640, 960], "#43: 4 колонки");
    assert_eq!(ys.keys().copied().collect::<Vec<_>>(), [0, 144, 288, 432, 576], "#43: 5 рядов");

    let w46 = &fixture.layouts["codian_46"].windows;
    assert_eq!(w46.iter().filter(|[_, _, w, h]| *w == 320 && *h == 180).count(), 10, "#46: 10 малых");
    assert_eq!(w46.iter().filter(|[_, _, w, h]| *w == 480 && *h == 360).count(), 2, "#46: 2 крупных");
    // Центральная пара — ровно над/под, по центру холста, а не 160×90 как в
    // устаревшей таблице.
    assert_eq!(&w46[5..7], &[[480, 180, 320, 180], [480, 360, 320, 180]], "#46: центральная пара");
    let big: Vec<&[i64; 4]> = w46.iter().filter(|[_, _, w, h]| *w == 480 && *h == 360).collect();
    assert_eq!(big, [&[0, 180, 480, 360], &[800, 180, 480, 360]], "#46: крупные симметричны");
}

#[test]
fn геометрия_43_46_совпадает_с_независимым_fixture() {
    let fixture = fixture();
    for (id, layout) in &fixture.layouts {
        let actual = rects(id);
        assert_eq!(actual.len(), layout.windows.len(), "{id}: число окон");
        assert_eq!(actual, layout.windows, "{id}: координаты 1280×720");
        for [x, y, w, h] in actual {
            assert!(x >= 0 && y >= 0 && w > 0 && h > 0, "{id}: пустое/внехолстовое окно");
            assert!(x + w <= W as i64 && y + h <= H as i64, "{id}: окно выходит за холст");
        }
    }
}

#[test]
fn основные_окна_43_46_покрывают_холст_без_пересечений() {
    let fixture = fixture();
    for id in fixture.layouts.keys() {
        let slots = codian::geometry(id, W, H).expect("раскладка существует");
        let area: f64 = slots.iter().map(|slot| slot.rect.width * slot.rect.height).sum();
        assert!((area - W * H).abs() < 0.5, "{id}: площадь {area} != {}", W * H);
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

#[test]
fn фикстура_масштабируется_на_1920x1080() {
    let fixture = fixture();
    for (id, layout) in &fixture.layouts {
        let scaled = codian::geometry(id, 1920.0, 1080.0).expect("раскладка существует");
        assert_eq!(scaled.len(), layout.windows.len(), "{id}: число окон при масштабе");
        for (slot, [x, y, w, h]) in scaled.iter().zip(&layout.windows) {
            // floor-пикселизация: каждый край отличается не более чем на 1 px.
            assert!((slot.rect.x - *x as f64 * 1.5).abs() <= 1.0, "{id}: x масштабируется");
            assert!((slot.rect.y - *y as f64 * 1.5).abs() <= 1.0, "{id}: y масштабируется");
            assert!((slot.rect.width - *w as f64 * 1.5).abs() <= 1.0, "{id}: ширина масштабируется");
            assert!((slot.rect.height - *h as f64 * 1.5).abs() <= 1.0, "{id}: высота масштабируется");
        }
    }
}
