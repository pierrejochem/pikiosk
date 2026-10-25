//! kiosk-ui: fullscreen egui kiosk app with liquid-glass widgets.
//!
//!   kiosk-ui                         fullscreen on the display (DRM/KMS, Linux)
//!   kiosk-ui --window                in a desktop window (build with --features desktop)
//!   kiosk-ui --screenshot out.png [--size 1280x720] [--page home|apps|display|network|touch|system|all]
//!
//! Environment: KIOSK_DATA (settings + wallpaper.jpg), KIOSK_WALLPAPER,
//! KIOSK_THEME, KIOSK_SCALE, KIOSK_FPS (default 30), KIOSK_MODE (e.g. 1280x720).

mod app;
mod backlight;
mod catalog;
mod keyboard;
mod store;
mod sysinfo;
mod wifi;

#[cfg(target_os = "linux")]
mod headless;

#[cfg(feature = "desktop")]
mod desktop;

use app::KioskApp;

/// What `--page` can show: (page, Settings section, name).
const SHOTS: [(usize, usize, &str); 6] = [
    (0, 0, "home"),
    (app::APPS_PAGE, 0, "apps"),
    (app::SETTINGS_PAGE, app::DISPLAY_SECTION, "display"),
    (app::SETTINGS_PAGE, app::NETWORK_SECTION, "network"),
    (app::SETTINGS_PAGE, app::TOUCH_SECTION, "touch"),
    (app::SETTINGS_PAGE, app::SYSTEM_SECTION, "system"),
];

fn usage() -> ! {
    eprintln!(
        "usage: kiosk-ui [--window] [--screenshot FILE.png [--size WxH] [--page home|apps|display|network|touch|system|all]]"
    );
    std::process::exit(2);
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut args = std::env::args().skip(1);
    let mut window = false;
    let mut shot: Option<std::path::PathBuf> = None;
    let mut size = [1280u32, 720];
    let mut pages = vec![SHOTS[0]];
    while let Some(a) = args.next() {
        match a.as_str() {
            "--window" => window = true,
            "--screenshot" => shot = Some(args.next().unwrap_or_else(|| usage()).into()),
            "--size" => {
                let s = args.next().unwrap_or_else(|| usage());
                let (w, h) = s.split_once('x').unwrap_or_else(|| usage());
                size = [w.parse().unwrap_or_else(|_| usage()), h.parse().unwrap_or_else(|_| usage())];
            }
            "--page" => {
                pages = match args.next().as_deref() {
                    Some("all") => SHOTS.to_vec(),
                    // "controls" and "settings" are the old names of the Display section
                    Some("controls" | "settings") => vec![SHOTS[2]],
                    Some(name) => vec![*SHOTS.iter().find(|s| s.2 == name).unwrap_or_else(|| usage())],
                    None => usage(),
                }
            }
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    let _ = (&size, &pages);

    let mut app = KioskApp::new();
    // systemd restarts the service
    app.on_restart = Some(Box::new(|| {
        log::info!("restart requested");
        std::process::exit(0);
    }));

    if let Some(path) = shot {
        #[cfg(target_os = "linux")]
        return headless::screenshot(app, &path, size, &pages);
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("--screenshot needs Linux/Mesa; on macOS use --window ({})", path.display());
    }

    if window || !cfg!(target_os = "linux") {
        #[cfg(feature = "desktop")]
        return desktop::run(app);
        #[cfg(not(feature = "desktop"))]
        anyhow::bail!("built without the desktop window; rebuild with: cargo run --features desktop -- --window");
    }

    #[cfg(target_os = "linux")]
    return kiosk_kit::run(app.with_fade_in());
    #[cfg(not(target_os = "linux"))]
    unreachable!()
}
