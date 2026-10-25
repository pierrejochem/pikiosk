//! The Apps page and the store behind it: installed apps as glass tiles, then,
//! after the admin PIN, the catalog, an app's detail and the install progress.
//! kiosk-ui installs nothing itself; it sends requests to kioskd over a Unix
//! socket and draws what comes back. The PIN is checked by kioskd; kiosk-ui only
//! holds the short-lived token kioskd gives out for a correct one.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Shape, Stroke, TextureHandle, Ui, pos2, vec2};
use kiosk_kit::Lay;
use kiosk_kit::glass::Glass;
use kiosk_kit::widgets::{self as w, Icon, Sc, text};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: Option<String>,
    #[serde(default)]
    pub previous: Option<String>,
}

/// An app in the signed store's catalog.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CatalogApp {
    pub id: String,
    pub name: String,
    pub version: String,
    pub category: String,
    pub size: u64,
    #[serde(default)]
    pub permissions: Vec<String>,
    pub installed: Option<String>,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub boards: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct StoreInfo {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub channel: String,
    pub synced_at: Option<u64>,
    #[serde(default)]
    pub apps: Vec<CatalogApp>,
}

/// An update that failed its health check and was rolled back.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Notice {
    pub id: String,
    pub name: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct AdminInfo {
    #[serde(default)]
    pub pin_set: bool,
    #[serde(default)]
    pub locked_for: u64,
    #[serde(default)]
    pub tries_left: u32,
    #[serde(default)]
    pub lockout: u64,
}

