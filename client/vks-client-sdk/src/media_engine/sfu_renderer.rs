//! SFU renderer: микширование N отдельных треков через `vks-layout`.
//!
//! SFU — клиент получает N треков; renderer раскладывает их через
//! `vks_layout::grid::GridLayout` (или focus). Основание:
//! `docs/architecture/ARCHITECTURE.md` §241 (sfu_renderer.rs),
//! `docs/rules/FRONTEND_RULES.md` §7 (VideoGrid).

use crate::render_surface::DrawCommand;

/// SFU renderer: N участников → N DrawCommand (по grid-layout).
#[derive(Debug, Clone, Default)]
pub struct SfuRenderer;

impl SfuRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Раскладывает `participants` по grid-layout на канве `size`.
    #[must_use]
    pub fn compose(&self, participants: &[String], size: (u32, u32)) -> Vec<DrawCommand> {
        // Через vks-layout: GridLayout::compute(&Canvas, n) -> Vec<Rect>.
        let canvas = vks_layout::Canvas::new(size.0, size.1);
        let rects = vks_layout::GridLayout::compute(&canvas, participants.len());
        participants
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let (x, y, w, h) = match rects.get(i) {
                    Some(r) => (r.x, r.y, r.w, r.h),
                    None => (0, 0, 0, 0),
                };
                DrawCommand { participant_id: p.clone(), rect: (x, y, w, h), scale: 1.0 }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_participant_fills_canvas() {
        let r = SfuRenderer::new();
        let cmds = r.compose(&["a".into()], (640, 480));
        assert_eq!(cmds.len(), 1);
        // 1 участник → вся канва
        assert_eq!(cmds[0].rect, (0, 0, 640, 480));
    }

    #[test]
    fn two_participants_split() {
        let r = SfuRenderer::new();
        let cmds = r.compose(&["a".into(), "b".into()], (640, 480));
        assert_eq!(cmds.len(), 2);
        // 2 → две половины (по ширине или высоте)
        let total: u32 = cmds.iter().map(|c| c.rect.2 * c.rect.3).sum();
        assert_eq!(total, 640 * 480);
    }

    #[test]
    fn four_participants_grid_2x2() {
        let r = SfuRenderer::new();
        let cmds = r.compose(&["a".into(), "b".into(), "c".into(), "d".into()], (640, 480));
        assert_eq!(cmds.len(), 4);
        // каждый 320x240
        for c in &cmds {
            assert_eq!(c.rect.2 * c.rect.3, 320 * 240);
        }
        // нет overlap (каждый rect в своём квадранте)
        let xs: Vec<u32> = cmds.iter().map(|c| c.rect.0).collect();
        assert!(xs.contains(&0) && xs.contains(&320));
    }

    #[test]
    fn empty_participants_no_commands() {
        let r = SfuRenderer::new();
        let cmds = r.compose(&[], (640, 480));
        assert!(cmds.is_empty());
    }
}
