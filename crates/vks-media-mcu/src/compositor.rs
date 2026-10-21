//! Compositor: N input frames + per-slot [`PixelRect`]s → a single
//! 1920×1080 [`CompositeFrame`].
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): N видео → canvas 1920×1080. The default
//! implementation is a deterministic nearest-neighbour scaler with
//! letterboxing (uniform fit, centered) into each rect region; the real
//! bilinear (libyuv) compositor sits behind the `oxideav` feature
//! (ADR-0004). Frame rate is fixed at 30 fps (drop/repeat, no
//! interpolation) — see the engine.
//!
//! No `unsafe`.

use std::collections::BTreeMap;

use crate::frame::{CompositeFrame, Frame, CANVAS_HEIGHT, CANVAS_WIDTH};
use crate::layout_bridge::PixelRect;

/// Canvas size (1920×1080) the MCU composites onto.
pub const CANVAS_SIZE: (u32, u32) = (CANVAS_WIDTH, CANVAS_HEIGHT);

/// Compositor.
#[derive(Debug, Clone, Copy, Default)]
pub struct Compositor;

/// Максимальное число слотов, сортируемых на стеке без аллокации.
const STACK_SORT_SLOTS: usize = 128;

impl Compositor {
    /// Composes `frames` (slot key → frame) into a canvas using `rects`
    /// (slot key → target rect).
    ///
    /// - Each frame is scaled to its rect with a uniform scale
    ///   (letterbox) and drawn centered inside the rect; the rest of the
    ///   rect stays black.
    /// - Slots with no frame are left black.
    /// - Rects are clamped to the canvas.
    /// - Overlapping rects: higher `z` wins; equal `z` falls back to slot key
    ///   (deterministic order).
    pub fn compose(&self, frames: &BTreeMap<u32, Frame>, rects: &BTreeMap<u32, PixelRect>) -> CompositeFrame {
        let mut canvas = CompositeFrame::black(CANVAS_WIDTH, CANVAS_HEIGHT);
        let pixels = CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize;
        if frames.values().any(|frame| frame.is_yuv420p()) {
            canvas.data.fill(16);
            canvas.data.resize(pixels * 3 / 2, 128);
        }
        // Overlap/PiP: z_index задаёт порядок отрисовки, ключ — стабильный
        // tie-break. BTreeMap сам по себе сортировал только по slot id.
        // До 128 слотов сортируем на стеке: compose вызывается 30 раз/с и не
        // должен аллоцировать на горячем пути.
        let mut stack_keys = [0u32; STACK_SORT_SLOTS];
        if rects.len() <= stack_keys.len() {
            let n = rects.len();
            for (slot, key) in stack_keys.iter_mut().zip(rects.keys()) {
                *slot = *key;
            }
            stack_keys[..n].sort_unstable_by_key(|key| (rects[key].z, *key));
            for key in &stack_keys[..n] {
                if let Some(frame) = frames.get(key) {
                    Self::blit(&mut canvas, frame, &rects[key]);
                }
            }
        } else {
            let mut ordered: Vec<(&u32, &PixelRect)> = rects.iter().collect();
            ordered.sort_unstable_by_key(|(key, rect)| (rect.z, **key));
            for (key, rect) in ordered {
                if let Some(frame) = frames.get(key) {
                    Self::blit(&mut canvas, frame, rect);
                }
            }
        }
        canvas
    }

