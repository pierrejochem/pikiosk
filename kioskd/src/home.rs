//! The Home gesture: while an app runs, holding a finger in the top-right
//! corner of the screen for three seconds stops the app, so kiosk-ui comes
//! back even when the app hangs.
//!
//! The touchscreen is read straight from evdev, read-only and without
//! grabbing it, so the app keeps its own touch input.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::launch;

// Linux input event codes
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_SYN: u16 = 0x00;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;

/// The corner is this many pixels wide and tall.
const CORNER_PX: f32 = 64.0;

/// A 3x2 matrix in libinput's form: x' = a*x + b*y + c, y' = d*x + e*y + f,
/// on coordinates scaled to 0..1.
pub type Matrix = [f32; 6];
pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

pub fn apply(m: Matrix, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5])
}

fn parse_matrix(text: &str) -> Option<Matrix> {
    let v: Vec<f32> = text.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    v.try_into().ok()
}

/// The calibration kiosk-ui uses: the one saved on the device, else the
/// image's udev default, else none.
pub fn calibration() -> Matrix {
    let conf = std::env::var("KIOSKD_UI_CONF").unwrap_or_else(|_| "/data/app/kiosk-ui.conf".into());
    if let Ok(text) = std::fs::read_to_string(conf)
        && let Some(m) = text.lines().find_map(|l| parse_matrix(l.strip_prefix("touch_matrix=")?))
    {
        return m;
    }
    let rules = std::env::var("KIOSKD_UDEV_RULE").unwrap_or_else(|_| "/etc/udev/rules.d/99-touch-calibration.rules".into());
    if let Ok(text) = std::fs::read_to_string(rules)
        && let Some(m) = text.lines().find_map(|l| {
            let rest = l.split("LIBINPUT_CALIBRATION_MATRIX=\"").nth(1)?;
            parse_matrix(rest.split('"').next()?)
        })
    {
        return m;
    }
    IDENTITY
}

/// The screen size in pixels: KIOSKD_DISPLAY, else the first mode of a
/// connected output.
fn display_size() -> Option<(f32, f32)> {
    let parse = |s: &str| -> Option<(f32, f32)> {
        let (w, h) = s.trim().split_once('x')?;
        Some((w.parse().ok()?, h.split(['i', 'p']).next()?.parse().ok()?))
    };
    if let Ok(v) = std::env::var("KIOSKD_DISPLAY") {
        return parse(&v);
    }
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let dir = entry.path();
        if std::fs::read_to_string(dir.join("status")).is_ok_and(|s| s.trim() == "connected")
            && let Ok(modes) = std::fs::read_to_string(dir.join("modes"))
            && let Some(size) = modes.lines().next().and_then(parse)
        {
            return Some(size);
        }
    }
    None
}

/// True when a touch at (x, y) in 0..1 raw coordinates is in the Home corner.
pub fn in_corner(m: Matrix, x: f32, y: f32, (w, h): (f32, f32)) -> bool {
    let (x, y) = apply(m, x, y);
    x >= 1.0 - CORNER_PX / w && y <= CORNER_PX / h
}

/// Tracks one press: it fires once after being held in the corner long enough.
#[derive(Default)]
pub struct Hold {
    since: Option<Instant>,
    fired: bool,
}

impl Hold {
    pub fn update(&mut self, touching: bool, in_corner: bool, now: Instant) {
        if !touching {
            *self = Hold::default();
        } else if !in_corner {
            self.since = None;
        } else if self.since.is_none() && !self.fired {
            self.since = Some(now);
        }
    }

    /// True once, when the press has lasted `hold`.
    pub fn fired(&mut self, hold: Duration, now: Instant) -> bool {
        match self.since {
            Some(t) if now.duration_since(t) >= hold => {
                self.since = None;
                self.fired = true;
                true
            }
            _ => false,
        }
    }

