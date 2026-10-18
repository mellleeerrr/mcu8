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
    /// Раскладывает участников согласно канонической раскладке [`vks_layout::Layout`] (M0.4 / M3.2).
    /// Учитывает z-index (отрисовка от нижнего к верхнему), pinned и фокус.
    #[must_use]
    pub fn compose_from_layout(&self, layout: &vks_layout::Layout) -> Vec<DrawCommand> {
        let mut slots = layout.slots.clone();
        slots.sort_by_key(|s| s.z_index);
        slots
            .into_iter()
            .filter_map(|s| {
                let pid = s.participant_id?;
                let u = pid.as_u128();
                let pid_str = uuid::Uuid::from_u128(u).to_string();
                Some(DrawCommand {
                    participant_id: pid_str,
                    rect: (s.rect.x, s.rect.y, s.rect.w, s.rect.h),
                    scale: 1.0,
                })
            })
            .collect()
    }

    /// Раскладывает участников на канвасе `size` согласно [`vks_layout::NormalizedLayout`] (M0.4 / M3.2).
    #[must_use]
    pub fn compose_from_normalized(&self, norm: &vks_layout::NormalizedLayout, size: (u32, u32)) -> Vec<DrawCommand> {
        let canvas = vks_layout::Canvas::new(size.0, size.1);
        let layout = norm.scale_to_canvas(&canvas);
        self.compose_from_layout(&layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_layout::{Canvas, Layout, LayoutKind, NormalizedLayout, Rect, Slot};

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

    #[test]
    fn compose_from_layout_with_arbitrary_slot_ids_7_and_42() {
        let r = SfuRenderer::new();
        let p_a = uuid::Uuid::from_u128(101);
        let p_b = uuid::Uuid::from_u128(202);

        let mut layout = Layout::new(LayoutKind::Grid);
        // Slot 7: p_a, Slot 42: p_b
        layout.slots.push(Slot::with_attributes(
            7,
            Some(vks_layout::ParticipantId::new(p_a.as_u128())),
            Rect::new(0, 0, 960, 1080),
            0,
            false,
            false,
        ));
        layout.slots.push(Slot::with_attributes(
            42,
            Some(vks_layout::ParticipantId::new(p_b.as_u128())),
            Rect::new(960, 0, 960, 1080),
            1,
            false,
            false,
        ));

        let cmds = r.compose_from_layout(&layout);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].participant_id, p_a.to_string());
        assert_eq!(cmds[0].rect, (0, 0, 960, 1080));
        assert_eq!(cmds[1].participant_id, p_b.to_string());
        assert_eq!(cmds[1].rect, (960, 0, 960, 1080));
    }

    #[test]
    fn compose_from_normalized_proportions_across_canvases() {
        let r = SfuRenderer::new();
        let p_a = uuid::Uuid::from_u128(1);
        let p_b = uuid::Uuid::from_u128(2);

        // Layout on 1920x1080
        let mut mcu_layout = Layout::new(LayoutKind::Grid);
        mcu_layout.slots.push(Slot::new(
            0,
            Some(vks_layout::ParticipantId::new(p_a.as_u128())),
            Rect::new(0, 0, 960, 1080),
        ));
        mcu_layout.slots.push(Slot::new(
            1,
            Some(vks_layout::ParticipantId::new(p_b.as_u128())),
            Rect::new(960, 0, 960, 1080),
        ));

        let canvas_mcu = Canvas::new(1920, 1080);
        let norm: NormalizedLayout = mcu_layout.to_normalized(&canvas_mcu);

        // Render on client 640x360 canvas
        let sdk_cmds = r.compose_from_normalized(&norm, (640, 360));
        assert_eq!(sdk_cmds.len(), 2);
        // Each tile must occupy exactly half width (320px) and full height (360px)
        assert_eq!(sdk_cmds[0].participant_id, p_a.to_string());
        assert_eq!(sdk_cmds[0].rect, (0, 0, 320, 360));
        assert_eq!(sdk_cmds[1].participant_id, p_b.to_string());
        assert_eq!(sdk_cmds[1].rect, (320, 0, 320, 360));
    }
}
