#!/usr/bin/env python3
"""Сборка `crates/vks-layout/src/codian.rs` целиком: заголовок + таблица + логика.

ВНИМАНИЕ (T11, 2026-10-24): скрипт исторический и перезаписывает production
целиком. `gen_codian.py` не содержит R4-решений для 43/46/49 и R4-правок PIP
33/38/39/51/52, поэтому прямой запуск откатит их. Для 43/46/49 источник истины —
`SUPERSEDED.md` и `crates/vks-layout/tests/fixtures/codian_spec_43_46_1280x720.json`.
Перед запуском — сверить diff с текущим `codian.rs`; без сверки не запускать.
"""

import importlib.util
import os

os.chdir("/home/pro/Рабочий стол/mcu")
spec = importlib.util.spec_from_file_location("e", "docs/progress/evidence/codian-layouts/emit_rust.py")
E = importlib.util.module_from_spec(spec)
spec.loader.exec_module(E)
spec2 = importlib.util.spec_from_file_location("g", "docs/progress/evidence/codian-layouts/gen_codian.py")
G = importlib.util.module_from_spec(spec2)
spec2.loader.exec_module(G)

HEADER = '''//! Раскладки Codian MCU 4200: 52 пресета из справочника + 2 кастомные
//! комбинированные раскладки (22 и 14 окон).
//!
//! Источник истины — `Codian_MCU4200_Video_Layouts_Specification.docx`: поле
//! «Геометрия: …» каждого макета. Топология независимо сверена с 52
//! чертежами вендора из того же документа — совпадение 52/52 по числу окон и
//! по карте крупных окон.
//!
//! Модель данных: раскладка — набор **областей** на модульной сетке `den`
//! (1/`den` канваса). Область делится на `cols × rows` равных окон. Наложенные
//! окна PIP заданы отдельным списком и рисуются поверх (z = 1).
//!
//! Пиксельная арифметика — та же, что у вендорных пресетов
//! (`preset::cell_rect`: `floor(coord * canvas / grid)`), поэтому обе ветки
//! дают бесшовную сетку без щелей и наложений.

use crate::preset::{CalculatedSlot, cell_rect};
use crate::roles::SlotRole;
use serde::{Deserialize, Serialize};

/// Область раскладки на модульной сетке: делится на `cols × rows` равных окон.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    /// Левый край области (в единицах модульной сетки).
    pub x0: u16,
    /// Верхний край области.
    pub y0: u16,
    /// Правый край области.
    pub x1: u16,
    /// Нижний край области.
    pub y1: u16,
    /// Число окон по горизонтали внутри области.
    pub cols: u16,
    /// Число окон по вертикали внутри области.
    pub rows: u16,
}

/// Наложенное окно PIP на модульной сетке (рисуется поверх основных окон).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pip {
    /// Левый край.
    pub x0: u16,
    /// Верхний край.
    pub y0: u16,
    /// Правый край.
    pub x1: u16,
    /// Нижний край.
    pub y1: u16,
}

const fn r(x0: u16, y0: u16, x1: u16, y1: u16, cols: u16, rows: u16) -> Region {
    Region { x0, y0, x1, y1, cols, rows }
}

const fn p(x0: u16, y0: u16, x1: u16, y1: u16) -> Pip {
    Pip { x0, y0, x1, y1 }
}

/// Описание раскладки Codian.
#[derive(Debug, Clone, Copy)]
pub struct CodianScheme {
    /// Человекочитаемое название (из спецификации).
    pub title: &'static str,
    /// Знаменатель модульной сетки: 1/`den` ширины и высоты канваса.
    pub den: u16,
    /// Области основной (не наложенной) части раскладки.
    pub regions: &'static [Region],
    /// Наложенные окна PIP.
    pub pips: &'static [Pip],
    /// Роли окон (только для кастомных раскладок; порядок = порядок окон).
    pub roles: &'static [SlotRole],
}

'''