    /// Scales `frame` into `rect` (uniform fit, centered) in the canvas.
    fn blit(canvas: &mut CompositeFrame, frame: &Frame, rect: &PixelRect) {
        let (cw, ch) = (CANVAS_WIDTH as usize, CANVAS_HEIGHT as usize);
        // Visible region = intersection of `rect` with the canvas [0..W]×[0..H].
        let rx = rect.x as usize;
        let ry = rect.y as usize;
        let right = rect.x.saturating_add(rect.w) as usize;
        let bottom = rect.y.saturating_add(rect.h) as usize;
        let rx = rx.min(cw);
        let ry = ry.min(ch);
        let right = right.min(cw);
        let bottom = bottom.min(ch);
        let rw = right.saturating_sub(rx);
        let rh = bottom.saturating_sub(ry);
        if rw == 0 || rh == 0 || frame.width == 0 || frame.height == 0 {
            return;
        }
        // Uniform (letterbox) scale, matching vks_layout::Renderer.
        let sx = rw as f64 / frame.width as f64;
        let sy = rh as f64 / frame.height as f64;
        let s = sx.min(sy);
        let dw = (frame.width as f64 * s).round() as usize;
        let dh = (frame.height as f64 * s).round() as usize;
        if dw == 0 || dh == 0 {
            return;
        }
        // Center the fitted image inside the rect (usize math, clamped to
        // the canvas).
        let ox = rx + rw.saturating_sub(dw) / 2;
        let oy = ry + rh.saturating_sub(dh) / 2;
        let fw = frame.width as usize;
        let fh = frame.height as usize;
        for dy in 0..dh {
            let fy = (dy as f64 / s) as usize; // nearest neighbour, clamped
            let fy = fy.min(fh - 1);
            let row = oy + dy;
            if row >= ch {
                break;
            }
            for dx in 0..dw {
                let fx = (dx as f64 / s) as usize;
                let fx = fx.min(fw - 1);
                let col = ox + dx;
                if col >= cw {
                    break;
                }
                let src = fy * fw + fx;
                let val = frame.data.get(src).copied().unwrap_or(0);
                let dst = row * cw + col;
                if dst < canvas.data.len() {
                    canvas.data[dst] = val;
                }
            }
        }
        if canvas.data.len() == cw * ch * 3 / 2 {
            let source_pixels = fw * fh;
            let chroma_width = fw / 2;
            let chroma_height = fh / 2;
            for plane in 0..2 {
                let source_offset = source_pixels + plane * (source_pixels / 4);
                let destination_offset = cw * ch + plane * (cw * ch / 4);
                for dy in 0..dh / 2 {
                    let row = oy / 2 + dy;
                    if row >= ch / 2 {
                        break;
                    }
                    for dx in 0..dw / 2 {
                        let column = ox / 2 + dx;
                        if column >= cw / 2 {
                            break;
                        }
                        let value = if frame.is_yuv420p() {
                            let source_x = ((dx as f64 / s) as usize).min(chroma_width - 1);
                            let source_y = ((dy as f64 / s) as usize).min(chroma_height - 1);
                            frame.data[source_offset + source_y * chroma_width + source_x]
                        } else {
                            128
                        };
                        canvas.data[destination_offset + row * (cw / 2) + column] = value;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i420_color_survives_full_canvas_composition() {
        let pixels = 320 * 180;
        let mut red = vec![76; pixels];
        red.extend(vec![85; pixels / 4]);
        red.extend(vec![255; pixels / 4]);
        let frames = BTreeMap::from([(0, Frame::new(320, 180, red))]);
        let rects = BTreeMap::from([(0, PixelRect::new(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT))]);
        let result = Compositor.compose(&frames, &rects);
        let output_pixels = CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize;
        assert_eq!(result.data.len(), output_pixels * 3 / 2, "I420 chroma planes were discarded");
        assert!(result.data[..output_pixels].iter().all(|v| *v == 76));
        assert!(result.data[output_pixels..output_pixels * 5 / 4].iter().all(|v| *v == 85));
        assert!(result.data[output_pixels * 5 / 4..].iter().all(|v| *v == 255));
    }

    fn slot_rects(n: u32) -> (BTreeMap<u32, Frame>, BTreeMap<u32, PixelRect>) {
        // n distinct frames + n distinct 960×540 tiles in a 2×2 grid.
        let tiles = [(0u32, 0u32), (960, 0), (0, 540), (960, 540)];
        let mut frames = BTreeMap::new();
        let mut rects = BTreeMap::new();
        for i in 0..n.min(4) {
            let (x, y) = tiles[i as usize];
            frames.insert(i, Frame::filled(320, 180, (10 * (i + 1)) as u8));
            rects.insert(i, PixelRect::new(x, y, 960, 540));
        }
        (frames, rects)
    }

    #[test]
    fn two_frames_land_in_their_tiles() {
        let (frames, rects) = slot_rects(2);
        let out = Compositor.compose(&frames, &rects);
        assert_eq!((out.width, out.height), (CANVAS_WIDTH, CANVAS_HEIGHT));
        // Each frame occupies exactly its 960×540 tile, fully filled.
        let tile_pixels = 960 * 540;
        for (i, &val) in [10u8, 20u8].iter().enumerate() {
            let (tx, ty) = if i == 0 { (0u32, 0u32) } else { (960, 0) };
            let mut all_match = true;
            for py in 0..540u32 {
                for px in 0..960u32 {
                    let idx = ((ty + py) as usize) * CANVAS_WIDTH as usize + (tx + px) as usize;
                    if out.data[idx] != val {
                        all_match = false;
                    }
                }
            }
            assert!(all_match, "tile {i} must be uniformly filled with {val}");
            assert_eq!(tile_pixels, 960 * 540);
        }
        // Unassigned tiles (bottom row) stay black.
        for py in 540u32..1080 {
            for px in 0u32..1920 {
                assert_eq!(out.data[py as usize * 1920 + px as usize], 0, "unassigned tile stays black");
            }
        }
    }

    #[test]
    fn single_frame_fills_full_canvas() {
        let mut frames = BTreeMap::new();
        frames.insert(0u32, Frame::filled(1920, 1080, 42));
        let mut rects = BTreeMap::new();
        rects.insert(0, PixelRect::new(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT));
        let out = Compositor.compose(&frames, &rects);
        assert_eq!(out.data, vec![42u8; CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize]);
    }

    #[test]
    fn zero_frames_black_canvas() {
        let out = Compositor.compose(&BTreeMap::new(), &BTreeMap::new());
        assert_eq!(out.data, vec![0u8; CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize], "no frames → black canvas");
    }

    #[test]
    fn missing_frame_slot_stays_black() {
        let mut frames = BTreeMap::new();
        frames.insert(0u32, Frame::filled(320, 180, 7));
        let mut rects = BTreeMap::new();
        rects.insert(0, PixelRect::new(0, 0, 960, 540));
        rects.insert(1, PixelRect::new(960, 0, 960, 540)); // no frame
        let out = Compositor.compose(&frames, &rects);
        // slot 1 region (top-right) must be black.
        assert_eq!(out.data[960 + 100], 0);
        assert_eq!(out.data[500 * 1920 + 960 + 100], 0);
        // slot 0 region is drawn.
        assert_eq!(out.data[100], 7);
    }

    #[test]
    fn higher_z_index_is_drawn_on_top_for_overlap_pip() {
        let frames = BTreeMap::from([(0, Frame::filled(320, 180, 10)), (1, Frame::filled(320, 180, 20))]);
        let rects = BTreeMap::from([
            (0, PixelRect::new(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT).with_z(0)),
            (1, PixelRect::new(0, 0, 960, 540).with_z(1)),
        ]);
        let out = Compositor.compose(&frames, &rects);
        assert_eq!(out.data[100], 20, "PiP window (z=1) paints over the large zone");
        assert_eq!(out.data[600 * 1920 + 100], 10, "large zone (z=0) remains outside the PiP");
    }

    #[test]
    fn letterbox_keeps_aspect() {
        // 4:3 frame (320×240) in a 16:9 tile (960×540):
        // sx = 960/320 = 3.0, sy = 540/240 = 2.25 → s = 2.25 →
        // dw = 720, dh = 540 → centered horizontally: ox = 120.
        let mut frames = BTreeMap::new();
        frames.insert(0u32, Frame::filled(320, 240, 5));
        let mut rects = BTreeMap::new();
        rects.insert(0, PixelRect::new(0, 0, 960, 540));
        let out = Compositor.compose(&frames, &rects);
        // The image spans x in [120, 840) — outside is black.
        assert_eq!(out.data[100], 0, "left letterbox bar");
        assert_eq!(out.data[900], 0, "right letterbox bar");
        assert_eq!(out.data[400], 5, "image center");
        assert_eq!(out.data[539 * 1920 + 400], 5, "image bottom edge");
    }
}
