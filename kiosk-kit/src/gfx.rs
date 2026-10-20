//! One egui frame on a raw GL context (used by the KMS and headless backends;
//! the desktop backend lets eframe do this).

use std::sync::Arc;
use std::time::Duration;

use egui::{Pos2, RawInput, Rect, ViewportId, vec2};
use glow::HasContext as _;

use crate::App;

pub struct Gfx {
    pub ctx: egui::Context,
    pub painter: egui_glow::Painter,
    pub gl: Arc<glow::Context>,
}

impl Gfx {
    pub fn new(gl: Arc<glow::Context>) -> anyhow::Result<Self> {
        unsafe {
            log::info!(
                "GL: {} / {}",
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VERSION)
            );
        }
        let painter = egui_glow::Painter::new(gl.clone(), "", None, true).map_err(|e| anyhow::anyhow!("egui painter: {e}"))?;
        let ctx = egui::Context::default();
        ctx.options_mut(|o| {
            o.theme_preference = egui::ThemePreference::Dark;
            // resistive panels jitter 8-13 px under a fingertip during one tap; with
            // egui's default of 6 such taps turn into drags and never click
            o.input_options.max_click_dist = 20.0;
        });
        Ok(Self { ctx, painter, gl })
    }

    /// Runs and paints one frame into the currently bound framebuffer.
    /// Returns how long egui is happy to wait before the next frame.
    pub fn frame(&mut self, app: &mut dyn App, size_px: [u32; 2], ppp: f32, events: Vec<egui::Event>, time: f64) -> Duration {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(size_px[0] as f32, size_px[1] as f32) / ppp)),
            time: Some(time),
            events,
            focused: true,
            max_texture_side: Some(self.painter.max_texture_side()),
            ..Default::default()
        };
        raw.viewports.entry(ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);

        let mut out = self.ctx.run_ui(raw, |ui| app.ui(ui));
        let prims = self.ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);
        unsafe {
            self.gl.viewport(0, 0, size_px[0] as i32, size_px[1] as i32);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.painter.paint_and_update_textures(size_px, out.pixels_per_point, &prims, &mut out.textures_delta);
        out.viewport_output.get(&ViewportId::ROOT).map(|v| v.repaint_delay).unwrap_or(Duration::MAX)
    }
}

impl Drop for Gfx {
    fn drop(&mut self) {
        self.painter.destroy();
    }
}

/// Points-per-pixel for a screen: `KIOSK_SCALE` or ~720 logical rows.
pub fn pixels_per_point(height_px: u32) -> f32 {
    std::env::var("KIOSK_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| *s > 0.1)
        .unwrap_or_else(|| (height_px as f32 / 720.0).clamp(1.0, 3.0))
}