#[derive(Clone, Debug, Deserialize, Default)]
pub struct Progress {
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub done: u64,
    #[serde(default)]
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LocalPackage {
    pub file: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub installed: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Overview {
    allow_unsigned: bool,
    apps: Vec<InstalledApp>,
    local: Vec<LocalPackage>,
    #[serde(default)]
    store: StoreInfo,
    #[serde(default)]
    notices: Vec<Notice>,
    #[serde(default)]
    admin: AdminInfo,
}

#[derive(Deserialize)]
pub(crate) struct Reply {
    pub ok: bool,
    pub error: Option<String>,
    pub data: Option<serde_json::Value>,
}

pub(crate) enum Event {
    Overview(Overview),
    /// the request worked; refresh the lists
    Done,
    Failed(String),
    /// kioskd did not answer
    Unreachable,
    Unlocked(String),
    WrongPin { tries_left: u32 },
    Locked { secs: u64 },
    Progress(Progress),
    WifiScan(crate::wifi::ScanData),
    WifiScanFailed(String),
    /// a network was joined or forgotten
    WifiDone,
    WifiFailed(String),
}

#[derive(Clone, PartialEq, Debug)]
pub enum View {
    Apps,
    Catalog,
    /// the app with this id
    Detail(String),
}

/// Sends requests on short-lived threads and wakes the UI with the answer.
pub(crate) struct Client {
    sock: PathBuf,
    tx: Sender<Event>,
    wake: Option<Arc<std::os::fd::OwnedFd>>,
}

#[cfg(target_os = "linux")]
fn make_wake() -> Option<Arc<std::os::fd::OwnedFd>> {
    use std::os::fd::FromRawFd;
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    (fd >= 0).then(|| Arc::new(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
}

#[cfg(not(target_os = "linux"))]
fn make_wake() -> Option<Arc<std::os::fd::OwnedFd>> {
    None
}

fn call(sock: &PathBuf, req: &serde_json::Value) -> Result<Reply, String> {
    let mut s = UnixStream::connect(sock).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(120))).map_err(|e| e.to_string())?;
    writeln!(s, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| e.to_string())
}

impl Client {
    pub(crate) fn send(&self, ctx: &egui::Context, req: serde_json::Value, done: fn(Reply) -> Event) {
        let (sock, tx, wake, ctx) = (self.sock.clone(), self.tx.clone(), self.wake.clone(), ctx.clone());
        std::thread::spawn(move || {
            let ev = match call(&sock, &req) {
                Ok(r) => done(r),
                Err(e) => {
                    log::warn!("kioskd: {e}");
                    Event::Unreachable
                }
            };
            let _ = tx.send(ev);
            if let Some(fd) = wake {
                use std::os::fd::AsRawFd;
                let one = 1u64;
                unsafe { libc::write(fd.as_raw_fd(), &one as *const u64 as *const _, 8) };
            }
            ctx.request_repaint();
        });
    }
}

fn failed(r: Reply) -> Event {
    Event::Failed(r.error.unwrap_or_else(|| "request failed".into()))
}

/// Where one tile sits in the grid.
struct Slot {
    row: usize,
    col: usize,
    span: usize,
    item: Item,
}

pub(crate) enum Item {
    App(usize),
    /// the tile that opens the store (behind the admin PIN)
    Store,
}

struct Grid {
    left: f32,
    top: f32,
    tile_w: f32,
    tile_h: f32,
    gap: f32,
    cols: usize,
    rows_visible: usize,
}

impl Grid {
    fn new(l: &Lay, area: Rect, bar_top: f32, tile_h: f32) -> Grid {
        let margin = l.v(80.0, 24.0);
        let cols = if l.compact { 4 } else { 5 };
        let gap = l.v(18.0, 14.0);
        let top = l.v(150.0, 92.0);
        let tile_w = ((area.width() - 2.0 * margin).min(l.v(1100.0, 760.0)) - gap * (cols - 1) as f32) / cols as f32;
        let width = tile_w * cols as f32 + gap * (cols - 1) as f32;
        let rows_visible = (((bar_top - top - l.v(6.0, 6.0) + gap) / (tile_h + gap)) as usize).max(1);
        Grid { left: area.center().x - width * 0.5, top, tile_w, tile_h, gap, cols, rows_visible }
    }

    fn rect(&self, row: usize, col: usize, span: usize, first_row: usize) -> Rect {
        let w = self.tile_w * span as f32 + self.gap * (span - 1) as f32;
        Rect::from_min_size(
            pos2(self.left + col as f32 * (self.tile_w + self.gap), self.top + (row - first_row) as f32 * (self.tile_h + self.gap)),
            vec2(w, self.tile_h),
        )
    }
}

fn tile_color(id: &str) -> Color32 {
    const COLORS: [(u8, u8, u8); 6] = [(242, 169, 59), (47, 163, 107), (214, 69, 127), (90, 95, 224), (28, 154, 160), (138, 79, 216)];
    let h = id.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
    let (r, g, b) = COLORS[h as usize % COLORS.len()];
    Color32::from_rgb(r, g, b)
}

fn dashed_outline(p: &egui::Painter, r: Rect, radius: f32, color: Color32) {
    let _ = radius;
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    p.extend(Shape::dashed_line(&pts, Stroke::new(1.5, color), 7.0, 5.0));
}


/// The admin PIN dialog.
pub(crate) struct Pin {
    pub digits: String,
    /// tries left after a wrong PIN
    pub wrong: Option<u32>,
    /// time (egui seconds) at which the lockout ends
    pub locked_until: Option<f64>,
    /// an unlock request is on its way
    pub waiting: bool,
    /// where to go after a correct PIN
    pub then: View,
}

pub(crate) struct Job {
    pub id: String,
    pub name: String,
    pub update: bool,
}

pub struct Store {
    pub(crate) client: Client,
    rx: Receiver<Event>,
    pub apps: Vec<InstalledApp>,
    pub local: Vec<LocalPackage>,
    pub allow_unsigned: bool,
    /// None until kioskd has answered once
    pub reachable: Option<bool>,
    pub info: StoreInfo,
    pub notices: Vec<Notice>,
    pub admin: AdminInfo,
    pub(crate) busy: Option<String>,
    pub(crate) toast: Option<(String, f64)>,
    pub view: View,
    pub(crate) row: usize,
    vdrag: f32,
    icons: HashMap<String, Option<TextureHandle>>,
    /// admin session: the token from kioskd and when it ends (egui seconds)
    pub token: Option<String>,
    pub(crate) admin_until: f64,
    last_keepalive: f64,
    pub(crate) pin: Option<Pin>,
    pub(crate) tab: usize,
    pub(crate) job: Option<Job>,
    pub(crate) progress: Progress,
    progress_asked: f64,
    progress_inflight: bool,
    pub(crate) confirm_remove: Option<(String, f64)>,
    pub(crate) wifi: crate::wifi::Wifi,
}

/// The admin session ends this long after the last touch.
const SESSION_SECS: f64 = 60.0;

impl Store {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let sock = std::env::var_os("KIOSKD_SOCK").map(PathBuf::from).unwrap_or_else(|| "/run/kioskd/kioskd.sock".into());
        Store {
            client: Client { sock, tx, wake: make_wake() },
            rx,
            apps: Vec::new(),
            local: Vec::new(),
            allow_unsigned: false,
            reachable: None,
            info: StoreInfo::default(),
            notices: Vec::new(),
            admin: AdminInfo::default(),
            busy: None,
            toast: None,
            view: View::Apps,
            row: 0,
            vdrag: 0.0,
            icons: HashMap::new(),
            token: None,
            admin_until: 0.0,
            last_keepalive: 0.0,
            pin: None,
            tab: 0,
            job: None,
            progress: Progress::default(),
            progress_asked: 0.0,
            progress_inflight: false,
            confirm_remove: None,
            wifi: crate::wifi::Wifi::default(),
        }
    }

    pub fn wake_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.client.wake.as_ref().map(|f| f.as_raw_fd())
    }