    pub fn deadline(&self, hold: Duration) -> Option<Instant> {
        self.since.map(|t| t + hold)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    sec: i64,
    usec: i64,
    kind: u16,
    code: u16,
    value: i32,
}

#[repr(C)]
#[derive(Default)]
struct AbsInfo {
    value: i32,
    min: i32,
    max: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

struct Touch {
    file: File,
    path: PathBuf,
    x: AbsInfo,
    y: AbsInfo,
    pos: (f32, f32),
    touching: bool,
}

fn ioctl_read<T>(fd: i32, nr: u32, buf: &mut T) -> bool {
    let size = std::mem::size_of::<T>();
    // _IOC(_IOC_READ, 'E', nr, size)
    let req = (2u32 << 30) | ((size as u32) << 16) | (0x45 << 8) | nr;
    unsafe { libc::ioctl(fd, req as _, buf as *mut T) >= 0 }
}

fn has_bit(bits: &[u8], bit: u16) -> bool {
    bits.get(bit as usize / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
}

/// Opens every input device that is a touchscreen.
fn find_touchscreens() -> Vec<Touch> {
    let mut found = Vec::new();
    let Ok(dir) = std::fs::read_dir("/dev/input") else { return found };
    for entry in dir.flatten() {
        let path = entry.path();
        if !path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event")) {
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&path) else { continue };
        let fd = file.as_raw_fd();
        let mut keys = [0u8; 96];
        let mut abs = [0u8; 8];
        if !ioctl_read(fd, 0x20 + EV_KEY as u32, &mut keys) || !ioctl_read(fd, 0x20 + EV_ABS as u32, &mut abs) {
            continue;
        }
        let (cx, cy) = if has_bit(&abs, ABS_X) && has_bit(&abs, ABS_Y) {
            (ABS_X, ABS_Y)
        } else if has_bit(&abs, ABS_MT_POSITION_X) && has_bit(&abs, ABS_MT_POSITION_Y) {
            (ABS_MT_POSITION_X, ABS_MT_POSITION_Y)
        } else {
            continue;
        };
        if !has_bit(&keys, BTN_TOUCH) {
            continue;
        }
        let (mut x, mut y) = (AbsInfo::default(), AbsInfo::default());
        if !ioctl_read(fd, 0x40 + cx as u32, &mut x)
            || !ioctl_read(fd, 0x40 + cy as u32, &mut y)
            || x.max <= x.min
            || y.max <= y.min
        {
            continue;
        }
        log::info!("home gesture: watching {}", path.display());
        found.push(Touch { file, path, x, y, pos: (0.0, 0.0), touching: false });
    }
    found
}

impl Touch {
    fn norm(&self, raw: i32, a: &AbsInfo) -> f32 {
        ((raw - a.min) as f32 / (a.max - a.min) as f32).clamp(0.0, 1.0)
    }

