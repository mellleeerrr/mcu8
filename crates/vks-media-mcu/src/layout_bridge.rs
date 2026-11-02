//! Layout bridge: `vks-layout::Layout` → pixel `Rect`s for the
//! compositor, with a 300 ms crossfade on layout changes.
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): vks-layout → пиксельные Rect
//! (crossfade 300 ms при смене). When the applied layout changes, the
//! per-slot rects blend linearly from the previous layout's rects to the
//! new ones over `crossfade_ms` (300 ms); after that window the rects
//! snap to the new layout.
//!
//! The bridge is stateless per `elapsed_ms` input and deterministic:
//! the same (previous, current, elapsed) triple always yields the same
//! rects. The compositor calls [`LayoutBridge::current`] every frame with
//! the elapsed time since the last layout change.
//!
//! No `unsafe`.

use std::collections::BTreeMap;

use vks_layout::Layout;

/// Default crossfade window (ms) per DEVELOPMENT_PLAN (300 ms).
pub const CROSSFADE_MS: u64 = 300;

/// A pixel rect (u32) for the compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    /// Left (px).
    pub x: u32,
    /// Top (px).
    pub y: u32,
    /// Width (px).
    pub w: u32,
    /// Height (px).
    pub h: u32,
    /// Stacking order: higher z is drawn later (on top).
    pub z: i32,
}

impl PixelRect {
    /// Creates a pixel rect.
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h, z: 0 }
    }

    /// Sets stacking order (builder style).
    #[must_use]
    pub fn with_z(mut self, z: i32) -> Self {
        self.z = z;
        self
    }

    /// Zero rect.
    pub fn empty() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

/// Slot id key used in the bridge (opaque, orderable for determinism).
pub type SlotKey = u32;

/// Slot rects map: slot key → [`PixelRect`].
pub type SlotRects = BTreeMap<SlotKey, PixelRect>;

/// Zero-area rect at the origin (fade-in/out anchor).
fn zero_rect() -> PixelRect {
    PixelRect::new(0, 0, 0, 0)
}

/// Interpolates two rects linearly: `from` at t=0, `to` at t=1.
fn lerp(a: u32, b: u32, t: f64) -> u32 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u32
}

fn lerp_rect(from: &PixelRect, to: &PixelRect, t: f64) -> PixelRect {
    let x = lerp(from.x, to.x, t) & !1;
    let y = lerp(from.y, to.y, t) & !1;
    let w = (lerp(from.w, to.w, t) & !1).max(2);
    let h = (lerp(from.h, to.h, t) & !1).max(2);
    PixelRect::new(x, y, w, h).with_z(to.z)
}

/// Layout bridge: produces per-slot pixel rects for the compositor.
#[derive(Debug, Clone)]
pub struct LayoutBridge {
    /// Crossfade window (ms).
    pub crossfade_ms: u64,
    /// Rects of the previously applied layout (crossfade "from").
    previous: SlotRects,
    /// Rects of the currently applied layout (crossfade "to").
    current: SlotRects,
}

impl Default for LayoutBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl LayoutBridge {
    /// Creates a bridge with the default 300 ms crossfade.
    pub fn new() -> Self {
        Self { crossfade_ms: CROSSFADE_MS, previous: BTreeMap::new(), current: BTreeMap::new() }
    }

    /// Creates a bridge with a custom crossfade window.
    pub fn with_crossfade_ms(crossfade_ms: u64) -> Self {
        Self { crossfade_ms, previous: BTreeMap::new(), current: BTreeMap::new() }
    }

    /// Applies a new layout, starting a crossfade from the previous
    /// layout's rects.
    pub fn apply(&mut self, layout: &Layout) {
        self.previous = self.current.clone();
        self.current = Self::rects_of(layout);
    }

    /// The rects of a layout, keyed by slot id.
    fn rects_of(layout: &Layout) -> SlotRects {
        layout
            .slots
            .iter()
            .map(|s| (s.id.as_u32(), PixelRect::new(s.rect.x, s.rect.y, s.rect.w, s.rect.h).with_z(s.z_index)))
            .collect()
    }

