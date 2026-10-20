//! Desktop window via eframe (macOS, Linux, Windows) for development.

use crate::app::KioskApp;

struct DesktopApp(KioskApp);

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

pub fn run(app: KioskApp) -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 720.0]).with_title("kiosk-ui"),
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
