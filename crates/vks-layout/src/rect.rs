//! Пиксельная геометрия: [`Rect`] и [`Canvas`].
//!
//! Все раскладки в VKS выражаются в **пикселях** (u32) на канвасе
//! заданного размера. Это единая «общая валюта» для двух рендереров
//! (MCU compositor и клиент): клиент масштабирует rect в проценты от
//! canvas-size, MCU использует pиксели как есть.
//!
//! Инварианты (LAYOUT_ENGINE.md §11, в координатах canvas):
//! - rect внутри canvas;
//! - в grid-раскладках rects не пересекаются и покрывают canvas целиком.

use serde::{Deserialize, Serialize};

/// Канвас — размер области, на которую накладывается раскладка.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Canvas {
    /// Ширина в пикселях.
    pub width: u32,
    /// Высота в пикселях.
    pub height: u32,
}

impl Canvas {
    /// Создаёт канвас из пары (width, height).
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Пустой (нулевой) канвас.
    pub fn zero() -> Self {
        Self { width: 0, height: 0 }
    }

    /// Полный canvas как [`Rect`].
    pub fn as_rect(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    /// Площадь canvas в пикселях.
    pub fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// True, если размер ненулевой.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Прямоугольник в пикселях: позиция `x`/`y` и размер `w`/`h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Rect {
    /// x-координата левого верхнего угла (px).
    pub x: u32,
    /// y-координата левого верхнего угла (px).
    pub y: u32,
    /// Ширина (px).
    pub w: u32,
    /// Высота (px).
    pub h: u32,
}

impl Rect {
    /// Создаёт rect из (x, y, w, h).
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// Нулевой rect в точке (x, y).
    pub fn point(x: u32, y: u32) -> Self {
        Self { x, y, w: 0, h: 0 }
    }

    /// Площадь в пикселях (u64 против overflow).
    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    /// True, если rect нулевой (w == 0 || h == 0).
    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Центр rect (floor, пиксели — целые).
    pub fn center(&self) -> (u32, u32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }

    /// True, если точка (px, py) принадлежит rect (включая левую/верхнюю
    /// границу, не включая правую/нижнюю — half-open [x, x+w) × [y, y+h)).
    pub fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && py >= self.y && px < self.x.saturating_add(self.w) && py < self.y.saturating_add(self.h)
    }

    /// True, если два rect имеют пересечение с положительной площадью.
    pub fn intersects(&self, other: &Rect) -> bool {
        let x_overlap = self.x < other.x.saturating_add(other.w) && other.x < self.x.saturating_add(self.w);
        let y_overlap = self.y < other.y.saturating_add(other.h) && other.y < self.y.saturating_add(self.h);
        x_overlap && y_overlap && !self.is_empty() && !other.is_empty()
    }

    /// Отрезает rect от canvas: результат всегда внутри canvas,
    /// размеры не превышают доступное (пересечение rect с canvas).
    pub fn clamp(&self, canvas: &Canvas) -> Rect {
        // x0,y0 = left/top of intersection; right/bottom clamped to canvas.
        let right = self.x.saturating_add(self.w).min(canvas.width);
        let bottom = self.y.saturating_add(self.h).min(canvas.height);
        let x = self.x.min(canvas.width);
        let y = self.y.min(canvas.height);
        let w = right.saturating_sub(x);
        let h = bottom.saturating_sub(y);
        Self::new(x, y, w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_area() {
        assert_eq!(Canvas::new(1920, 1080).area(), 1920 * 1080);
        assert_eq!(Canvas::zero().area(), 0);
        assert!(Canvas::zero().is_empty());
        assert!(!Canvas::new(1, 1).is_empty());
    }

    #[test]
    fn rect_area_and_empty() {
        assert_eq!(Rect::new(0, 0, 100, 50).area(), 5000);
        assert!(Rect::new(0, 0, 0, 50).is_empty());
        assert!(Rect::new(0, 0, 100, 0).is_empty());
    }

    #[test]
    fn rect_center() {
        assert_eq!(Rect::new(10, 20, 100, 50).center(), (60, 45));
        // floor for odd sizes.
        assert_eq!(Rect::new(0, 0, 1, 1).center(), (0, 0));
    }

    #[test]
    fn rect_contains_half_open() {
        let r = Rect::new(10, 10, 20, 20);
        assert!(r.contains(10, 10));
        assert!(r.contains(29, 29));
        assert!(!r.contains(30, 30)); // right/bottom exclusive
        assert!(!r.contains(9, 9));
        assert!(!r.contains(10, 30));
    }

    #[test]
    fn rect_intersects() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(5, 5, 10, 10);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
        let c = Rect::new(10, 0, 10, 10); // touching at edge — no area
        assert!(!a.intersects(&c));
        let empty = Rect::new(5, 5, 0, 10);
        assert!(!a.intersects(&empty));
    }

    #[test]
    fn rect_clamp_to_canvas() {
        let canvas = Canvas::new(100, 100);
        // partially outside on right/bottom → trimmed to canvas
        assert_eq!(Rect::new(90, 90, 30, 30).clamp(&canvas), Rect::new(90, 90, 10, 10));
        // entirely inside → unchanged
        assert_eq!(Rect::new(10, 10, 5, 5).clamp(&canvas), Rect::new(10, 10, 5, 5));
        // larger than canvas → shrunk to canvas
        assert_eq!(Rect::new(0, 0, 200, 200).clamp(&canvas), canvas.as_rect());
        // partially outside on left/top (x < 0 underflows to near u32::MAX)
        assert_eq!(Rect::new(u32::MAX, 0, 10, 10).clamp(&canvas), Rect::new(100, 0, 0, 10));
    }
}
