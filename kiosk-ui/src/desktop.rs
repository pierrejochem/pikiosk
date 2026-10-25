//! Desktop window via eframe (macOS, Linux, Windows) for development.

use crate::app::KioskApp;

struct DesktopApp(KioskApp);

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

/// The window size: `KIOSK_WINDOW=800x480` shows the compact layout of the 5" panel.
fn window_size() -> [f32; 2] {
    let parsed = std::env::var("KIOSK_WINDOW").ok().and_then(|s| {
        let (w, h) = s.split_once('x')?;
        Some([w.parse().ok()?, h.parse().ok()?])
    });
    parsed.unwrap_or([1280.0, 720.0])
}

pub fn run(app: KioskApp) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(window_size()).with_title("kiosk-ui"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "kiosk-ui",
        options,
        Box::new(|cc| {
            cc.egui_ctx.set_theme(egui::Theme::Dark);
            Ok(Box::new(DesktopApp(app)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