    /// Asks kioskd for the installed apps, the catalog and the notices.
    pub fn refresh(&mut self, ctx: &egui::Context) {
        self.client.send(ctx, serde_json::json!({"cmd": "overview"}), |r| match (r.ok, r.data.clone()) {
            (true, Some(d)) => serde_json::from_value(d).map(Event::Overview).unwrap_or_else(|e| Event::Failed(e.to_string())),
            _ => failed(r),
        });
    }

    pub fn back_to_grid(&mut self) {
        self.view = View::Apps;
        self.row = 0;
    }

    /// A dialog covers the page: the tab bar steps aside.
    pub fn modal(&self) -> bool {
        self.pin.is_some() || (self.view == View::Apps && !self.notices.is_empty())
    }

    pub(crate) fn is_admin(&self) -> bool {
        self.token.is_some()
    }

    /// Adds the admin token to a privileged request.
    pub(crate) fn authed(&self, mut req: serde_json::Value) -> serde_json::Value {
        req["token"] = serde_json::json!(self.token);
        req
    }

    pub(crate) fn lock(&mut self, ctx: &egui::Context) {
        if self.token.take().is_some() {
            self.client.send(ctx, serde_json::json!({"cmd": "lock"}), |_| Event::Done);
        }
        self.pin = None;
        if self.view != View::Apps {
            self.back_to_grid();
        }
    }

    /// Goes to , asking for the PIN first unless the admin session is open.
    pub(crate) fn begin_admin(&mut self, then: View, time: f64) {
        if self.is_admin() {
            self.view = then;
            self.row = 0;
            self.tab = 0;
            return;
        }
        let locked_until = (self.admin.locked_for > 0).then(|| time + self.admin.locked_for as f64);
        self.pin = Some(Pin { digits: String::new(), wrong: None, locked_until, waiting: false, then });
    }

    pub(crate) fn submit_pin(&mut self, ctx: &egui::Context) {
        let Some(pin) = &mut self.pin else { return };
        pin.waiting = true;
        let digits = pin.digits.clone();
        self.client.send(ctx, serde_json::json!({"cmd": "unlock", "pin": digits}), |r| {
            let d = r.data.clone().unwrap_or_default();
            if r.ok {
                Event::Unlocked(d["token"].as_str().unwrap_or_default().to_string())
            } else if let Some(secs) = d["locked_for"].as_u64() {
                Event::Locked { secs }
            } else if let Some(tries_left) = d["tries_left"].as_u64() {
                Event::WrongPin { tries_left: tries_left as u32 }
            } else {
                failed(r)
            }
        });
    }

