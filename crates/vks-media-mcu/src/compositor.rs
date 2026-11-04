//! Compositor: N input frames + per-slot [`PixelRect`]s → a single
//! 1920×1080 [`Frame`].
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): N видео → canvas 1920×1080. The default
//! implementation is a deterministic nearest-neighbour scaler with
//! letterboxing (uniform fit, centered) into each rect region; the 2D
//! bilinear scaler is T05b. There is no `oxideav` feature in this crate
//! (the empty feature was removed in T03c). Frame rate is fixed at 30 fps
//! (drop/repeat, no interpolation) — see the engine.
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

impl Compositor {
    /// Composes `frames` (slot key → frame) into a canvas using `rects`
    /// (slot key → target rect).
    ///
    /// - Each frame is scaled to its rect with a uniform scale
    ///   (letterbox) and drawn centered inside the rect. Letterbox bars are not
    ///   painted: they keep whatever the canvas already holds (background for
    ///   the first layer, lower-z content for a PiP).
    /// - Slots with no frame are left black.
    /// - Rects are clamped to the canvas.
    /// - Overlapping rects: higher `z` wins; equal `z` falls back to slot key
    ///   (deterministic order).
    pub fn compose(&self, frames: &BTreeMap<u32, Arc<Frame>>, rects: &BTreeMap<u32, PixelRect>) -> Frame {
        self.compose_sized(CANVAS_WIDTH, CANVAS_HEIGHT, frames, rects)
    }

    /// Как [`Compositor::compose`], но на холсте произвольного размера.
    ///
    /// `O1`: холст комнаты = выходной профиль (720p/360p/1080p), а не всегда
    /// 1920×1080. Тогда egress-`resize_i420` при совпадении холста с профилем
    /// становится `memcpy` (0.26 мс на 1080p вместо 18.8–20.9 мс скалярного
    /// сэмплера). Нечётные размеры дают grayscale-холст: YUV420 требует
    /// чётности, а профили шлюза всегда чётные — это защитная ветка.
    pub fn compose_sized(
        &self,
        width: u32,
        height: u32,
        frames: &BTreeMap<u32, Arc<Frame>>,
        rects: &BTreeMap<u32, PixelRect>,
    ) -> Frame {
        let mut canvas = Frame::new(width, height, Vec::new());
        self.compose_sized_into(&mut canvas, frames, rects);
        canvas
    }