FOOTER = '''
/// true, если id — пресет Codian (`codian_01`…`codian_52`, `codian_custom_*`).
pub fn is_codian_preset(id: &str) -> bool {
    CODIAN_SCHEMES.iter().any(|(preset, _)| *preset == id)
}

/// Все id пресетов Codian (в порядке спецификации).
pub fn codian_preset_ids() -> impl Iterator<Item = &'static str> {
    CODIAN_SCHEMES.iter().map(|(preset, _)| *preset)
}

/// Описание пресета Codian по id (`None` для неизвестного id).
pub fn scheme(id: &str) -> Option<&'static CodianScheme> {
    CODIAN_SCHEMES.iter().find(|(preset, _)| *preset == id).map(|(_, scheme)| scheme)
}

/// Число окон раскладки Codian (включая наложенные PIP).
pub fn slot_count(id: &str) -> Option<usize> {
    let scheme = scheme(id)?;
    Some(
        scheme
            .regions
            .iter()
            .map(|region| usize::from(region.cols) * usize::from(region.rows))
            .sum::<usize>()
            + scheme.pips.len(),
    )
}

/// Роли окон раскладки (пустой срез для пресетов без ролей).
pub fn roles(id: &str) -> Option<&'static [SlotRole]> {
    scheme(id).map(|scheme| scheme.roles)
}

/// Геометрия слотов пресета Codian в пикселях канваса.
///
/// Области разворачиваются в окна построчно, наложенные PIP идут последними
/// с `z_order = 1`. `None` — для неизвестного id.
pub fn geometry(id: &str, width: f64, height: f64) -> Option<Vec<CalculatedSlot>> {
    let scheme = scheme(id)?;
    let width = if width.is_finite() && width > 0.0 { width } else { 1280.0 };
    let height = if height.is_finite() && height > 0.0 { height } else { 720.0 };
    let den = scheme.den;
    let mut slots = Vec::new();
    for region in scheme.regions {
        let cell_width = (region.x1 - region.x0) / region.cols;
        let cell_height = (region.y1 - region.y0) / region.rows;
        for row in 0..region.rows {
            for col in 0..region.cols {
                let x = region.x0 + col * cell_width;
                let y = region.y0 + row * cell_height;
                slots.push(CalculatedSlot {
                    rect: cell_rect((x, y, cell_width, cell_height), den, den, (width, height)),
                    z_order: 0,
                });
            }
        }
    }
    for pip in scheme.pips {
        slots.push(CalculatedSlot {
            rect: cell_rect((pip.x0, pip.y0, pip.x1 - pip.x0, pip.y1 - pip.y0), den, den, (width, height)),
            z_order: 1,
        });
    }
    Some(slots)
}
'''


def rows():
    out = []
    for num in sorted(G.EXPECTED):
        regions = G.LAYOUTS.get(num) or [G.R(0, 0, 1, 1)]
        pips = G.OVERLAY.get(num, [])
        den = G.den_for(regions, pips)
        regs = E.region_tuples(regions, den)
        pips_d = E.pip_tuples(pips, den)
        out.append(f'    ("codian_{num:02}", CodianScheme {{ title: "{E.NAMES[num]}", den: {den}, regions: &[')
        for x in regs:
            out.append(f"        r({x[0]}, {x[1]}, {x[2]}, {x[3]}, {x[4]}, {x[5]}),")
        out.append("    ], pips: &[")
        for x in pips_d:
            out.append(f"        p({x[0]}, {x[1]}, {x[2]}, {x[3]}),")
        out.append("    ], roles: &[] }),")
    for cid, sp in E.CUSTOM.items():
        dx, _dy = sp["den"]
        out.append(f'    ("{cid}", CodianScheme {{ title: "{cid}", den: {dx}, regions: &[')
        for x in sp["regions"]:
            out.append(f"        r({x[0]}, {x[1]}, {x[2]}, {x[3]}, {x[4]}, {x[5]}),")
        out.append("    ], pips: &[")
        out.append("    ], roles: &[")
        for role in sp["roles"]:
            out.append(f"        SlotRole::{role},")
        out.append("    ] }),")
    return out


if __name__ == "__main__":
    table = (
        "/// Справочник раскладок Codian: 52 пресета + 2 кастомные.\n"
        "///\n"
        "/// Порядок пресетов — как в спецификации (макет #1 … #52).\n"
        "static CODIAN_SCHEMES: &[(&str, CodianScheme)] = &[\n" + "\n".join(rows()) + "\n];\n"
    )
    open("crates/vks-layout/src/codian.rs", "w").write(HEADER + table + FOOTER)
    print("записан crates/vks-layout/src/codian.rs")
