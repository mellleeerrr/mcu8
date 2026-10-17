//! Абстракция рендер-поверхности (canvas / element / offscreen).
//!
//! SDK не знает, куда рендерит (брау canvas, canvas, Dcanvas) — только
//! *что* рендерит (DrawCommand от media_engine). Основание:
//! `docs/architecture/ARCHITECTURE.md` §233 (render_surface.rs).

/// Рендер-команда: нарисовать `participant_id` в `rect` (px, normalized 0..1 или px —
/// определяется поверхностью).
#[derive(Debug, Clone, PartialEq)]
pub struct DrawCommand {
    /// Участник (track id).
    pub participant_id: String,
    /// Прямоугольник (px).
    pub rect: (u32, u32, u32, u32),
    /// Масштаб (1.0 = 1:1).
    pub scale: f32,
}

/// Рендер-поверхность — цель для DrawCommand.
pub trait RenderSurface: Send + Sync {
    /// Рендерит один кадр (список команд).
    fn render(&self, commands: &[DrawCommand]);
    /// Размер поверхности (px).
    fn size(&self) -> (u32, u32);
}

/// Null-поверхность (для тестов + headless).
#[derive(Debug, Default)]
pub struct NullSurface;

impl RenderSurface for NullSurface {
    fn render(&self, _commands: &[DrawCommand]) {}
    fn size(&self) -> (u32, u32) {
        (0, 0)
    }
}

/// Поверхность-записчик: собирает команды (для unit-тестов / инспекции).
#[derive(Debug, Default)]
pub struct RecordingSurface {
    inner: std::sync::Arc<std::sync::Mutex<Vec<DrawCommand>>>,
}

impl RecordingSurface {
    #[must_use]
    pub fn new() -> Self {
        Self { inner: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())) }
    }

    /// Снимает накопленные команды.
    #[must_use]
    pub fn take(&self) -> Vec<DrawCommand> {
        self.inner.lock().unwrap().drain(..).collect()
    }
}

impl RenderSurface for RecordingSurface {
    fn render(&self, commands: &[DrawCommand]) {
        self.inner.lock().unwrap().extend_from_slice(commands);
    }
    fn size(&self) -> (u32, u32) {
        (640, 480)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_surface_renders() {
        let s = NullSurface;
        s.render(&[DrawCommand { participant_id: "p".into(), rect: (0, 0, 10, 10), scale: 1.0 }]);
        assert_eq!(s.size(), (0, 0));
    }

    #[test]
    fn recording_surface_collects_commands() {
        let s = RecordingSurface::new();
        s.render(&[
            DrawCommand { participant_id: "a".into(), rect: (0, 0, 1, 1), scale: 1.0 },
            DrawCommand { participant_id: "b".into(), rect: (1, 1, 2, 2), scale: 0.5 },
        ]);
        let cmds = s.take();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].participant_id, "a");
        // take() освобождает
        assert!(s.take().is_empty());
    }
}
