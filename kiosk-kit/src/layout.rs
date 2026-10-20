use egui::Rect;

/// Layout metrics. Two designs: the regular one is drawn for 1280x720 and
/// scales with the screen height; the compact one is drawn for 800x480
/// touch panels (bigger type and touch targets relative to the screen).
pub struct Lay {
    pub compact: bool,
    /// design units -> points
    pub k: f32,
    /// scale for the widgets' own proportions
    pub wu: f32,
}

impl Lay {
    pub fn new(screen: Rect) -> Self {
        let compact = screen.height() < 600.0 || screen.width() < 1000.0;
        let k = if compact {
            (screen.height() / 480.0).min(screen.width() / 800.0)
        } else {
            (screen.height() / 720.0).clamp(0.5, 3.0)
        };
        Self { compact, k, wu: if compact { 0.86 * k } else { k } }
    }

    /// pick the regular or compact design value
    pub fn v(&self, regular: f32, compact: f32) -> f32 {
        if self.compact { compact * self.k } else { regular * self.k }
    }
}
