//! Рендерер: [`Renderer`] превращает [`Layout`] + метаданные кадров в
//! список [`DrawCommand`] — план композиции, который потребляет
//! GL/wasm-рендерер (MCU compositor) или CSS/Taffy (клиент).
//!
//! Никаких пиксельных операций — только геометрия и масштабы.

use crate::core::{Layout, ParticipantId};
use crate::rect::{Canvas, Rect};
use std::collections::HashMap;

/// Метаданные кадра участника (для расчёта scale при композиции).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FrameMeta {
    /// Нативная ширина кадра (px).
    pub native_width: u32,
    /// Нативная высота кадра (px).
    pub native_height: u32,
}

/// Команда отрисовки: один участник → один rect + scale.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct DrawCommand {
    /// Участник, чей кадр отрисовывается.
    pub participant_id: ParticipantId,
    /// Целевой rect на canvas (px).
    pub rect: Rect,
    /// Масштаб (tile_size / native_size) — uniform, letterboxed.
    pub scale: f32,
}

/// Рендерер: Layout + frame meta → Vec<DrawCommand>.
#[derive(Debug, Clone, Default)]
pub struct Renderer;

impl Renderer {
    /// Собирает список [`DrawCommand`] для занятых слотов.
    ///
    /// Порядок = порядок слотов в [`Layout`] (z-порядок). Слоты без
    /// привязанного участника пропускаются. Scale = min(rect_w/native_w,
    /// rect_h/native_h) — uniform fit (letterbox); 1.0 при совпадении.
    pub fn render(
        &self,
        _canvas: &Canvas,
        layout: &Layout,
        frames: &HashMap<ParticipantId, FrameMeta>,
    ) -> Vec<DrawCommand> {
        let mut out = Vec::with_capacity(layout.slots.len());
        for slot in &layout.slots {
            let Some(pid) = slot.participant_id else {
                continue;
            };
            let Some(meta) = frames.get(&pid) else {
                continue; // no frame yet — skip (slot stays empty)
            };
            let scale = Self::fit_scale(&slot.rect, meta);
            out.push(DrawCommand { participant_id: pid, rect: slot.rect, scale });
        }
        out
    }

    /// Uniform fit-scale: min(rect_w / native_w, rect_h / native_h).
    /// 1.0 при совпадении или нулевом native (degenerate).
    fn fit_scale(rect: &Rect, meta: &FrameMeta) -> f32 {
        if rect.w == 0 || rect.h == 0 || meta.native_width == 0 || meta.native_height == 0 {
            return 1.0;
        }
        let sx = rect.w as f32 / meta.native_width as f32;
        let sy = rect.h as f32 / meta.native_height as f32;
        sx.min(sy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Layout, LayoutKind, Slot, SlotId};

    fn frame(w: u32, h: u32) -> FrameMeta {
        FrameMeta { native_width: w, native_height: h }
    }

    fn slot(pid: Option<u128>, x: u32, y: u32, w: u32, h: u32) -> Slot {
        Slot { id: SlotId::new(0), participant_id: pid.map(ParticipantId::new), rect: Rect::new(x, y, w, h) }
    }

    fn pid(i: u128) -> ParticipantId {
        ParticipantId::new(i)
    }

    #[test]
    fn render_four_person_grid() {
        let canvas = Canvas::new(1920, 1080);
        // 2x2 grid rects
        let tiles = [(0u32, 0u32), (960, 0), (0, 540), (960, 540)];
        let mut slots = Vec::new();
        for (i, (x, y)) in tiles.iter().enumerate() {
            slots.push(slot(Some(i as u128 + 1), *x, *y, 960, 540));
        }
        let layout = Layout { version: 1, kind: LayoutKind::Grid, slots };
        let mut frames = HashMap::new();
        for i in 1..=4 {
            frames.insert(pid(i), frame(1280, 720));
        }
        let cmds = Renderer.render(&canvas, &layout, &frames);
        assert_eq!(cmds.len(), 4, "4 participants → 4 DrawCommands");
        // each command has the grid rect
        for (i, c) in cmds.iter().enumerate() {
            let (x, y) = tiles[i];
            assert_eq!(c.rect, Rect::new(x, y, 960, 540));
            assert_eq!(c.participant_id, pid(i as u128 + 1));
            // scale = min(960/1280, 540/720) = min(0.75, 0.75) = 0.75
            assert!((c.scale - 0.75).abs() < 1e-4);
        }
    }

    #[test]
    fn render_skips_unassigned_and_missing_frames() {
        let canvas = Canvas::new(100, 100);
        let layout = Layout {
            version: 1,
            kind: LayoutKind::Grid,
            slots: vec![
                slot(Some(1), 0, 0, 50, 50),
                slot(None, 50, 0, 50, 50),    // unassigned
                slot(Some(2), 0, 50, 50, 50), // participant 2, no frame
                slot(Some(3), 50, 50, 50, 50),
            ],
        };
        let mut frames = HashMap::new();
        frames.insert(pid(1), frame(100, 100));
        frames.insert(pid(3), frame(100, 100));
        // no frame for participant 2
        let cmds = Renderer.render(&canvas, &layout, &frames);
        assert_eq!(cmds.len(), 2);
        let pids: Vec<ParticipantId> = cmds.iter().map(|c| c.participant_id).collect();
        assert!(pids.contains(&pid(1)));
        assert!(pids.contains(&pid(3)));
    }

    #[test]
    fn render_scale_letterbox() {
        let canvas = Canvas::new(100, 100);
        // rect 100x10, native 10x100 → sx=1.0, sy=0.1 → scale 0.1
        let layout = Layout { version: 1, kind: LayoutKind::Focus, slots: vec![slot(Some(1), 0, 0, 100, 10)] };
        let mut frames = HashMap::new();
        frames.insert(pid(1), frame(10, 100));
        let cmds = Renderer.render(&canvas, &layout, &frames);
        assert_eq!(cmds.len(), 1);
        assert!((cmds[0].scale - 0.1).abs() < 1e-4);
    }

    #[test]
    fn render_empty_layout() {
        let canvas = Canvas::new(100, 100);
        let layout = Layout::new(LayoutKind::Grid);
        let frames = HashMap::new();
        assert!(Renderer.render(&canvas, &layout, &frames).is_empty());
    }
}
