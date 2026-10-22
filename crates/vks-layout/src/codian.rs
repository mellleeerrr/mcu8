//! Раскладки Codian MCU 4200: 52 пресета из справочника + 2 кастомные
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

use crate::preset::{cell_rect, CalculatedSlot};
use crate::roles::SlotRole;

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

/// Справочник раскладок Codian: 52 пресета + 2 кастомные.
///
/// Порядок пресетов — как в спецификации (макет #1 … #52).
static CODIAN_SCHEMES: &[(&str, CodianScheme)] = &[
    (
        "codian_01",
        CodianScheme {
            title: "Полный экран", den: 1, regions: &[r(0, 0, 1, 1, 1, 1)], pips: &[], roles: &[]
        },
    ),
    ("codian_02", CodianScheme { title: "Сетка 2×2", den: 2, regions: &[r(0, 0, 2, 2, 2, 2)], pips: &[], roles: &[] }),
    ("codian_03", CodianScheme { title: "Сетка 3×3", den: 3, regions: &[r(0, 0, 3, 3, 3, 3)], pips: &[], roles: &[] }),
    ("codian_04", CodianScheme { title: "Сетка 4×4", den: 4, regions: &[r(0, 0, 4, 4, 4, 4)], pips: &[], roles: &[] }),
    (
        "codian_05",
        CodianScheme {
            title: "Угловая 1+5 (3×3)",
            den: 3,
            regions: &[r(0, 0, 2, 2, 1, 1), r(2, 0, 3, 2, 1, 2), r(0, 2, 3, 3, 3, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_06",
        CodianScheme {
            title: "Угловая 1+7 (4×4)",
            den: 4,
            regions: &[r(0, 0, 3, 3, 1, 1), r(3, 0, 4, 3, 1, 3), r(0, 3, 4, 4, 4, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_07",
        CodianScheme {
            title: "Угловая 1+12 (4×4)",
            den: 4,
            regions: &[r(0, 0, 2, 2, 1, 1), r(2, 0, 4, 2, 2, 2), r(0, 2, 4, 4, 4, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_08",
        CodianScheme {
            title: "Асимметричная 3+6 (3×3)",
            den: 24,
            regions: &[r(0, 0, 24, 6, 3, 1), r(0, 6, 24, 15, 3, 1), r(0, 15, 24, 24, 3, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_09",
        CodianScheme {
            title: "Асимметричная 4+12 (4×4)",
            den: 60,
            regions: &[r(0, 0, 60, 12, 4, 1), r(0, 12, 60, 28, 4, 1), r(0, 28, 60, 44, 4, 1), r(0, 44, 60, 60, 4, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_10",
        CodianScheme {
            title: "2+4 сверху-внизу",
            den: 12,
            regions: &[r(0, 0, 12, 8, 2, 1), r(0, 8, 12, 12, 4, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_11",
        CodianScheme {
            title: "4+2 сверху-внизу",
            den: 12,
            regions: &[r(0, 0, 12, 4, 4, 1), r(0, 4, 12, 12, 2, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_12",
        CodianScheme {
            title: "Горизонтальный сплит",
            den: 2,
            regions: &[r(0, 0, 2, 1, 1, 1), r(0, 1, 2, 2, 1, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_13",
        CodianScheme {
            title: "1+2 горизонтальный",
            den: 2,
            regions: &[r(0, 0, 2, 1, 1, 1), r(0, 1, 2, 2, 2, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_14",
        CodianScheme {
            title: "1+3 горизонтальный",
            den: 3,
            regions: &[r(0, 0, 3, 2, 1, 1), r(0, 2, 3, 3, 3, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_15",
        CodianScheme {
            title: "1+4 горизонтальный",
            den: 4,
            regions: &[r(0, 0, 4, 3, 1, 1), r(0, 3, 4, 4, 4, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_16",
        CodianScheme {
            title: "Вертикальный сплит",
            den: 2,
            regions: &[r(0, 0, 1, 2, 1, 1), r(1, 0, 2, 2, 1, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_17",
        CodianScheme {
            title: "1+2 вертикальный",
            den: 2,
            regions: &[r(0, 0, 1, 2, 1, 1), r(1, 0, 2, 2, 1, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_18",
        CodianScheme {
            title: "1+3 вертикальный",
            den: 3,
            regions: &[r(0, 0, 2, 3, 1, 1), r(2, 0, 3, 3, 1, 3)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_19",
        CodianScheme {
            title: "1+4 вертикальный",
            den: 4,
            regions: &[r(0, 0, 3, 4, 1, 1), r(3, 0, 4, 4, 1, 4)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_20",
        CodianScheme {
            title: "1+8 малые снизу",
            den: 4,
            regions: &[r(0, 0, 4, 2, 1, 1), r(0, 2, 4, 4, 4, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_21",
        CodianScheme {
            title: "8+1 малые сверху",
            den: 4,
            regions: &[r(0, 0, 4, 2, 4, 2), r(0, 2, 4, 4, 1, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_22",
        CodianScheme {
            title: "1+8 малые справа",
            den: 4,
            regions: &[r(0, 0, 2, 4, 1, 1), r(2, 0, 4, 4, 2, 4)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_23",
        CodianScheme {
            title: "8+1 малые слева",
            den: 4,
            regions: &[r(0, 0, 2, 4, 2, 4), r(2, 0, 4, 4, 1, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_24",
        CodianScheme {
            title: "4+1+4 горизонтальный центр",
            den: 4,
            regions: &[r(0, 0, 4, 1, 4, 1), r(0, 1, 4, 3, 1, 1), r(0, 3, 4, 4, 4, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_25",
        CodianScheme {
            title: "4+2+4 горизонтальный центр",
            den: 4,
            regions: &[r(0, 0, 4, 1, 4, 1), r(0, 1, 4, 3, 2, 1), r(0, 3, 4, 4, 4, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_26",
        CodianScheme {
            title: "4+1+4 вертикальный центр",
            den: 4,
            regions: &[r(0, 0, 1, 4, 1, 4), r(1, 0, 3, 4, 1, 1), r(3, 0, 4, 4, 1, 4)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_27",
        CodianScheme {
            title: "4+2+4 вертикальный центр",
            den: 4,
            regions: &[r(0, 0, 1, 4, 1, 4), r(1, 0, 3, 4, 1, 2), r(3, 0, 4, 4, 1, 4)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_28",
        CodianScheme {
            title: "2+8 сверху-внизу",
            den: 4,
            regions: &[r(0, 0, 4, 2, 2, 1), r(0, 2, 4, 4, 4, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_29",
        CodianScheme {
            title: "8+2 сверху-внизу",
            den: 4,
            regions: &[r(0, 0, 4, 2, 4, 2), r(0, 2, 4, 4, 2, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_30",
        CodianScheme {
            title: "2+8 слева-направо",
            den: 4,
            regions: &[r(0, 0, 2, 4, 1, 2), r(2, 0, 4, 4, 2, 4)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_31",
        CodianScheme {
            title: "8+2 слева-направо",
            den: 4,
            regions: &[r(0, 0, 2, 4, 2, 4), r(2, 0, 4, 4, 1, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_32",
        CodianScheme {
            title: "Рамка 1+12 (4×4)",
            den: 4,
            regions: &[
                r(0, 0, 1, 1, 1, 1),
                r(1, 0, 2, 1, 1, 1),
                r(2, 0, 3, 1, 1, 1),
                r(3, 0, 4, 1, 1, 1),
                r(0, 1, 1, 2, 1, 1),
                r(3, 1, 4, 2, 1, 1),
                r(0, 2, 1, 3, 1, 1),
                r(3, 2, 4, 3, 1, 1),
                r(0, 3, 1, 4, 1, 1),
                r(1, 3, 2, 4, 1, 1),
                r(2, 3, 3, 4, 1, 1),
                r(3, 3, 4, 4, 1, 1),
                r(1, 1, 3, 3, 1, 1),
            ],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_33",
        CodianScheme {
            title: "Наложенная PIP-рамка 1+12",
            den: 1000,
            regions: &[r(0, 0, 1000, 1000, 1, 1)],
            pips: &[
                p(31, 42, 246, 245),
                p(274, 42, 489, 245),
                p(517, 42, 732, 245),
                p(760, 42, 975, 245),
                p(31, 282, 246, 486),
                p(760, 282, 975, 486),
                p(31, 523, 246, 726),
                p(760, 523, 975, 726),
                p(31, 764, 246, 967),
                p(274, 764, 489, 967),
                p(517, 764, 732, 967),
                p(760, 764, 975, 967),
            ],
            roles: &[],
        },
    ),
    (
        "codian_34",
        CodianScheme {
            title: "Квадрант ПН 3+4",
            den: 4,
            regions: &[r(0, 0, 2, 2, 1, 1), r(2, 0, 4, 2, 1, 1), r(0, 2, 2, 4, 1, 1), r(2, 2, 4, 4, 2, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_35",
        CodianScheme {
            title: "Квадрант ЛН 3+4",
            den: 4,
            regions: &[r(0, 0, 2, 2, 1, 1), r(2, 0, 4, 2, 1, 1), r(2, 2, 4, 4, 1, 1), r(0, 2, 2, 4, 2, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_36",
        CodianScheme {
            title: "Квадрант ЛВ 3+4",
            den: 4,
            regions: &[r(2, 0, 4, 2, 1, 1), r(0, 2, 2, 4, 1, 1), r(2, 2, 4, 4, 1, 1), r(0, 0, 2, 2, 2, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_37",
        CodianScheme {
            title: "Квадрант ПВ 3+4",
            den: 4,
            regions: &[r(0, 0, 2, 2, 1, 1), r(0, 2, 2, 4, 1, 1), r(2, 2, 4, 4, 1, 1), r(2, 0, 4, 2, 2, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_38",
        CodianScheme {
            title: "PIP снизу 1+3",
            den: 1000,
            regions: &[r(0, 0, 1000, 1000, 1, 1)],
            pips: &[p(37, 726, 324, 950), p(358, 726, 645, 950), p(679, 726, 966, 950)],
            roles: &[],
        },
    ),
    (
        "codian_39",
        CodianScheme {
            title: "PIP справа 1+4",
            den: 1000,
            regions: &[r(0, 0, 1000, 1000, 1, 1)],
            pips: &[p(766, 42, 969, 245), p(766, 282, 969, 486), p(766, 523, 969, 726), p(766, 764, 969, 967)],
            roles: &[],
        },
    ),
    (
        "codian_40",
        CodianScheme {
            title: "3+10 сверху-внизу",
            den: 60,
            regions: &[r(0, 0, 60, 30, 3, 1), r(0, 30, 60, 60, 5, 2)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_41",
        CodianScheme {
            title: "5+3+5 горизонтальный центр",
            den: 60,
            regions: &[r(0, 0, 60, 15, 5, 1), r(0, 15, 60, 45, 3, 1), r(0, 45, 60, 60, 5, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_42",
        CodianScheme {
            title: "10+3 сверху-внизу",
            den: 60,
            regions: &[r(0, 0, 60, 30, 5, 2), r(0, 30, 60, 60, 3, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_43",
        CodianScheme {
            title: "Сетка 4×5 (20 окон)",
            den: 28,
            regions: &[
                r(0, 0, 28, 5, 4, 1),
                r(0, 5, 28, 10, 4, 1),
                r(0, 10, 28, 16, 4, 1),
                r(0, 16, 28, 22, 4, 1),
                r(0, 22, 28, 28, 4, 1),
            ],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_44",
        CodianScheme {
            title: "Угловая 1+16 (4×5)",
            den: 20,
            regions: &[
                r(0, 0, 10, 8, 1, 1),
                r(10, 0, 15, 4, 1, 1),
                r(15, 0, 20, 4, 1, 1),
                r(10, 4, 15, 8, 1, 1),
                r(15, 4, 20, 8, 1, 1),
                r(0, 8, 20, 20, 4, 3),
            ],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_45",
        CodianScheme {
            title: "Верхний центр 1+16 (4×5)",
            den: 20,
            regions: &[
                r(5, 0, 15, 8, 1, 1),
                r(0, 0, 5, 4, 1, 1),
                r(15, 0, 20, 4, 1, 1),
                r(0, 4, 5, 8, 1, 1),
                r(15, 4, 20, 8, 1, 1),
                r(0, 8, 20, 20, 4, 3),
            ],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_46",
        CodianScheme {
            title: "Симметричный центр 2+10",
            den: 8,
            regions: &[
                r(0, 0, 8, 2, 4, 1),
                r(0, 2, 3, 6, 1, 1),
                r(3, 2, 4, 4, 1, 1),
                r(3, 4, 4, 6, 1, 1),
                r(4, 2, 8, 6, 1, 1),
                r(0, 6, 8, 8, 4, 1),
            ],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_47",
        CodianScheme {
            title: "Рамка 1+16 (5×5)",
            den: 5,
            regions: &[
                r(0, 0, 1, 1, 1, 1),
                r(1, 0, 2, 1, 1, 1),
                r(2, 0, 3, 1, 1, 1),
                r(3, 0, 4, 1, 1, 1),
                r(4, 0, 5, 1, 1, 1),
                r(0, 1, 1, 2, 1, 1),
                r(4, 1, 5, 2, 1, 1),
                r(0, 2, 1, 3, 1, 1),
                r(4, 2, 5, 3, 1, 1),
                r(0, 3, 1, 4, 1, 1),
                r(4, 3, 5, 4, 1, 1),
                r(0, 4, 1, 5, 1, 1),
                r(1, 4, 2, 5, 1, 1),
                r(2, 4, 3, 5, 1, 1),
                r(3, 4, 4, 5, 1, 1),
                r(4, 4, 5, 5, 1, 1),
                r(1, 1, 4, 4, 1, 1),
            ],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_48",
        CodianScheme {
            title: "Сетка 5×5 (25 окон)", den: 5, regions: &[r(0, 0, 5, 5, 5, 5)], pips: &[], roles: &[]
        },
    ),
    (
        "codian_49",
        CodianScheme {
            title: "2+12 сверху-внизу",
            den: 36,
            regions: &[r(0, 0, 36, 15, 2, 1), r(0, 15, 36, 36, 4, 3)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_50",
        CodianScheme {
            title: "1+6 нижняя полоса",
            den: 6,
            regions: &[r(0, 0, 6, 5, 1, 1), r(0, 5, 6, 6, 6, 1)],
            pips: &[],
            roles: &[],
        },
    ),
    (
        "codian_51",
        CodianScheme {
            title: "PIP укрупнённый 1+1",
            den: 1000,
            regions: &[r(0, 0, 1000, 1000, 1, 1)],
            pips: &[p(636, 618, 956, 942)],
            roles: &[],
        },
    ),
    (
        "codian_52",
        CodianScheme {
            title: "PIP малый 1+1",
            den: 1000,
            regions: &[r(0, 0, 1000, 1000, 1, 1)],
            pips: &[p(779, 768, 963, 950)],
            roles: &[],
        },
    ),
    (
        "codian_custom_22",
        CodianScheme {
            title: "codian_custom_22",
            den: 12,
            regions: &[
                r(0, 0, 5, 2, 1, 1),
                r(5, 0, 10, 2, 1, 1),
                r(10, 0, 12, 1, 1, 1),
                r(10, 1, 12, 2, 1, 1),
                r(0, 2, 12, 5, 6, 3),
            ],
            pips: &[],
            roles: &[
                SlotRole::Fixed,
                SlotRole::Active,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Default,
                SlotRole::Rolling,
                SlotRole::Default,
                SlotRole::Default,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Default,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Rolling,
            ],
        },
    ),
    (
        "codian_custom_14",
        CodianScheme {
            title: "codian_custom_14",
            den: 6,
            regions: &[r(0, 0, 3, 2, 1, 1), r(3, 0, 6, 2, 1, 1), r(0, 2, 6, 3, 6, 1), r(0, 3, 6, 4, 6, 1)],
            pips: &[],
            roles: &[
                SlotRole::Fixed,
                SlotRole::Active,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Default,
                SlotRole::Rolling,
                SlotRole::Rolling,
                SlotRole::Default,
                SlotRole::Default,
                SlotRole::Rolling,
                SlotRole::Default,
                SlotRole::Default,
                SlotRole::Default,
                SlotRole::Rolling,
            ],
        },
    ),
];

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
        scheme.regions.iter().map(|region| usize::from(region.cols) * usize::from(region.rows)).sum::<usize>()
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