    /// Computes the effective rects at `elapsed_ms` since the last layout
    /// change.
    ///
    /// - `elapsed_ms >= crossfade_ms` (or first layout): snap to `current`.
    /// - `0 < elapsed_ms < crossfade_ms`: linear blend of `previous` and
    ///   `current`; slots present on only one side are taken as-is
    ///   (appearing slots fade in from zero-area at their new position,
    ///   disappearing slots fade out toward zero-area).
    /// - `elapsed_ms == 0`: `previous` (the state before the change).
    pub fn current(&self, elapsed_ms: u64) -> SlotRects {
        if elapsed_ms == 0 && !self.previous.is_empty() {
            return self.previous.clone();
        }
        if elapsed_ms >= self.crossfade_ms || self.previous.is_empty() {
            return self.current.clone();
        }
        let t = elapsed_ms as f64 / self.crossfade_ms as f64;
        let mut out = BTreeMap::new();
        let zero = zero_rect();
        for (key, to) in &self.current {
            let from = self.previous.get(key).unwrap_or(&zero);
            out.insert(*key, lerp_rect(from, to, t));
        }
        for (key, from) in &self.previous {
            if !self.current.contains_key(key) {
                out.insert(*key, lerp_rect(from, &zero, t));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_layout::{LayoutKind, Rect, Slot};

    fn layout_v1() -> Layout {
        let mut l = Layout::new(LayoutKind::Grid);
        l.version = 1;
        l.slots = vec![
            Slot::new(0, None, Rect::new(0, 0, 960, 540)),
            Slot::new(1, None, Rect::new(960, 0, 960, 540)),
            Slot::new(2, None, Rect::new(0, 540, 960, 540)),
            Slot::new(3, None, Rect::new(960, 540, 960, 540)),
        ];
        l
    }

    /// A different layout: 2×1 (two halves stacked) — every rect differs
    /// from `layout_v1`.
    fn layout_v2() -> Layout {
        let mut l = Layout::new(LayoutKind::Grid);
        l.version = 2;
        l.slots =
            vec![Slot::new(0, None, Rect::new(0, 0, 1920, 540)), Slot::new(1, None, Rect::new(0, 540, 1920, 540))];
        l
    }

    #[test]
    fn static_layout_yields_stable_rects() {
        let mut bridge = LayoutBridge::new();
        let l1 = layout_v1();
        bridge.apply(&l1);
        // No change in progress: every elapsed gives the same rects.
        let a = bridge.current(0);
        let b = bridge.current(150);
        let c = bridge.current(300);
        let d = bridge.current(10_000);
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(c, d);
        // And they match the layout's own rects.
        assert_eq!(a.get(&0).unwrap(), &PixelRect::new(0, 0, 960, 540));
        assert_eq!(a.get(&3).unwrap(), &PixelRect::new(960, 540, 960, 540));
    }

    #[test]
    fn crossfade_midpoint_is_interpolated() {
        let mut bridge = LayoutBridge::new();
        bridge.apply(&layout_v1());
        // let the first layout settle
        assert!(bridge.current(300) == bridge.current(400));
        bridge.apply(&layout_v2());

        // t=0 → previous (v1) rects.
        assert_eq!(bridge.current(0).get(&0).unwrap(), &PixelRect::new(0, 0, 960, 540));

        // t=150 → midpoint between v1 and v2 for slot 0:
        // v1 (0,0,960,540) → v2 (0,0,1920,540): mid w = 1440.
        let mid = bridge.current(150);
        let s0 = mid.get(&0).unwrap();
        assert_eq!(s0.x, 0);
        assert_eq!(s0.y, 0);
        assert_eq!(s0.w, 1440, "midpoint width between 960 and 1920");
        assert_eq!(s0.h, 540);

        // Slot 2 disappears in v2 → fades out: at t=150 it is half-sized.
        let s2 = mid.get(&2).unwrap();
        assert_eq!(s2.w, 480, "disappearing slot at midpoint is half width");
        assert_eq!(s2.h, 270);
    }

    #[test]
    fn crossfade_snap_after_window() {
        let mut bridge = LayoutBridge::new();
        bridge.apply(&layout_v1());
        bridge.apply(&layout_v2());
        // t >= 300 → exactly v2, no fade remnants.
        let after = bridge.current(300);
        assert_eq!(after.get(&0).unwrap(), &PixelRect::new(0, 0, 1920, 540));
        assert_eq!(after.get(&1).unwrap(), &PixelRect::new(0, 540, 1920, 540));
        assert!(!after.contains_key(&2), "slot 2 fully gone after fade");
        assert!(!after.contains_key(&3));
        // Same at any later time.
        assert_eq!(bridge.current(301), after);
        assert_eq!(bridge.current(10_000), after);
    }

    #[test]
    fn crossfade_monotonic_toward_target() {
        let mut bridge = LayoutBridge::new();
        bridge.apply(&layout_v1());
        bridge.apply(&layout_v2());
        // Width of slot 0 must monotonically approach 1920 over time.
        let w0 = |t: u64| bridge.current(t).get(&0).map(|r| r.w);
        assert!(w0(0) < w0(75));
        assert!(w0(75) < w0(150));
        assert!(w0(150) < w0(225));
        assert!(w0(225) < w0(300));
        assert_eq!(w0(300), Some(1920));
    }

    #[test]
    fn first_apply_no_crossfade() {
        let mut bridge = LayoutBridge::new();
        bridge.apply(&layout_v2());
        // previous is empty → snaps to current immediately.
        assert_eq!(bridge.current(0).get(&0).unwrap(), &PixelRect::new(0, 0, 1920, 540));
    }

    #[test]
    fn custom_crossfade_window() {
        let mut bridge = LayoutBridge::with_crossfade_ms(100);
        bridge.apply(&layout_v1());
        bridge.apply(&layout_v2());
        // t=50 of a 100 ms window: mid width 1440.
        assert_eq!(bridge.current(50).get(&0).unwrap().w, 1440);
        // t=100 → snap.
        assert_eq!(bridge.current(100).get(&0).unwrap().w, 1920);
    }
}