    /// Рендерит композит в существующий `canvas`, избегая аллокации нового буфера.
    pub fn compose_sized_into(
        &self,
        canvas: &mut Frame,
        frames: &BTreeMap<u32, Arc<Frame>>,
        rects: &BTreeMap<u32, PixelRect>,
    ) {
        let (width, height) = (canvas.width, canvas.height);
        if width == 0 || height == 0 {
            canvas.data.clear();
            return;
        }
        let pixels = width as usize * height as usize;
        let is_even_canvas = width.is_multiple_of(2) && height.is_multiple_of(2);
        let yuv_canvas =
            is_even_canvas && (frames.is_empty() || frames.values().any(|frame| frame.is_yuv420p()));
        let required_len = if yuv_canvas { pixels * 3 / 2 } else { pixels };
        if canvas.data.len() != required_len {
            canvas.data.resize(required_len, 0);
        }
        if yuv_canvas {
            Self::fill_studio_black(&mut canvas.data, pixels);
        } else {
            #[cfg(target_arch = "x86_64")]
            if is_x86_feature_detected!("avx2") && pixels >= 32 {
                unsafe {
                    Self::fill_row_avx2(&mut canvas.data[..pixels], 0);
                }
            } else {
                canvas.data[..pixels].fill(0);
            }
            #[cfg(not(target_arch = "x86_64"))]
            canvas.data[..pixels].fill(0);
        }
        // Overlap/PiP: z_index задаёт порядок отрисовки, ключ — стабильный
        // tie-break. BTreeMap сам по себе сортировал только по slot id.
        let mut ordered: Vec<(&u32, &PixelRect)> = rects.iter().collect();
        ordered.sort_unstable_by_key(|(key, rect)| (rect.z, **key));
        for (key, rect) in ordered {
            if let Some(frame) = frames.get(key) {
                Self::blit(canvas, frame, rect);
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn blend_50_avx2(row: &mut [u8], target_val: u8) {
        use std::arch::x86_64::*;
        let target_vec = _mm256_set1_epi8(target_val as i8);
        let mut i = 0;
        let len = row.len();
        while i + 32 <= len {
            let ptr = row.as_mut_ptr().add(i);
            let v = _mm256_loadu_si256(ptr as *const __m256i);
            let blended = _mm256_avg_epu8(v, target_vec);
            _mm256_storeu_si256(ptr as *mut __m256i, blended);
            i += 32;
        }
        while i < len {
            *row.get_unchecked_mut(i) = ((*row.get_unchecked(i) as u16 + target_val as u16 + 1) / 2) as u8;
            i += 1;
        }
    }

    fn blend_50(row: &mut [u8], target_val: u8) {
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") {
                unsafe {
                    Self::blend_50_avx2(row, target_val);
                }
                return;
            }
        }
        for p in row {
            *p = ((*p as u16 + target_val as u16 + 1) / 2) as u8;
        }
    }

    /// Отрисовка полупрозрачной OSD-плашки с именем участника внизу слота.
    /// Работает поверх активного видеокадра или чистого фона конференции (без заглушек).
    pub fn render_video_name_badge(canvas: &mut Frame, rect: &PixelRect, label: &str) {
        if label.trim().is_empty() {
            return;
        }
        let (cw, ch) = (canvas.width as usize, canvas.height as usize);
        if cw == 0 || ch == 0 {
            return;
        }
        let right = (rect.x.saturating_add(rect.w) as usize).min(cw);
        let bottom = (rect.y.saturating_add(rect.h) as usize).min(ch);
        let rx = (rect.x as usize).min(cw);
        let ry = (rect.y as usize).min(ch);
        let rw = right.saturating_sub(rx);
        let rh = bottom.saturating_sub(ry);
        if rw < 8 || rh < 8 {
            return;
        }

        let is_yuv = canvas.is_yuv420p();
        let bar_h = (rh / 10).clamp(16, 40);
        let bar_y = ry + rh.saturating_sub(bar_h);
        let bar_bottom = ry + rh;

        // 1. Векторный 50% альфа-блендинг строки Y с темным фоном (Y=16)
        for cy in bar_y..bar_bottom {
            let row_start = cy * cw + rx;
            let row_end = row_start + rw;
            if let Some(row_slice) = canvas.data.get_mut(row_start..row_end) {
                Self::blend_50(row_slice, 16);
            }
        }

        // 2. Блендинг U и V плоскостей к нейтральному уровню 128 (для YUV420p)
        if is_yuv {
            let u_plane = cw * ch;
            let v_plane = cw * ch + cw * ch / 4;
            let uv_cw = cw / 2;
            let uv_rx = rx / 2;
            let uv_rw = (rw / 2).max(1);
            let uv_bar_y = bar_y / 2;
            let uv_bar_bottom = (bar_bottom / 2).min(ch / 2);

            for row_y in uv_bar_y..uv_bar_bottom {
                let u_row = u_plane + row_y * uv_cw + uv_rx;
                let v_row = v_plane + row_y * uv_cw + uv_rx;
                if let Some(slice_u) = canvas.data.get_mut(u_row..u_row + uv_rw) {
                    Self::blend_50(slice_u, 128);
                }
                if let Some(slice_v) = canvas.data.get_mut(v_row..v_row + uv_rw) {
                    Self::blend_50(slice_v, 128);
                }
            }
        }

        // 3. Рендеринг текста имени (с масштабированием и поддержкой кириллицы)
        let scale = (bar_h / 14).clamp(1, 3);
        let font_h = 7 * scale;
        let text_y = bar_y + bar_h.saturating_sub(font_h) / 2;
        let char_w = 6 * scale;
        let max_chars = rw.saturating_sub(scale * 4) / char_w;
        if max_chars == 0 {
            return;
        }

        let char_count = label.chars().count();
        let display_text: String = if char_count > max_chars && max_chars > 2 {
            let mut s: String = label.chars().take(max_chars - 2).collect();
            s.push_str("..");
            s
        } else {
            label.chars().take(max_chars).collect()
        };

        let text_w = display_text.chars().count() * char_w;
        let start_x = rx + rw.saturating_sub(text_w) / 2;

        for (i, c) in display_text.chars().enumerate() {
            let glyph = Self::font_glyph_5x7(c);
            let char_x = start_x + i * char_w;
            for col in 0..5 {
                let col_bits = glyph[col];
                for row in 0..7 {
                    if (col_bits >> row) & 1 == 1 {
                        for sy in 0..scale {
                            let py = text_y + row * scale + sy;
                            if py >= ch {
                                break;
                            }
                            for sx in 0..scale {
                                let px = char_x + col * scale + sx;
                                if px >= right || px >= cw {
                                    break;
                                }
                                let y_idx = py * cw + px;
                                if let Some(p) = canvas.data.get_mut(y_idx) {
                                    *p = 235; // Bright white Y
                                }
                                if is_yuv {
                                    let uv_cw = cw / 2;
                                    let u_idx = cw * ch + (py / 2) * uv_cw + (px / 2);
                                    let v_idx = cw * ch + (cw * ch / 4) + (py / 2) * uv_cw + (px / 2);
                                    if let Some(up) = canvas.data.get_mut(u_idx) {
                                        *up = 128;
                                    }
                                    if let Some(vp) = canvas.data.get_mut(v_idx) {
                                        *vp = 128;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn font_glyph_5x7(c: char) -> [u8; 5] {
        let code = c as u32;
        if (32..=126).contains(&code) {
            return FONT_5X7[(code - 32) as usize];
        }
        let upper = c.to_uppercase().next().unwrap_or(c);
        match upper {
            'А' => FONT_5X7[('A' as u32 - 32) as usize],
            'Б' => [0x7f, 0x49, 0x49, 0x49, 0x71],
            'В' => FONT_5X7[('B' as u32 - 32) as usize],
            'Г' => [0x7f, 0x01, 0x01, 0x01, 0x01],
            'Д' => [0x60, 0x3f, 0x21, 0x3f, 0x60],
            'Е' | 'Ё' => FONT_5X7[('E' as u32 - 32) as usize],
            'Ж' => [0x49, 0x2a, 0x7f, 0x2a, 0x49],
            'З' => [0x22, 0x41, 0x49, 0x49, 0x36],
            'И' => [0x7f, 0x10, 0x08, 0x04, 0x7f],
            'Й' => [0x7f, 0x10, 0x09, 0x04, 0x7f],
            'К' => FONT_5X7[('K' as u32 - 32) as usize],
            'Л' => [0x60, 0x1f, 0x01, 0x01, 0x7f],
            'М' => FONT_5X7[('M' as u32 - 32) as usize],
            'Н' => FONT_5X7[('H' as u32 - 32) as usize],
            'О' => FONT_5X7[('O' as u32 - 32) as usize],
            'П' => [0x7f, 0x01, 0x01, 0x01, 0x7f],
            'Р' => FONT_5X7[('P' as u32 - 32) as usize],
            'С' => FONT_5X7[('C' as u32 - 32) as usize],
            'Т' => FONT_5X7[('T' as u32 - 32) as usize],
            'У' => [0x07, 0x08, 0x70, 0x08, 0x07],
            'Ф' => [0x1c, 0x22, 0x7f, 0x22, 0x1c],
            'Х' => FONT_5X7[('X' as u32 - 32) as usize],
            'Ц' => [0x7f, 0x40, 0x40, 0x7f, 0xc0],
            'Ч' => [0x0f, 0x08, 0x08, 0x08, 0x7f],
            'Ш' => [0x7f, 0x40, 0x7f, 0x40, 0x7f],
            'Щ' => [0x7f, 0x40, 0x7f, 0x40, 0xff],
            'Ъ' => [0x01, 0x7f, 0x48, 0x48, 0x30],
            'Ы' => [0x7f, 0x48, 0x48, 0x30, 0x7f],
            'Ь' => [0x7f, 0x48, 0x48, 0x48, 0x30],
            'Э' => [0x22, 0x41, 0x49, 0x49, 0x3e],
            'Ю' => [0x7f, 0x08, 0x3e, 0x41, 0x3e],
            'Я' => [0x46, 0x29, 0x19, 0x09, 0x7f],
            _ => [0x02, 0x01, 0x51, 0x09, 0x06], // '?'
        }
    }

    /// Рендерит композит с OSD-плашками в существующий `canvas`.
    pub fn compose_sized_with_labels_into(
        &self,
        canvas: &mut Frame,
        frames: &BTreeMap<u32, Arc<Frame>>,
        rects: &BTreeMap<u32, PixelRect>,
        labels: &BTreeMap<u32, String>,
    ) {
        self.compose_sized_into(canvas, frames, rects);
        let mut ordered: Vec<(&u32, &PixelRect)> = rects.iter().collect();
        ordered.sort_unstable_by_key(|(key, rect)| (rect.z, **key));
        for (key, rect) in ordered {
            if let Some(label) = labels.get(key) {
                if !label.trim().is_empty() {
                    Self::render_video_name_badge(canvas, rect, label.trim());
                }
            }
        }
    }

    /// Композитинг с отрисовкой OSD-плашек имен для всех назначенных участников.
    pub fn compose_sized_with_labels(
        &self,
        width: u32,
        height: u32,
        frames: &BTreeMap<u32, Arc<Frame>>,
        rects: &BTreeMap<u32, PixelRect>,
        labels: &BTreeMap<u32, String>,
    ) -> Frame {
        let mut canvas = Frame::new(width, height, Vec::new());
        self.compose_sized_with_labels_into(&mut canvas, frames, rects, labels);
        canvas
    }

    /// Scales `frame` into `rect` (uniform fit, centered) in the canvas.
    ///
    /// Nearest-neighbour mapping is exact integer floor math:
    /// `fx = dx * fw / dw`, `fy = dy * fh / dh`. The earlier 16.16 fixed-point
    /// sampler truncated `dx / s` and could pick a source column one to the left;
    /// the horizontal index is now advanced incrementally, so no division runs
    /// per pixel and the mapping is exactly `floor(dx*fw/dw)`.
    pub fn blit(canvas: &mut Frame, frame: &Frame, rect: &PixelRect) {
        let (cw, ch) = (canvas.width as usize, canvas.height as usize);
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
        // Uniform (letterbox) fit, matching the room layout convention, in integer math:
        // width-limited when rw/fw <= rh/fh, else height-limited. The free axis is
        // rounded half-up exactly like the former `f64::round`.
        let (mut dw, mut dh) = if rw * fh <= rh * fw {
            (rw, ((fh * rw + fw / 2) / fw).max(1))
        } else {
            (((fw * rh + fh / 2) / fh).max(1), rh)
        };

        let is_yuv_canvas = canvas.data.len() == cw * ch * 3 / 2;
        if is_yuv_canvas {
            dw = (dw & !1).max(2).min(rw);
            dh = (dh & !1).max(2).min(rh);
        }

        let mut ox = rx + rw.saturating_sub(dw) / 2;
        let mut oy = ry + rh.saturating_sub(dh) / 2;
        if is_yuv_canvas {
            ox = (ox & !1).min(cw.saturating_sub(dw));
            oy = (oy & !1).min(ch.saturating_sub(dh));
        }
        let step_x = fw / dw;
        let rem_x = fw % dw;
        for dy in 0..dh {
            let fy = if fh == dh { dy } else { dy * fh / dh };
            let Some(source) = frame.data.get(fy * fw..fy * fw + fw) else { continue };
            let destination_row = (oy + dy) * cw;
            let Some(destination) = canvas.data.get_mut(destination_row + ox..destination_row + ox + dw) else {
                continue;
            };
            Self::scale_row(destination, source, dw, step_x, rem_x);
        }
        // Chroma is half-resolution. Layout rects can have odd origins or widths
        // (Focus strips with 8+ participants, 7x7 presets, crossfade intermediate
        // rects), so the boundary chroma column is shared with the letterbox or
        // neighbouring sample and nearest 2x2 siting can bleed by one chroma
        // sample. The 4-participant 2x2 soak uses even tiles and does not exercise
        // this; no chroma correction is applied.
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
                    let source_y = if chroma_height == chroma_dh { dy } else { dy * chroma_height / chroma_dh.max(1) };
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
                    let source_row = &frame.data[source_offset + source_y * chroma_width
                        ..source_offset + source_y * chroma_width + chroma_width];
                    let cw_dest = chroma_dw.max(1);
                    Self::scale_row(destination, source_row, cw_dest, chroma_step_x, chroma_rem_x);
                }
            }
        }
    }
    /// Copies one scaled row using exact `floor(dx * src_w / dst_w)`.
    ///
    /// The scalar fallback is the incremental quotient/remainder sampler. Exact
    /// integer downscales take every `src_w / dst_w`-th sample; the canonical
    /// 2×2 layout (1280×720 into 960×540) is a 4:3 downscale, where three
    /// output samples come from each group of four input samples (`0, 1, 2`;
    /// `3` is skipped). Both fast paths are byte-identical to the fallback; the
    /// tests pin this against the floor formula.
    pub fn scale_row(destination: &mut [u8], source: &[u8], dw: usize, step_x: usize, rem_x: usize) {
        debug_assert_eq!(destination.len(), dw);
        if step_x == 1 && rem_x == 0 {
            if let Some(source) = source.get(..dw) {
                #[cfg(target_arch = "x86_64")]
                if is_x86_feature_detected!("avx2") && dw >= 32 {
                    unsafe {
                        Self::copy_row_avx2(destination, source);
                    }
                    return;
                }
                destination.copy_from_slice(source);
            }
            return;
        }
        if rem_x == 0 && step_x == 2 {
            #[cfg(target_arch = "x86_64")]
            if is_x86_feature_detected!("avx2") && dw >= 16 && source.len() >= dw * 2 {
                unsafe {
                    Self::downsample_2x_avx2(destination, source, dw);
                }
                return;
            }
        }
        if rem_x == 0 && step_x > 1 {
            if let Some(source) = source.get(..dw.saturating_mul(step_x)) {
                for (pixel, chunk) in destination.iter_mut().zip(source.chunks_exact(step_x)) {
                    *pixel = chunk[0];
                }
                return;
            }
        }
        // 4:3 downscale (fw*3 == dw*4): each 3 output samples use source 0,1,2
        // of the next 4, matching floor(x*4/3) exactly. Canonical 2×2 tiles.
        if dw.is_multiple_of(3) && source.len().saturating_mul(3) == dw.saturating_mul(4) {
            for (group, chunk) in destination.chunks_exact_mut(3).zip(source.chunks_exact(4)) {
                group[0] = chunk[0];
                group[1] = chunk[1];
                group[2] = chunk[2];
            }
            return;
        }
        let mut fx = 0usize;
        let mut rem = 0usize;
        for pixel in destination {
            *pixel = source.get(fx).copied().unwrap_or(16);
            fx += step_x;
            rem += rem_x;
            if rem >= dw {
                rem -= dw;
                fx += 1;
            }
        }
    }

    /// Затирает `rect` фоном канваса (luma 16 / chroma 128 для YUV, 0 для
    /// серого). Используется персональной сборкой: ячейка зрителя в общей
    /// сетке освобождается под самопросмотр.
    pub fn clear(canvas: &mut Frame, rect: &PixelRect) {
        let (cw, ch) = (canvas.width as usize, canvas.height as usize);
        let right = (rect.x.saturating_add(rect.w) as usize).min(cw);
        let bottom = (rect.y.saturating_add(rect.h) as usize).min(ch);
        let rx = (rect.x as usize).min(cw);
        let ry = (rect.y as usize).min(ch);
        let rw = right.saturating_sub(rx);
        let rh = bottom.saturating_sub(ry);
        if rw == 0 || rh == 0 {
            return;
        }
        let luma = if canvas.is_yuv420p() { 16 } else { 0 };
        #[cfg(target_arch = "x86_64")]
        let use_avx2 = is_x86_feature_detected!("avx2") && rw >= 32;
        for row in ry..ry + rh {
            let offset = row * cw + rx;
            if let Some(line) = canvas.data.get_mut(offset..offset + rw) {
                #[cfg(target_arch = "x86_64")]
                if use_avx2 {
                    unsafe {
                        Self::fill_row_avx2(line, luma);
                    }
                    continue;
                }
                line.fill(luma);
            }
        }
        // YUV-канвас: хроматическая плоскость вдвое меньше по каждой оси.
        if canvas.data.len() == cw * ch * 3 / 2 {
            let chroma_right = (right / 2).min(cw / 2);
            let chroma_bottom = (bottom / 2).min(ch / 2);
            let chroma_rx = rx / 2;
            let chroma_ry = ry / 2;
            let chroma_rw = chroma_right.saturating_sub(chroma_rx);
            #[cfg(target_arch = "x86_64")]
            let use_avx2_chroma = is_x86_feature_detected!("avx2") && chroma_rw >= 32;
            for plane in 0..2 {
                let plane_offset = cw * ch + plane * (cw * ch / 4);
                for row in chroma_ry..chroma_bottom {
                    let offset = plane_offset + row * (cw / 2) + chroma_rx;
                    if let Some(line) = canvas.data.get_mut(offset..offset + chroma_rw) {
                        #[cfg(target_arch = "x86_64")]
                        if use_avx2_chroma {
                            unsafe {
                                Self::fill_row_avx2(line, 128);
                            }
                            continue;
                        }
                        line.fill(128);
                    }
                }
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn copy_row_avx2(dst: &mut [u8], src: &[u8]) {
        use std::arch::x86_64::*;
        let len = dst.len().min(src.len());
        let mut i = 0;
        while i + 32 <= len {
            let chunk = _mm256_loadu_si256(src.as_ptr().add(i) as *const __m256i);
            _mm256_storeu_si256(dst.as_mut_ptr().add(i) as *mut __m256i, chunk);
            i += 32;
        }
        if i < len {
            std::ptr::copy_nonoverlapping(src.as_ptr().add(i), dst.as_mut_ptr().add(i), len - i);
        }
    }

    pub(crate) fn fill_studio_black(data: &mut [u8], pixels: usize) {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") && pixels >= 32 {
            unsafe {
                Self::fill_row_avx2(&mut data[..pixels], 16);
                if data.len() >= pixels * 3 / 2 {
                    Self::fill_row_avx2(&mut data[pixels..pixels * 3 / 2], 128);
                }
            }
            return;
        }
        data[..pixels].fill(16);
        if data.len() >= pixels * 3 / 2 {
            data[pixels..pixels * 3 / 2].fill(128);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    pub(crate) unsafe fn fill_row_avx2(dst: &mut [u8], value: u8) {
        use std::arch::x86_64::*;
        let fill = _mm256_set1_epi8(value as i8);
        let len = dst.len();
        let mut i = 0;
        while i + 32 <= len {
            _mm256_storeu_si256(dst.as_mut_ptr().add(i) as *mut __m256i, fill);
            i += 32;
        }
        if i < len {
            dst[i..].fill(value);
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn downsample_2x_avx2(dst: &mut [u8], src: &[u8], count: usize) {
        use std::arch::x86_64::*;
        let shuffle_mask = _mm256_setr_epi8(
            0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1,
            0, 2, 4, 6, 8, 10, 12, 14, -1, -1, -1, -1, -1, -1, -1, -1,
        );
        let mut i = 0;
        while i + 16 <= count && (i * 2 + 32) <= src.len() {
            let v = _mm256_loadu_si256(src.as_ptr().add(i * 2) as *const __m256i);
            let shuffled = _mm256_shuffle_epi8(v, shuffle_mask);
            let lo = _mm256_castsi256_si128(shuffled);
            let hi = _mm256_extracti128_si256(shuffled, 1);
            let packed = _mm_unpacklo_epi64(lo, hi);
            _mm_storeu_si128(dst.as_mut_ptr().add(i) as *mut __m128i, packed);
            i += 16;
        }
        while i < count && i * 2 < src.len() {
            *dst.get_unchecked_mut(i) = *src.get_unchecked(i * 2);
            i += 1;
        }
    }
}

pub const FONT_5X7: [[u8; 5]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00],
    [0x00, 0x00, 0x5f, 0x00, 0x00],
    [0x00, 0x07, 0x00, 0x07, 0x00],
    [0x14, 0x7f, 0x14, 0x7f, 0x14],
    [0x24, 0x2a, 0x7f, 0x2a, 0x12],
    [0x23, 0x13, 0x08, 0x64, 0x62],
    [0x36, 0x49, 0x55, 0x22, 0x50],
    [0x00, 0x05, 0x03, 0x00, 0x00],
    [0x00, 0x1c, 0x22, 0x41, 0x00],
    [0x00, 0x41, 0x22, 0x1c, 0x00],
    [0x08, 0x2a, 0x1c, 0x2a, 0x08],
    [0x08, 0x08, 0x3e, 0x08, 0x08],
    [0x00, 0x50, 0x30, 0x00, 0x00],
    [0x08, 0x08, 0x08, 0x08, 0x08],
    [0x00, 0x60, 0x60, 0x00, 0x00],
    [0x20, 0x10, 0x08, 0x04, 0x02],
    [0x3e, 0x51, 0x49, 0x45, 0x3e],
    [0x00, 0x42, 0x7f, 0x40, 0x00],
    [0x42, 0x61, 0x51, 0x49, 0x46],
    [0x21, 0x41, 0x45, 0x4b, 0x31],
    [0x18, 0x14, 0x12, 0x7f, 0x10],
    [0x27, 0x45, 0x45, 0x45, 0x39],
    [0x3c, 0x4a, 0x49, 0x49, 0x30],
    [0x01, 0x71, 0x09, 0x05, 0x03],
    [0x36, 0x49, 0x49, 0x49, 0x36],
    [0x06, 0x49, 0x49, 0x29, 0x1e],
    [0x00, 0x36, 0x36, 0x00, 0x00],
    [0x00, 0x56, 0x36, 0x00, 0x00],
    [0x00, 0x08, 0x14, 0x22, 0x41],
    [0x14, 0x14, 0x14, 0x14, 0x14],
    [0x41, 0x22, 0x14, 0x08, 0x00],
    [0x02, 0x01, 0x51, 0x09, 0x06],
    [0x32, 0x49, 0x79, 0x41, 0x3e],
    [0x7e, 0x11, 0x11, 0x11, 0x7e],
    [0x7f, 0x49, 0x49, 0x49, 0x36],
    [0x3e, 0x41, 0x41, 0x41, 0x22],
    [0x7f, 0x41, 0x41, 0x22, 0x1c],
    [0x7f, 0x49, 0x49, 0x49, 0x41],
    [0x7f, 0x09, 0x09, 0x01, 0x01],
    [0x3e, 0x41, 0x41, 0x51, 0x32],
    [0x7f, 0x08, 0x08, 0x08, 0x7f],
    [0x00, 0x41, 0x7f, 0x41, 0x00],
    [0x20, 0x40, 0x41, 0x3f, 0x01],
    [0x7f, 0x08, 0x14, 0x22, 0x41],
    [0x7f, 0x40, 0x40, 0x40, 0x40],
    [0x7f, 0x02, 0x04, 0x02, 0x7f],
    [0x7f, 0x04, 0x08, 0x10, 0x7f],
    [0x3e, 0x41, 0x41, 0x41, 0x3e],
    [0x7f, 0x09, 0x09, 0x09, 0x06],
    [0x3e, 0x41, 0x51, 0x21, 0x5e],
    [0x7f, 0x09, 0x19, 0x29, 0x46],
    [0x46, 0x49, 0x49, 0x49, 0x31],
    [0x01, 0x01, 0x7f, 0x01, 0x01],
    [0x3f, 0x40, 0x40, 0x40, 0x3f],
    [0x1f, 0x20, 0x40, 0x20, 0x1f],
    [0x7f, 0x20, 0x18, 0x20, 0x7f],
    [0x63, 0x14, 0x08, 0x14, 0x63],
    [0x03, 0x04, 0x78, 0x04, 0x03],
    [0x61, 0x51, 0x49, 0x45, 0x43],
    [0x00, 0x7f, 0x41, 0x41, 0x00],
    [0x02, 0x04, 0x08, 0x10, 0x20],
    [0x00, 0x41, 0x41, 0x7f, 0x00],
    [0x04, 0x02, 0x01, 0x02, 0x04],
    [0x40, 0x40, 0x40, 0x40, 0x40],
    [0x00, 0x01, 0x02, 0x04, 0x00],
    [0x20, 0x54, 0x54, 0x54, 0x78],
    [0x7f, 0x48, 0x44, 0x44, 0x38],
    [0x38, 0x44, 0x44, 0x44, 0x20],
    [0x38, 0x44, 0x44, 0x48, 0x7f],
    [0x38, 0x54, 0x54, 0x54, 0x18],
    [0x08, 0x7e, 0x09, 0x01, 0x02],
    [0x08, 0x14, 0x54, 0x54, 0x3c],
    [0x7f, 0x08, 0x04, 0x04, 0x78],
    [0x00, 0x44, 0x7d, 0x40, 0x00],
    [0x20, 0x40, 0x44, 0x3d, 0x00],
    [0x00, 0x7f, 0x10, 0x28, 0x44],
    [0x00, 0x41, 0x7f, 0x40, 0x00],
    [0x7c, 0x04, 0x18, 0x04, 0x78],
    [0x7c, 0x08, 0x04, 0x04, 0x78],
    [0x38, 0x44, 0x44, 0x44, 0x38],
    [0x7c, 0x14, 0x14, 0x14, 0x08],
    [0x08, 0x14, 0x14, 0x18, 0x7c],
    [0x7c, 0x08, 0x04, 0x04, 0x08],
    [0x48, 0x54, 0x54, 0x54, 0x20],
    [0x04, 0x3f, 0x44, 0x40, 0x20],
    [0x3c, 0x40, 0x40, 0x20, 0x7c],
    [0x1c, 0x20, 0x40, 0x20, 0x1c],
    [0x3c, 0x40, 0x30, 0x40, 0x3c],
    [0x44, 0x28, 0x10, 0x28, 0x44],
    [0x0c, 0x50, 0x50, 0x50, 0x3c],
    [0x44, 0x64, 0x54, 0x4c, 0x44],
    [0x00, 0x08, 0x36, 0x41, 0x00],
    [0x00, 0x00, 0x7f, 0x00, 0x00],
    [0x00, 0x41, 0x36, 0x08, 0x00],
    [0x08, 0x08, 0x2a, 0x1c, 0x08],
];

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_i420(width: usize, height: usize) -> Frame {
        // 2-D luma and per-axis chroma gradients: a uniform or horizontal-only
        // fill cannot detect a vertical `fy` regression or a chroma mapping bug.
        let mut data = vec![0u8; width * height * 3 / 2];
        for y in 0..height {
            for x in 0..width {
                data[y * width + x] = ((x + 3 * y) % 251) as u8;
            }
        }
        let chroma = width * height / 4;
        let (cw, ch) = (width / 2, height / 2);
        for y in 0..ch {
            for x in 0..cw {
                data[width * height + y * cw + x] = (40 + x % 200) as u8;
                data[width * height + chroma + y * cw + x] = (60 + 3 * y % 180) as u8;
            }
        }
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
        for dy in [0usize, 1, 2, 7, 269, 539] {
            for dx in [0usize, 1, 2, 4, 9, 100, 359, 719] {
                let sx = dx * 320 / 720;
                let sy = dy * 240 / 540;
                let expected = ((sx + 3 * sy) % 251) as u8;
                let index = dy * canvas_w + 120 + dx;
                assert_eq!(out.data[index], expected, "dx={dx},dy={dy} must sample source ({sx},{sy})");
            }
        }
        // Chroma planes use the same exact mapping on their own resolution:
        // dw/2 = 360 columns start at ox/2 = 60. U varies by x, V by y, so both
        // chroma axes are pinned.
        let chroma_offset = canvas_w * CANVAS_HEIGHT as usize;
        let chroma_plane = (canvas_w / 2) * (CANVAS_HEIGHT as usize / 2);
        let chroma_stride = canvas_w / 2;
        for dy in [0usize, 1, 269] {
            let sy = dy * 120 / 270;
            let v_expected = (60 + 3 * sy % 180) as u8;
            for dx in [0usize, 1, 179, 359] {
                let sx = dx * 160 / 360;
                let u_expected = (40 + sx % 200) as u8;
                let index = chroma_offset + dy * chroma_stride + 60 + dx;
                assert_eq!(out.data[index], u_expected, "U plane at dx={dx},dy={dy}");
                assert_eq!(out.data[index + chroma_plane], v_expected, "V plane at dx={dx},dy={dy}");
            }
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

    /// A PiP over an existing frame must not clear its letterbox bars: the bars
    /// keep the lower-z layer (documented contract), while the fitted frame wins
    /// inside the image area.
    #[test]
    fn pip_letterbox_bars_keep_lower_layer() {
        let lower = Arc::new(Frame::filled(1920, 1080, 10));
        let pip = Arc::new(Frame::filled(640, 480, 77));
        let frames = BTreeMap::from([(0u32, lower), (1u32, pip)]);
        let rects = BTreeMap::from([
            (0u32, PixelRect::new(0, 0, 1920, 1080)),
            (1u32, PixelRect::new(0, 0, 960, 540).with_z(1)),
        ]);
        let out = Compositor.compose(&frames, &rects);
        assert_eq!(out.data[0], 10, "left bar of a 720x540 PiP keeps the lower layer");
        assert_eq!(out.data[119], 10);
        assert_eq!(out.data[120], 77, "fitted PiP starts at the centered offset");
        assert_eq!(out.data[120 + 719], 77);
        assert_eq!(out.data[120 + 720], 10, "right bar keeps the lower layer");
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
    fn exact_ratio_fast_paths_match_floor_mapping() {
        // 1280x720 -> 960x540: both axes are exactly 4:3, so the luma and
        // chroma fast paths run. 640x360 -> 480x270 is the same ratio and also
        // exercises integer chroma dimensions.
        for (fw, fh, dw) in [(1280usize, 720usize, 960usize), (640, 360, 480), (1280, 720, 640), (1280, 720, 1280)] {
            let mut source = vec![0u8; fw * fh];
            for y in 0..fh {
                for x in 0..fw {
                    source[y * fw + x] = ((x * 7 + y * 13) % 251) as u8;
                }
            }
            let mut destination = vec![0u8; dw];
            Compositor::scale_row(&mut destination, &source, dw, fw / dw, fw % dw);
            for (dx, pixel) in destination.iter().enumerate() {
                assert_eq!(*pixel, source[dx * fw / dw], "luma mismatch at dx={dx}, fw/dw={fw}/{dw}");
            }
        }
    }

    #[test]
    fn compose_sized_uses_requested_canvas() {
        let frames = BTreeMap::from([(0u32, Arc::new(Frame::new(64, 64, vec![7u8; 64 * 64])))]);
        let rects = BTreeMap::from([(0u32, PixelRect::new(0, 0, 1280, 720).with_z(0))]);
        let out = Compositor.compose_sized(1280, 720, &frames, &rects);
        assert_eq!((out.width, out.height), (1280, 720));
        assert_eq!(out.data.len(), 1280 * 720, "grayscale-холст для серого кадра");
        let out = Compositor.compose_sized(160, 90, &frames, &rects);
        assert_eq!((out.width, out.height), (160, 90));
        assert_eq!(out.data.len(), 160 * 90);
        assert_eq!(out.data[45 * 160 + 80], 7, "кадр попал в масштабированный rect");
        // Нечётный холст: YUV420 невозможен → grayscale, без паники.
        let out = Compositor.compose_sized(161, 91, &frames, &rects);
        assert_eq!(out.data.len(), 161 * 91);
        // Нулевой холст не паникует.
        assert!(Compositor.compose_sized(0, 0, &frames, &rects).data.is_empty());
    }

    #[test]
    fn two_frames_land_in_their_tiles() {
        let (frames, rects) = slot_rects(2);
        let out = Compositor.compose(&frames, &rects);
        assert_eq!((out.width, out.height), (CANVAS_WIDTH, CANVAS_HEIGHT));
        // Each frame occupies exactly its 960×540 tile, fully filled.
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
        let pixels = CANVAS_WIDTH as usize * CANVAS_HEIGHT as usize;
        let mut expected = vec![128u8; pixels * 3 / 2];
        expected[..pixels].fill(16);
        assert_eq!(out.data, expected, "no frames → studio black YUV420p canvas");
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
    fn clear_yuv_uses_studio_black_and_neutral_chroma() {
        let mut canvas = Frame::new(1920, 1080, vec![42u8; 1920 * 1080 * 3 / 2]);
        Compositor::clear(&mut canvas, &PixelRect::new(0, 0, 960, 540));
        assert_eq!(canvas.data[0], 16, "YUV luma background must be studio black");
        assert_eq!(canvas.data[100], 16);
        let chroma = 1920 * 1080;
        assert_eq!(canvas.data[chroma], 128, "YUV chroma background must be neutral");
        assert_eq!(canvas.data[chroma + 270 * 960], 42, "chroma outside the cleared rect stays untouched");
    }

    #[test]
    fn clear_grayscale_uses_zero_background() {
        let mut canvas = Frame::new(1920, 1080, vec![42u8; 1920 * 1080]);
        Compositor::clear(&mut canvas, &PixelRect::new(0, 0, 960, 540));
        assert_eq!(canvas.data[0], 0, "grayscale canvas background is 0, as documented");
        assert_eq!(canvas.data[100], 0);
        assert_eq!(canvas.data[1000 * 1920], 42, "outside the cleared rect stays untouched");
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

    #[test]
    fn compose_sized_with_labels_renders_placeholder_and_text() {
        let frames = BTreeMap::new();
        let rects = BTreeMap::from([(0u32, PixelRect::new(0, 0, 960, 540))]);
        let labels = BTreeMap::from([(0u32, "TrueConf Client".to_string())]);
        let out = Compositor.compose_sized_with_labels(960, 540, &frames, &rects, &labels);
        assert_eq!((out.width, out.height), (960, 540));
        let has_avatar = out.data.iter().any(|&p| p == 140);
        let has_text = out.data.iter().any(|&p| p == 235);
        assert!(!has_avatar, "placeholder silhouette must NOT be rendered");
        assert!(has_text, "name badge text rendered");
    }

    #[test]
    fn compose_sized_with_labels_renders_cyrillic_text() {
        let frames = BTreeMap::new();
        let rects = BTreeMap::from([(0u32, PixelRect::new(0, 0, 960, 540))]);
        let labels = BTreeMap::from([(0u32, "Поликом Конференция".to_string())]);
        let out = Compositor.compose_sized_with_labels(960, 540, &frames, &rects, &labels);
        let has_text = out.data.iter().any(|&p| p == 235);
        assert!(has_text, "cyrillic text badge rendered");
    }

    #[test]
    fn compose_sized_with_labels_overlays_badge_on_active_video() {
        let frame = Arc::new(Frame::filled(960, 540, 100));
        let frames = BTreeMap::from([(0u32, frame)]);
        let rects = BTreeMap::from([(0u32, PixelRect::new(0, 0, 960, 540))]);
        let labels = BTreeMap::from([(0u32, "Active Video".to_string())]);
        let out = Compositor.compose_sized_with_labels(960, 540, &frames, &rects, &labels);
        let has_text = out.data.iter().any(|&p| p == 235);
        assert!(has_text, "name badge text rendered over active video");
    }
}
