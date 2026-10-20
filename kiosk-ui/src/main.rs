//! kiosk-ui: fullscreen egui kiosk app with liquid-glass widgets.
//!
//!   kiosk-ui                         fullscreen on the display (DRM/KMS, Linux)
//!   kiosk-ui --window                in a desktop window (build with --features desktop)
//!   kiosk-ui --screenshot out.png [--size 1280x720] [--page home|apps|network|controls|settings|all]
//!
//! Environment: KIOSK_DATA (settings + wallpaper.jpg), KIOSK_WALLPAPER,
//! KIOSK_THEME, KIOSK_SCALE, KIOSK_FPS (default 30), KIOSK_MODE (e.g. 1280x720).

mod app;
mod backlight;
mod store;
mod sysinfo;

#[cfg(target_os = "linux")]
mod headless;

#[cfg(feature = "desktop")]
mod desktop;

use app::KioskApp;

fn usage() -> ! {
    eprintln!(
        "usage: kiosk-ui [--window] [--screenshot FILE.png [--size WxH] [--page home|apps|network|controls|settings|all]]"
    );
    std::process::exit(2);
}

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut args = std::env::args().skip(1);
    let mut window = false;
    let mut shot: Option<std::path::PathBuf> = None;
    let mut size = [1280u32, 720];
    let mut pages = vec![0usize];
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
                    Some("home") => vec![0],
                    Some("apps") => vec![1],
                    Some("network") => vec![2],
                    Some("controls") => vec![3],
                    Some("settings") => vec![4],
                    Some("all") => vec![0, 1, 2, 3, 4],
                    _ => usage(),
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
    return kiosk_kit::run(app);
    #[cfg(not(target_os = "linux"))]
    unreachable!()
}
