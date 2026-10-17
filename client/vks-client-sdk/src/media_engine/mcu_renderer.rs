//! MCU renderer: отображение ОДНОГО композитного трека (сервер смешал).
//!
//! MCU — клиент получает 1 трек (уже mixed); renderer кладёт его
//! на всю канву. Основание: `docs/architecture/ARCHITECTURE.md` §242
//! (mcu_renderer.rs), [FRONTEND_RULES.md §7](docs/rules/FRONTEND_RULES.md).

use crate::render_surface::DrawCommand;

/// MCU renderer: всегда 1 DrawCommand на всю канву.
#[derive(Debug, Clone, Default)]
pub struct McuRenderer;

impl McuRenderer {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Композитный трек → 1 DrawCommand на всю канву.
    #[must_use]
    pub fn compose(&self, size: (u32, u32)) -> Vec<DrawCommand> {
        vec![DrawCommand { participant_id: "composite".into(), rect: (0, 0, size.0, size.1), scale: 1.0 }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_one_command() {
        let r = McuRenderer::new();
        let cmds = r.compose((1280, 720));
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].participant_id, "composite");
        assert_eq!(cmds[0].rect, (0, 0, 1280, 720));
    }

    #[test]
    fn independent_of_participant_count() {
        // MCU не зависит от N участников — всегда 1 трек
        let r = McuRenderer::new();
        assert_eq!(r.compose((100, 100)).len(), 1);
        assert_eq!(r.compose((0, 0)).len(), 1);
    }
}
