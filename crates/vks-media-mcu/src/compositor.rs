//! Compositor: N input frames + per-slot [`PixelRect`]s → a single
//! 1920×1080 [`Frame`].
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
use std::sync::Arc;

use crate::frame::{Frame, CANVAS_HEIGHT, CANVAS_WIDTH};
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
    pub fn compose(&self, frames: &BTreeMap<u32, Arc<Frame>>, rects: &BTreeMap<u32, PixelRect>) -> Frame {
        let pixels = CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize;
        let yuv_canvas = frames.values().any(|frame| frame.is_yuv420p());
        // Build the canvas in one allocation: a YUV canvas starts at limited
        // range (16/128), a grayscale one at black. `filled` + `resize` used to
        // touch and copy the 3 MB buffer twice per tick.
        let mut canvas = if yuv_canvas {
            let mut data = vec![128u8; pixels * 3 / 2];
            data[..pixels].fill(16);
            Frame::new(CANVAS_WIDTH, CANVAS_HEIGHT, data)
        } else {
            Frame::new(CANVAS_WIDTH, CANVAS_HEIGHT, vec![0u8; pixels])
        };
        // Overlap/PiP: z_index задаёт порядок отрисовки, ключ — стабильный
        // tie-break. BTreeMap сам по себе сортировал только по slot id.
        // До 128 слотов сортируем на стеке: сортировка не аллоцирует. Сам
        // канвас (3 МБ для YUV) выделяется один раз за вызов — его переиспользование
        // требует per-room scratch-буфера и вынесено в отдельный срез S20.
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
    ///
    /// Nearest-neighbour mapping is exact integer floor math:
    /// `fx = dx * fw / dw`, `fy = dy * fh / dh` (the previous `f64` version
    /// truncated `dx / s` and could shift sampling by one source pixel). The
    /// horizontal index is advanced incrementally so no division runs per pixel.
    fn blit(canvas: &mut Frame, frame: &Frame, rect: &PixelRect) {
        let (cw, ch) = (CANVAS_WIDTH as usize, CANVAS_HEIGHT as usize);
        // Visible region = intersection of `rect` with the canvas [0..W]×[0..H].
        let right = (rect.x.saturating_add(rect.w) as usize).min(cw);
        let bottom = (rect.y.saturating_add(rect.h) as usize).min(ch);
        let rx = (rect.x as usize).min(cw);
        let ry = (rect.y as usize).min(ch);
        let rw = right.saturating_sub(rx);
        let rh = bottom.saturating_sub(ry);
        if rw == 0 || rh == 0 || frame.width == 0 || frame.height == 0 {
            return;
        }
        let fw = frame.width as usize;
        let fh = frame.height as usize;
        // Uniform (letterbox) fit, matching vks_layout::Renderer, in integer math:
        // width-limited when rw/fw <= rh/fh, else height-limited. The free axis is
        // rounded half-up exactly like the former `f64::round`.
        let (dw, dh) = if rw * fh <= rh * fw {
            (rw, ((fh * rw + fw / 2) / fw).max(1))
        } else {
            (((fw * rh + fh / 2) / fh).max(1), rh)
        };
        if dw == 0 || dh == 0 {
            return;
        }
        let ox = rx + rw.saturating_sub(dw) / 2;
        let oy = ry + rh.saturating_sub(dh) / 2;
        let step_x = fw / dw;
        let rem_x = fw % dw;
        for dy in 0..dh {
            let fy = dy * fh / dh;
            let Some(source) = frame.data.get(fy * fw..fy * fw + fw) else { continue };
            let destination_row = (oy + dy) * cw;
            let Some(destination) = canvas.data.get_mut(destination_row + ox..destination_row + ox + dw) else {
                continue;
            };
            let mut fx = 0usize;
            let mut rem = 0usize;
            for pixel in destination {
                *pixel = source[fx];
                fx += step_x;
                rem += rem_x;
                if rem >= dw {
                    rem -= dw;
                    fx += 1;
                }
            }
        }
        if canvas.data.len() == cw * ch * 3 / 2 {
            let is_yuv = frame.is_yuv420p();
            let source_pixels = fw * fh;
            let chroma_width = fw / 2;
            let chroma_height = fh / 2;
            let chroma_dw = dw / 2;
            let chroma_dh = dh / 2;
            let chroma_step_x = chroma_width / chroma_dw.max(1);
            let chroma_rem_x = chroma_width % chroma_dw.max(1);
            for plane in 0..2 {
                let source_offset = source_pixels + plane * (source_pixels / 4);
                let destination_offset = cw * ch + plane * (cw * ch / 4);
                for dy in 0..chroma_dh {
                    let source_y = dy * chroma_height / chroma_dh.max(1);
                    let row = oy / 2 + dy;
                    let Some(destination) = canvas.data.get_mut(
                        destination_offset + row * (cw / 2) + ox / 2
                            ..destination_offset + row * (cw / 2) + ox / 2 + chroma_dw,
                    ) else {
                        continue;
                    };
                    if !is_yuv {
                        destination.fill(128);
                        continue;
                    }
                    let mut fx = 0usize;
                    let mut rem = 0usize;
                    for pixel in destination {
                        *pixel = frame.data[source_offset + source_y * chroma_width + fx];
                        fx += chroma_step_x;
                        rem += chroma_rem_x;
                        if rem >= chroma_dw.max(1) {
                            rem -= chroma_dw.max(1);
                            fx += 1;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_i420(width: usize, height: usize) -> Frame {
        // Horizontal gradient pins the exact sampled source column; a uniform
        // fill cannot detect a nearest-neighbour regression.
        let mut data = vec![0u8; width * height * 3 / 2];
        for y in 0..height {
            for x in 0..width {
                data[y * width + x] = (x % 251) as u8;
            }
        }
        let chroma = width * height / 4;
        data[width * height..width * height + chroma].fill(100);
        data[width * height + chroma..].fill(200);
        Frame::new(width as u32, height as u32, data)
    }

    /// Exact floor mapping `fx = dx * fw / dw` at a non-integer scale
    /// (320x240 into 960x540 => dw=720, height-limited, ox=120).
    #[test]
    fn nearest_neighbour_maps_exact_source_columns() {
        let frame = Arc::new(gradient_i420(320, 240));
        let frames = BTreeMap::from([(0u32, frame)]);
        let rects = BTreeMap::from([(0u32, PixelRect::new(0, 0, 960, 540))]);
        let out = Compositor.compose(&frames, &rects);
        let canvas_w = CANVAS_WIDTH as usize;
        for dx in [0usize, 1, 2, 4, 9, 100, 359, 719] {
            let expected = (dx * 320 / 720) % 251;
            assert_eq!(out.data[dx + 120], expected as u8, "dx={dx} must sample source column {}", dx * 320 / 720);
        }
        // Chroma planes use the same exact mapping on their own resolution:
        // dw/2 = 360 columns start at ox/2 = 60; U plane is 100, V plane 200.
        let chroma_offset = canvas_w * CANVAS_HEIGHT as usize;
        let chroma_plane = (canvas_w / 2) * (CANVAS_HEIGHT as usize / 2);
        for dx in [0usize, 1, 179, 359] {
            assert_eq!(out.data[chroma_offset + 60 + dx], 100, "U plane at dx={dx}");
            assert_eq!(out.data[chroma_offset + chroma_plane + 60 + dx], 200, "V plane at dx={dx}");
        }
    }

    /// Letterbox geometry stays centered and integer-exact (640x480 into 960x540).
    #[test]
    fn letterbox_geometry_is_centered() {
        let frame = Arc::new(Frame::filled(640, 480, 77));
        let frames = BTreeMap::from([(0u32, frame)]);
        let rects = BTreeMap::from([(0u32, PixelRect::new(0, 0, 960, 540))]);
        let out = Compositor.compose(&frames, &rects);
        let canvas_w = CANVAS_WIDTH as usize;
        // 640x480 at 1.125x => 720x540, centered with 120 px pillars on each side.
        assert_eq!(out.data[0], 0, "left pillar is black");
        assert_eq!(out.data[119], 0);
        assert_eq!(out.data[120], 77, "image starts at the centered offset");
        assert_eq!(out.data[120 + 719], 77);
        assert_eq!(out.data[120 + 720], 0, "right pillar is black");
        assert_eq!(out.data[(540 - 1) * canvas_w + 120], 77, "no vertical letterbox in the fitted image");
        assert_eq!(out.data[540 * canvas_w + 120], 0, "bottom band outside 540-row image stays black");
    }

    #[test]
    fn i420_color_survives_full_canvas_composition() {
        let pixels = 320 * 180;
        let mut red = vec![76; pixels];
        red.extend(vec![85; pixels / 4]);
        red.extend(vec![255; pixels / 4]);
        let frames = BTreeMap::from([(0, Arc::new(Frame::new(320, 180, red)))]);
        let rects = BTreeMap::from([(0, PixelRect::new(0, 0, CANVAS_WIDTH, CANVAS_HEIGHT))]);
        let result = Compositor.compose(&frames, &rects);
        let output_pixels = CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize;
        assert_eq!(result.data.len(), output_pixels * 3 / 2, "I420 chroma planes were discarded");
        assert!(result.data[..output_pixels].iter().all(|v| *v == 76));
        assert!(result.data[output_pixels..output_pixels * 5 / 4].iter().all(|v| *v == 85));
        assert!(result.data[output_pixels * 5 / 4..].iter().all(|v| *v == 255));
    }

    fn slot_rects(n: u32) -> (BTreeMap<u32, Arc<Frame>>, BTreeMap<u32, PixelRect>) {
        // n distinct frames + n distinct 960×540 tiles in a 2×2 grid.
        let tiles = [(0u32, 0u32), (960, 0), (0, 540), (960, 540)];
        let mut frames = BTreeMap::new();
        let mut rects = BTreeMap::new();
        for i in 0..n.min(4) {
            let (x, y) = tiles[i as usize];
            frames.insert(i, Arc::new(Frame::filled(320, 180, (10 * (i + 1)) as u8)));
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
        frames.insert(0u32, Arc::new(Frame::filled(1920, 1080, 42)));
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
        frames.insert(0u32, Arc::new(Frame::filled(320, 180, 7)));
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
        let frames =
            BTreeMap::from([(0, Arc::new(Frame::filled(320, 180, 10))), (1, Arc::new(Frame::filled(320, 180, 20)))]);
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
        frames.insert(0u32, Arc::new(Frame::filled(320, 240, 5)));
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