    /// Takes the answers that arrived since the last frame, and keeps the admin session and install progress going.
    pub fn poll(&mut self, ctx: &egui::Context, time: f64) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Overview(o) => {
                    self.reachable = Some(true);
                    self.allow_unsigned = o.allow_unsigned;
                    self.apps = o.apps;
                    self.local = o.local;
                    self.info = o.store;
                    self.notices = o.notices;
                    self.admin = o.admin;
                    self.busy = None;
                    if self.local.is_empty() && !self.info.enabled && self.view == View::Catalog {
                        self.back_to_grid();
                    }
                    for a in &self.apps {
                        self.icons.entry(a.id.clone()).or_insert_with(|| load_icon(ctx, a));
                    }
                    self.icons.retain(|id, _| self.apps.iter().any(|a| a.id == *id));
                }
                Event::Done => {
                    self.busy = None;
                    self.job = None;
                    self.refresh(ctx);
                }
                Event::Failed(e) => {
                    self.reachable = Some(true);
                    self.busy = None;
                    self.job = None;
                    // a session that ended on kioskd's side is over here too
                    if e.contains("admin PIN") || e.contains("session") {
                        self.token = None;
                    }
                    self.toast = Some((e, time + 6.0));
                    self.refresh(ctx);
                }
                Event::Unreachable => {
                    self.reachable = Some(false);
                    self.busy = None;
                    self.job = None;
                    self.wifi.unreachable();
                }
                Event::Unlocked(token) => {
                    self.token = Some(token);
                    self.admin_until = time + SESSION_SECS;
                    self.last_keepalive = time;
                    if let Some(pin) = self.pin.take() {
                        self.view = pin.then;
                        self.row = 0;
                        self.tab = 0;
                    }
                }
                Event::WrongPin { tries_left } => {
                    if let Some(pin) = &mut self.pin {
                        pin.digits.clear();
                        pin.waiting = false;
                        pin.wrong = Some(tries_left);
                    }
                }
                Event::Locked { secs } => {
                    if let Some(pin) = &mut self.pin {
                        pin.digits.clear();
                        pin.waiting = false;
                        pin.wrong = None;
                        pin.locked_until = Some(time + secs as f64);
                    }
                }
                Event::Progress(p) => {
                    self.progress_inflight = false;
                    self.progress = p;
                }
                Event::WifiScan(d) => self.wifi.scanned(d),
                Event::WifiScanFailed(e) => self.wifi.scan_failed(e),
                Event::WifiDone => self.wifi.done(time),
                Event::WifiFailed(e) => {
                    // a session that ended on kioskd's side is over here too
                    if e.contains("admin PIN") || e.contains("session") {
                        self.token = None;
                    }
                    self.wifi.failed(e);
                }
            }
        }
        if self.toast.as_ref().is_some_and(|(_, until)| time > *until) {
            self.toast = None;
        }
        if self.confirm_remove.as_ref().is_some_and(|(_, until)| time > *until) {
            self.confirm_remove = None;
        }
        if let Some(pin) = &mut self.pin
            && pin.locked_until.is_some_and(|t| time >= t)
        {
            pin.locked_until = None;
            pin.wrong = None;
        }

        // the admin session: every touch extends it, kioskd is told now and then, and it ends after a minute of quiet
        if self.token.is_some() {
            if ctx.input(|i| i.pointer.any_down() || i.pointer.any_pressed()) {
                self.admin_until = time + SESSION_SECS;
            }
            if time > self.admin_until && self.job.is_none() {
                self.lock(ctx);
            } else if time - self.last_keepalive > 10.0 && self.admin_until > self.last_keepalive + 10.0 {
                self.last_keepalive = time;
                let t = self.token.clone().unwrap_or_default();
                self.client.send(ctx, serde_json::json!({"cmd": "keepalive", "token": t}), |r| if r.ok { Event::Done } else { failed(r) });
            }
        }
        // while an install runs, ask for its progress a few times a second
        if self.job.is_some() && !self.progress_inflight && time - self.progress_asked > 0.3 {
            self.progress_asked = time;
            self.progress_inflight = true;
            self.client.send(ctx, serde_json::json!({"cmd": "progress"}), |r| match (r.ok, r.data) {
                (true, Some(d)) => serde_json::from_value(d).map(Event::Progress).unwrap_or(Event::Progress(Progress::default())),
                _ => Event::Progress(Progress::default()),
            });
        }
    }

    fn slots(&self, g: &Grid) -> Vec<Slot> {
        let mut items: Vec<(Item, usize)> = (0..self.apps.len()).map(|i| (Item::App(i), 1)).collect();
        if self.info.enabled || (self.allow_unsigned && !self.local.is_empty()) {
            items.push((Item::Store, 2.min(g.cols)));
        }
        let (mut row, mut col) = (0, 0);
        let mut out = Vec::new();
        for (item, span) in items {
            if col + span > g.cols {
                row += 1;
                col = 0;
            }
            out.push(Slot { row, col, span, item });
            col += span;
        }
        out
    }

    /// Rows move one at a time when the page is swiped up or down.
    pub(crate) fn swipe_rows(&mut self, r: &egui::Response, l: &Lay, total_rows: usize, visible: usize) {
        if r.dragged() {
            self.vdrag += r.drag_delta().y;
        }
        if r.drag_stopped() {
            let threshold = l.v(60.0, 40.0);
            let max = total_rows.saturating_sub(visible);
            if self.vdrag < -threshold {
                self.row = (self.row + 1).min(max);
            } else if self.vdrag > threshold {
                self.row = self.row.saturating_sub(1);
            }
            self.vdrag = 0.0;
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, bar_top: f32, time: f64) {
        let p = ui.painter().clone();
        let modal = self.pin.is_some() || (self.view == View::Apps && !self.notices.is_empty());
        let sc_in = Sc { enabled: sc.enabled && self.busy.is_none() && !modal, ..sc };
        let margin = l.v(80.0, 24.0);
        let hy = l.v(104.0, 62.0);

        if self.reachable == Some(false) {
            text(&p, pos2(area.left() + margin, hy), Align2::LEFT_CENTER, "Apps", l.v(40.0, 28.0), sc.white(1.0));
            self.message(&p, l, sc, area, bar_top, "Apps are not available", "The app launcher (kioskd) is not running.");
        } else if self.reachable.is_some() {
            match self.view.clone() {
                View::Apps => {
                    text(&p, pos2(area.left() + margin, hy), Align2::LEFT_CENTER, "Apps", l.v(40.0, 28.0), sc.white(1.0));
                    let s = format!("{} installed", self.apps.len());
                    text(&p, pos2(area.right() - margin, hy + l.v(4.0, 3.0)), Align2::RIGHT_CENTER, &s, l.v(18.0, 13.0), sc.white(0.8));
                    self.grid(ui, l, sc_in, area, bar_top, time);
                }
                View::Catalog => self.catalog_view(ui, l, sc_in, area, bar_top, time),
                View::Detail(id) => self.detail_view(ui, l, sc_in, area, time, &id),
            }
        }

        if self.pin.is_some() {
            self.pin_modal(ui, l, sc, area, time);
        } else if self.view == View::Apps && !self.notices.is_empty() {
            self.notice_dialog(ui, l, sc, area);
        }

        if let Some(busy) = &self.busy {
            let c = pos2(area.center().x, bar_top - l.v(30.0, 22.0));
            let r = Rect::from_center_size(c, vec2(w::text_width(&p, busy, l.v(20.0, 16.0)) + l.v(60.0, 44.0), l.v(48.0, 38.0)));
            sc.glass(&p, r, Glass::control(r.height() * 0.5).tinted(w::ACCENT, 0.5));
            text(&p, r.center(), Align2::CENTER_CENTER, busy, l.v(20.0, 16.0), sc.white(1.0));
        } else if let Some((e, _)) = &self.toast {
            let c = pos2(area.center().x, bar_top - l.v(30.0, 22.0));
            let r = Rect::from_center_size(c, vec2((w::text_width(&p, e, l.v(18.0, 14.0)) + l.v(60.0, 44.0)).min(area.width() - 2.0 * margin), l.v(48.0, 38.0)));
            sc.glass(&p, r, Glass::control(r.height() * 0.5).tinted(Color32::from_rgb(255, 69, 58), 0.55));
            text(&p, r.center(), Align2::CENTER_CENTER, e, l.v(18.0, 14.0), sc.white(1.0));
        }
        // the lockout countdown and the session timer tick once a second
        if self.pin.as_ref().is_some_and(|p| p.locked_until.is_some()) || self.token.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    pub(crate) fn message(&self, p: &egui::Painter, l: &Lay, sc: Sc, area: Rect, bar_top: f32, title: &str, detail: &str) {
        let r = Rect::from_center_size(
            pos2(area.center().x, (l.v(150.0, 92.0) + bar_top) * 0.5),
            vec2((area.width() - 2.0 * l.v(80.0, 24.0)).min(l.v(700.0, 560.0)), l.v(150.0, 110.0)),
        );
        sc.glass(p, r, Glass::panel(l.v(30.0, 22.0)));
        text(p, pos2(r.center().x, r.center().y - l.v(18.0, 14.0)), Align2::CENTER_CENTER, title, l.v(26.0, 20.0), sc.white(1.0));
        text(p, pos2(r.center().x, r.center().y + l.v(22.0, 16.0)), Align2::CENTER_CENTER, detail, l.v(17.0, 13.0), sc.white(0.75));
    }

    fn grid(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, bar_top: f32, time: f64) {
        let p = ui.painter().clone();
        let g = Grid::new(l, area, bar_top, l.v(170.0, 132.0));
        let slots = self.slots(&g);
        let total_rows = slots.last().map_or(0, |s| s.row + 1);
        self.row = self.row.min(total_rows.saturating_sub(g.rows_visible));

        if slots.is_empty() {
            self.message(&p, l, sc, area, bar_top, "No apps installed", "Apps from the store appear here.");
            return;
        }
        let mut launch = None;
        let mut open_store = false;
        let mut dragged = None;
        for s in &slots {
            if s.row < self.row || s.row >= self.row + g.rows_visible {
                continue;
            }
            let r = g.rect(s.row, s.col, s.span, self.row);
            let id = match s.item {
                Item::App(i) => Id::unique(("app", &self.apps[i].id)),
                Item::Store => Id::unique("store-tile"),
            };
            let resp = ui.interact(r, id, if sc.enabled { Sense::click_and_drag() } else { Sense::hover() });
            let press = ui.ctx().animate_bool_with_time(id.with("press"), resp.is_pointer_button_down_on() && !resp.dragged(), 0.1);
            let rr = r.shrink(l.v(3.0, 2.0) * press);
            let pad = l.v(20.0, 16.0);
            match s.item {
                Item::App(i) => {
                    let a = &self.apps[i];
                    sc.glass(&p, rr, Glass::panel(l.v(26.0, 18.0)).highlight(1.0 + 0.6 * press));
                    let side = l.v(60.0, 48.0);
                    let ib = Rect::from_min_size(rr.min + vec2(pad, pad), vec2(side, side));
                    match self.icons.get(&a.id).and_then(|t| t.as_ref()) {
                        Some(tex) => {
                            egui::Image::new((tex.id(), ib.size()))
                                .corner_radius(CornerRadius::same((side * 0.27) as u8))
                                .tint(Color32::from_white_alpha((255.0 * sc.alpha) as u8))
                                .paint_at(ui, ib);
                        }
                        None => initial_tile(&p, sc, ib, &a.id, &a.name),
                    }
                    let fit = |s: &str, size: f32| size * ((rr.width() - 2.0 * pad) / w::text_width(&p, s, size)).min(1.0);
                    let ns = fit(&a.name, l.v(21.0, 16.0));
                    text(&p, pos2(rr.left() + pad, ib.bottom() + l.v(26.0, 20.0)), Align2::LEFT_CENTER, &a.name, ns, sc.white(1.0));
                    text(&p, pos2(rr.left() + pad, ib.bottom() + l.v(52.0, 40.0)), Align2::LEFT_CENTER, &a.version, l.v(15.0, 11.0), sc.white(0.78));
                }
                Item::Store => {
                    sc.glass(&p, rr, Glass::panel(l.v(26.0, 18.0)).frost(0.3).opacity(0.5));
                    dashed_outline(&p, rr, l.v(26.0, 18.0), sc.white(0.5));
                    let side = l.v(60.0, 48.0);
                    let ib = Rect::from_center_size(pos2(rr.left() + pad + side * 0.5, rr.center().y), vec2(side, side));
                    p.rect_filled(ib, CornerRadius::same((side * 0.27) as u8), sc.white(0.18));
                    w::icon(&p, Icon::Lock, ib.center(), side * 0.5, sc.white(1.0));
                    let tx = ib.right() + l.v(18.0, 14.0);
                    text(&p, pos2(tx, rr.center().y - l.v(12.0, 9.0)), Align2::LEFT_CENTER, "Store", l.v(21.0, 16.0), sc.white(1.0));
                    text(&p, pos2(tx, rr.center().y + l.v(14.0, 11.0)), Align2::LEFT_CENTER, "Install and update apps · admin PIN", l.v(15.0, 12.0), sc.white(0.8));
                }
            }
            if resp.clicked() {
                match s.item {
                    Item::App(i) => launch = Some(i),
                    Item::Store => open_store = true,
                }
            }
            if resp.dragged() || resp.drag_stopped() {
                dragged = Some(resp);
            }
        }
        if let Some(r) = dragged {
            self.swipe_rows(&r, l, total_rows, g.rows_visible);
        }
        if total_rows > g.rows_visible {
            self.row_dots(&p, l, sc, area, bar_top, total_rows, g.rows_visible);
        }
        if open_store {
            self.begin_admin(View::Catalog, time);
        }
        if let Some(i) = launch {
            self.launch(ui.ctx(), i);
        }
    }

    pub(crate) fn launch(&mut self, ctx: &egui::Context, i: usize) {
        let a = self.apps[i].clone();
        self.busy = Some(format!("Starting {}…", a.name));
        self.client.send(ctx, serde_json::json!({"cmd": "launch", "id": a.id}), |r| if r.ok { Event::Done } else { failed(r) });
    }

    fn row_dots(&self, p: &egui::Painter, l: &Lay, sc: Sc, area: Rect, bar_top: f32, total: usize, visible: usize) {
        let pages = total - visible + 1;
        for i in 0..pages {
            let c = pos2(area.right() - l.v(40.0, 14.0), (l.v(150.0, 92.0) + bar_top) * 0.5 + (i as f32 - (pages - 1) as f32 * 0.5) * l.v(18.0, 14.0));
            p.circle_filled(c, l.v(4.0, 3.0), sc.white(if i == self.row { 0.95 } else { 0.35 }));
        }
    }

    /// Centre of the nth tile in the grid view (for tests).
    #[cfg(test)]
    pub fn tile_center(&self, l: &Lay, area: Rect, bar_top: f32, n: usize) -> egui::Pos2 {
        let g = Grid::new(l, area, bar_top, l.v(170.0, 132.0));
        let s = &self.slots(&g)[n];
        g.rect(s.row, s.col, s.span, self.row).center()
    }
}

/// A coloured tile with the app's first letter, for apps without an icon.
pub(crate) fn initial_tile(p: &egui::Painter, sc: Sc, ib: Rect, id: &str, name: &str) {
    let side = ib.height();
    p.rect_filled(ib, CornerRadius::same((side * 0.27) as u8), sc.fade(tile_color(id)));
    let initial = name.chars().next().unwrap_or('?').to_uppercase().to_string();
    text(p, ib.center(), Align2::CENTER_CENTER, &initial, side * 0.5, sc.white(1.0));
}

fn load_icon(ctx: &egui::Context, app: &InstalledApp) -> Option<TextureHandle> {
    let path = app.icon.as_ref()?;
    // kioskd checks icons when it installs them; this is the second line of defence,
    // because a decoder crash here would crash-loop the kiosk
    if std::fs::metadata(path).ok()?.len() > 256 << 10 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    if w > 512 || h > 512 {
        return None;
    }
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ctx.load_texture(format!("app-icon-{}", app.id), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR))
}