    /// Reads pending events; returns true when a frame (SYN_REPORT) completed.
    fn read(&mut self) -> std::io::Result<bool> {
        let mut frame = false;
        loop {
            let mut ev = InputEvent::default();
            let n = unsafe { libc::read(self.file.as_raw_fd(), &mut ev as *mut _ as *mut _, std::mem::size_of::<InputEvent>()) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                return if e.kind() == std::io::ErrorKind::WouldBlock { Ok(frame) } else { Err(e) };
            }
            if n as usize != std::mem::size_of::<InputEvent>() {
                return Ok(frame);
            }
            match (ev.kind, ev.code) {
                (EV_ABS, ABS_X | ABS_MT_POSITION_X) => self.pos.0 = self.norm(ev.value, &self.x),
                (EV_ABS, ABS_Y | ABS_MT_POSITION_Y) => self.pos.1 = self.norm(ev.value, &self.y),
                (EV_KEY, BTN_TOUCH) => self.touching = ev.value != 0,
                (EV_SYN, 0) => frame = true,
                _ => {}
            }
        }
    }
}

/// Runs forever on its own thread.
pub fn watch() {
    let hold_for = Duration::from_millis(std::env::var("KIOSKD_HOME_HOLD_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(3000));
    let mut devices: Vec<Touch> = Vec::new();
    let mut hold = Hold::default();
    let mut last_scan = Instant::now() - Duration::from_secs(60);
    loop {
        // the touchscreen is USB: look again until one shows up, and after it goes away
        if devices.is_empty() && last_scan.elapsed() >= Duration::from_secs(5) {
            devices = find_touchscreens();
            last_scan = Instant::now();
        }
        let timeout = match hold.deadline(hold_for) {
            Some(t) => t.saturating_duration_since(Instant::now()).as_millis() as i32 + 1,
            None if devices.is_empty() => 5000,
            None => 60_000,
        };
        let mut pfds: Vec<libc::pollfd> = devices.iter().map(|d| libc::pollfd { fd: d.file.as_raw_fd(), events: libc::POLLIN, revents: 0 }).collect();
        if pfds.is_empty() {
            std::thread::sleep(Duration::from_millis(timeout as u64));
            continue;
        }
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout) };

        let mut lost = Vec::new();
        for (i, d) in devices.iter_mut().enumerate() {
            if pfds[i].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                lost.push(i);
                continue;
            }
            if pfds[i].revents & libc::POLLIN == 0 {
                continue;
            }
            match d.read() {
                Ok(true) | Ok(false) => {
                    let corner = display_size().is_some_and(|s| in_corner(calibration(), d.pos.0, d.pos.1, s));
                    hold.update(d.touching, corner, Instant::now());
                }
                Err(e) => {
                    log::warn!("{}: {e}", d.path.display());
                    lost.push(i);
                }
            }
        }
        for i in lost.into_iter().rev() {
            devices.remove(i);
        }
        if hold.fired(hold_for, Instant::now()) {
            if launch::running_apps().is_empty() {
                continue;
            }
            log::info!("home gesture: stopping the app");
            if let Err(e) = launch::stop() {
                log::warn!("home gesture: {e:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: (f32, f32) = (800.0, 480.0);

    #[test]
    fn corner_is_top_right_64_px() {
        // 760/800 = 0.95 from the left, 30/480 = 0.0625 from the top
        assert!(in_corner(IDENTITY, 0.95, 0.0625, SCREEN));
        assert!(!in_corner(IDENTITY, 0.5, 0.05, SCREEN), "middle top");
        assert!(!in_corner(IDENTITY, 0.99, 0.5, SCREEN), "middle right");
        assert!(!in_corner(IDENTITY, 0.05, 0.05, SCREEN), "top left");
    }

    #[test]
    fn calibration_moves_the_corner() {
        // raw touches sit 5% too far left: the matrix shifts them right
        let m = [1.0, 0.0, 0.05, 0.0, 1.0, 0.0];
        assert!(!in_corner(IDENTITY, 0.91, 0.02, SCREEN));
        assert!(in_corner(m, 0.91, 0.02, SCREEN));
    }

    #[test]
    fn matrix_is_read_from_text() {
        assert_eq!(parse_matrix("1 0 0.5 0 1 0"), Some([1.0, 0.0, 0.5, 0.0, 1.0, 0.0]));
        assert_eq!(parse_matrix("1 0 0.5"), None);
    }

    #[test]
    fn hold_fires_once_after_the_time() {
        let t0 = Instant::now();
        let s = |n: u64| t0 + Duration::from_secs(n);
        let hold = Duration::from_secs(3);
        let mut h = Hold::default();
        h.update(true, true, s(0));
        assert!(!h.fired(hold, s(2)));
        assert!(h.fired(hold, s(3)));
        assert!(!h.fired(hold, s(4)), "only once per press");
        h.update(true, true, s(5));
        assert!(!h.fired(hold, s(9)), "needs a release first");
        h.update(false, false, s(10));
        h.update(true, true, s(11));
        assert!(h.fired(hold, s(14)), "armed again after release");
    }

    #[test]
    fn leaving_the_corner_cancels_the_hold() {
        let t0 = Instant::now();
        let hold = Duration::from_secs(3);
        let mut h = Hold::default();
        h.update(true, true, t0);
        h.update(true, false, t0 + Duration::from_secs(2));
        assert!(!h.fired(hold, t0 + Duration::from_secs(4)));
        h.update(true, true, t0 + Duration::from_secs(5));
        assert!(!h.fired(hold, t0 + Duration::from_secs(6)), "the clock restarted");
    }
}
