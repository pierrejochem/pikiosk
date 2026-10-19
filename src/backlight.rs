//! Display backlight through /sys/class/backlight (e.g. the attiny
//! controller on Waveshare / official Pi DSI panels). The kiosk user gets
//! write access through a udev rule in the image.

use std::path::PathBuf;

pub struct Backlight {
    path: PathBuf,
    max: u32,
}

impl Backlight {
    /// First backlight device, or `KIOSK_BACKLIGHT` (a directory with
    /// `brightness` and `max_brightness`, handy for testing).
    pub fn find() -> Option<Self> {
        let dir = match std::env::var_os("KIOSK_BACKLIGHT") {
            Some(d) => PathBuf::from(d),
            None => {
                let mut entries: Vec<_> = std::fs::read_dir("/sys/class/backlight").ok()?.flatten().map(|e| e.path()).collect();
                entries.sort();
                entries.into_iter().next()?
            }
        };
        let max = std::fs::read_to_string(dir.join("max_brightness")).ok()?.trim().parse::<u32>().ok()?;
        if max == 0 {
            return None;
        }
        log::info!("backlight: {} (max {max})", dir.display());
        Some(Self { path: dir.join("brightness"), max })
    }

    /// 0..1; never fully dark, so the screen can always be seen to turn it back up.
    pub fn set(&self, level: f32) {
        let v = ((0.05 + 0.95 * level.clamp(0.0, 1.0)) * self.max as f32).round() as u32;
        if let Err(e) = std::fs::write(&self.path, v.to_string()) {
            log::warn!("backlight {}: {e}", self.path.display());
        }
    }
}
