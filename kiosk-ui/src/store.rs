//! The Apps page: the installed apps as glass tiles, and the packages found
//! on local media. kiosk-ui installs nothing itself; it sends requests to
//! kioskd over a Unix socket and draws what comes back.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Shape, Stroke, TextureHandle, Ui, pos2, vec2};
use kiosk_kit::Lay;
use kiosk_kit::glass::Glass;
use kiosk_kit::widgets::{self as w, GREEN, Icon, Sc, text};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct InstalledApp {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: Option<String>,
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
}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    error: Option<String>,
    data: Option<serde_json::Value>,
}

enum Event {
    Overview(Overview),
    /// the request worked; refresh the lists
    Done,
    Failed(String),
    /// kioskd did not answer
    Unreachable,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Apps,
    Local,
}

/// Sends requests on short-lived threads and wakes the UI with the answer.
struct Client {
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
    fn send(&self, ctx: &egui::Context, req: serde_json::Value, done: fn(Reply) -> Event) {
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

pub struct Store {
    client: Client,
    rx: Receiver<Event>,
    pub apps: Vec<InstalledApp>,
    pub local: Vec<LocalPackage>,
    pub allow_unsigned: bool,
    /// None until kioskd has answered once
    pub reachable: Option<bool>,
    busy: Option<String>,
    notice: Option<(String, f64)>,
    view: View,
    row: usize,
    vdrag: f32,
    icons: HashMap<String, Option<TextureHandle>>,
}

/// Where one tile sits in the grid.
struct Slot {
    row: usize,
    col: usize,
    span: usize,
    item: Item,
}

enum Item {
    App(usize),
    Local,
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

impl Store {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let sock = std::env::var_os("KIOSKD_SOCK").map(PathBuf::from).unwrap_or_else(|| "/run/kioskd.sock".into());
        Store {
            client: Client { sock, tx, wake: make_wake() },
            rx,
            apps: Vec::new(),
            local: Vec::new(),
            allow_unsigned: false,
            reachable: None,
            busy: None,
            notice: None,
            view: View::Apps,
            row: 0,
            vdrag: 0.0,
            icons: HashMap::new(),
        }
    }

    pub fn wake_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.client.wake.as_ref().map(|f| f.as_raw_fd())
    }

    /// Asks kioskd for the installed apps and the packages on local media.
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

    /// Takes the answers that arrived since the last frame.
    pub fn poll(&mut self, ctx: &egui::Context, time: f64) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Overview(o) => {
                    self.reachable = Some(true);
                    self.allow_unsigned = o.allow_unsigned;
                    self.apps = o.apps;
                    self.local = o.local;
                    self.busy = None;
                    if self.local.is_empty() {
                        self.view = View::Apps;
                    }
                    for a in &self.apps {
                        self.icons.entry(a.id.clone()).or_insert_with(|| load_icon(ctx, a));
                    }
                    self.icons.retain(|id, _| self.apps.iter().any(|a| a.id == *id));
                }
                Event::Done => {
                    self.busy = None;
                    self.refresh(ctx);
                }
                Event::Failed(e) => {
                    self.reachable = Some(true);
                    self.busy = None;
                    self.notice = Some((e, time + 6.0));
                }
                Event::Unreachable => {
                    self.reachable = Some(false);
                    self.busy = None;
                }
            }
        }
        if self.notice.as_ref().is_some_and(|(_, until)| time > *until) {
            self.notice = None;
        }
    }

    fn slots(&self, g: &Grid) -> Vec<Slot> {
        let mut items: Vec<(Item, usize)> = (0..self.apps.len()).map(|i| (Item::App(i), 1)).collect();
        if self.allow_unsigned && !self.local.is_empty() {
            items.push((Item::Local, 2.min(g.cols)));
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
    fn swipe_rows(&mut self, r: &egui::Response, l: &Lay, total_rows: usize, visible: usize) {
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

    pub fn ui(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, bar_top: f32) {
        let p = ui.painter().clone();
        let margin = l.v(80.0, 24.0);
        let hy = l.v(104.0, 62.0);
        let sc_in = Sc { enabled: sc.enabled && self.busy.is_none(), ..sc };

        match self.view {
            View::Apps => {
                text(&p, pos2(area.left() + margin, hy), Align2::LEFT_CENTER, "Apps", l.v(40.0, 28.0), sc.white(1.0));
                if self.reachable == Some(true) {
                    let s = format!("{} installed", self.apps.len());
                    text(&p, pos2(area.right() - margin, hy + l.v(4.0, 3.0)), Align2::RIGHT_CENTER, &s, l.v(18.0, 13.0), sc.white(0.8));
                }
            }
            View::Local => {
                text(&p, pos2(area.left() + margin, hy), Align2::LEFT_CENTER, "Local packages", l.v(40.0, 28.0), sc.white(1.0));
                let back = Rect::from_center_size(pos2(area.right() - margin - l.v(55.0, 45.0), hy), vec2(l.v(110.0, 90.0), l.v(44.0, 34.0)));
                if w::button(ui, sc_in, back, Id::unique("store-back"), "Back", None).clicked() {
                    self.back_to_grid();
                }
            }
        }

        if self.reachable == Some(false) {
            self.message(&p, l, sc, area, bar_top, "Apps are not available", "The app launcher (kioskd) is not running.");
        } else if self.reachable.is_none() {
            // first answer pending
        } else if self.view == View::Local {
            self.local_list(ui, l, sc_in, area, bar_top);
        } else {
            self.grid(ui, l, sc_in, area, bar_top);
        }

        if let Some(busy) = &self.busy {
            let c = pos2(area.center().x, bar_top - l.v(30.0, 22.0));
            let r = Rect::from_center_size(c, vec2(w::text_width(&p, busy, l.v(20.0, 16.0)) + l.v(60.0, 44.0), l.v(48.0, 38.0)));
            sc.glass(&p, r, Glass::control(r.height() * 0.5).tinted(w::ACCENT, 0.5));
            text(&p, r.center(), Align2::CENTER_CENTER, busy, l.v(20.0, 16.0), sc.white(1.0));
        } else if let Some((e, _)) = &self.notice {
            let c = pos2(area.center().x, bar_top - l.v(30.0, 22.0));
            let r = Rect::from_center_size(c, vec2((w::text_width(&p, e, l.v(18.0, 14.0)) + l.v(60.0, 44.0)).min(area.width() - 2.0 * margin), l.v(48.0, 38.0)));
            sc.glass(&p, r, Glass::control(r.height() * 0.5).tinted(Color32::from_rgb(255, 69, 58), 0.55));
            text(&p, r.center(), Align2::CENTER_CENTER, e, l.v(18.0, 14.0), sc.white(1.0));
        }
    }

    fn message(&self, p: &egui::Painter, l: &Lay, sc: Sc, area: Rect, bar_top: f32, title: &str, detail: &str) {
        let r = Rect::from_center_size(
            pos2(area.center().x, (l.v(150.0, 92.0) + bar_top) * 0.5),
            vec2((area.width() - 2.0 * l.v(80.0, 24.0)).min(l.v(700.0, 560.0)), l.v(150.0, 110.0)),
        );
        sc.glass(p, r, Glass::panel(l.v(30.0, 22.0)));
        text(p, pos2(r.center().x, r.center().y - l.v(18.0, 14.0)), Align2::CENTER_CENTER, title, l.v(26.0, 20.0), sc.white(1.0));
        text(p, pos2(r.center().x, r.center().y + l.v(22.0, 16.0)), Align2::CENTER_CENTER, detail, l.v(17.0, 13.0), sc.white(0.75));
    }

    fn grid(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, bar_top: f32) {
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
        let mut open_local = false;
        let mut dragged = None;
        for s in &slots {
            if s.row < self.row || s.row >= self.row + g.rows_visible {
                continue;
            }
            let r = g.rect(s.row, s.col, s.span, self.row);
            let (id, sense) = match s.item {
                Item::App(i) => (Id::unique(("app", &self.apps[i].id)), Sense::click_and_drag()),
                Item::Local => (Id::unique("local-tile"), Sense::click_and_drag()),
            };
            let resp = ui.interact(r, id, if sc.enabled { sense } else { Sense::hover() });
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
                        None => {
                            p.rect_filled(ib, CornerRadius::same((side * 0.27) as u8), sc.fade(tile_color(&a.id)));
                            let initial = a.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                            text(&p, ib.center(), Align2::CENTER_CENTER, &initial, side * 0.5, sc.white(1.0));
                        }
                    }
                    let fit = |s: &str, size: f32| size * ((rr.width() - 2.0 * pad) / w::text_width(&p, s, size)).min(1.0);
                    let ns = fit(&a.name, l.v(21.0, 16.0));
                    text(&p, pos2(rr.left() + pad, ib.bottom() + l.v(26.0, 20.0)), Align2::LEFT_CENTER, &a.name, ns, sc.white(1.0));
                    text(&p, pos2(rr.left() + pad, ib.bottom() + l.v(52.0, 40.0)), Align2::LEFT_CENTER, &a.version, l.v(15.0, 11.0), sc.white(0.78));
                }
                Item::Local => {
                    sc.glass(&p, rr, Glass::panel(l.v(26.0, 18.0)).frost(0.3).opacity(0.5));
                    dashed_outline(&p, rr, l.v(26.0, 18.0), sc.white(0.5));
                    let side = l.v(60.0, 48.0);
                    let ib = Rect::from_center_size(pos2(rr.left() + pad + side * 0.5, rr.center().y), vec2(side, side));
                    p.rect_filled(ib, CornerRadius::same((side * 0.27) as u8), sc.white(0.18));
                    w::icon(&p, Icon::Lock, ib.center(), side * 0.5, sc.white(1.0));
                    let tx = ib.right() + l.v(18.0, 14.0);
                    text(&p, pos2(tx, rr.center().y - l.v(12.0, 9.0)), Align2::LEFT_CENTER, "Local packages", l.v(21.0, 16.0), sc.white(1.0));
                    let sub = format!("{} on the SD card · install", self.local.len());
                    text(&p, pos2(tx, rr.center().y + l.v(14.0, 11.0)), Align2::LEFT_CENTER, &sub, l.v(15.0, 12.0), sc.white(0.8));
                }
            }
            if resp.clicked() {
                match s.item {
                    Item::App(i) => launch = Some(i),
                    Item::Local => open_local = true,
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
        if open_local {
            self.view = View::Local;
            self.row = 0;
        }
        if let Some(i) = launch {
            let a = self.apps[i].clone();
            self.busy = Some(format!("Starting {}…", a.name));
            self.client.send(ui.ctx(), serde_json::json!({"cmd": "launch", "id": a.id}), |r| if r.ok { Event::Done } else { failed(r) });
        }
    }

    fn row_dots(&self, p: &egui::Painter, l: &Lay, sc: Sc, area: Rect, bar_top: f32, total: usize, visible: usize) {
        let pages = total - visible + 1;
        for i in 0..pages {
            let c = pos2(area.right() - l.v(40.0, 14.0), (l.v(150.0, 92.0) + bar_top) * 0.5 + (i as f32 - (pages - 1) as f32 * 0.5) * l.v(18.0, 14.0));
            p.circle_filled(c, l.v(4.0, 3.0), sc.white(if i == self.row { 0.95 } else { 0.35 }));
        }
    }

    fn local_list(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, bar_top: f32) {
        let p = ui.painter().clone();
        let margin = l.v(80.0, 24.0);
        let top = l.v(150.0, 92.0);
        let row_h = l.v(92.0, 66.0);
        let gap = l.v(12.0, 10.0);
        let visible = (((bar_top - top - 6.0 + gap) / (row_h + gap)) as usize).max(1);
        let cw = (area.width() - 2.0 * margin).min(l.v(1100.0, 760.0));
        let left = area.center().x - cw * 0.5;
        let total = self.local.len();
        self.row = self.row.min(total.saturating_sub(visible));

        let mut install = None;
        let mut dragged = None;
        for (n, pkg) in self.local.iter().enumerate().skip(self.row).take(visible) {
            let r = Rect::from_min_size(pos2(left, top + (n - self.row) as f32 * (row_h + gap)), vec2(cw, row_h));
            let bg = ui.interact(r, Id::unique(("pkg-row", n)), if sc.enabled { Sense::drag() } else { Sense::hover() });
            if bg.dragged() || bg.drag_stopped() {
                dragged = Some(bg);
            }
            sc.glass(&p, r, Glass::panel(l.v(24.0, 18.0)));
            let pad = l.v(24.0, 18.0);
            text(&p, pos2(r.left() + pad, r.center().y - l.v(13.0, 10.0)), Align2::LEFT_CENTER, &pkg.name, l.v(22.0, 17.0), sc.white(1.0));
            let sub = match &pkg.installed {
                Some(v) if *v == pkg.version => format!("{} · installed", pkg.version),
                Some(v) => format!("{} · installed {v}", pkg.version),
                None => pkg.version.clone(),
            };
            text(&p, pos2(r.left() + pad, r.center().y + l.v(15.0, 12.0)), Align2::LEFT_CENTER, &sub, l.v(15.0, 12.0), sc.white(0.72));
            let bw = l.v(140.0, 108.0);
            let br = Rect::from_center_size(pos2(r.right() - pad - bw * 0.5, r.center().y), vec2(bw, l.v(48.0, 38.0)));
            if pkg.installed.as_deref() == Some(pkg.version.as_str()) {
                w::pill(ui, sc, pos2(r.right() - pad, r.center().y), "Installed", GREEN);
            } else {
                let label = if pkg.installed.is_some() { "Update" } else { "Install" };
                if w::button(ui, sc, br, Id::unique(("pkg-install", n)), label, Some(w::ACCENT)).clicked() {
                    install = Some(n);
                }
            }
        }
        if let Some(r) = dragged {
            self.swipe_rows(&r, l, total, visible);
        }
        if let Some(n) = install {
            let pkg = self.local[n].clone();
            self.busy = Some(format!("Installing {}…", pkg.name));
            self.client.send(ui.ctx(), serde_json::json!({"cmd": "install", "path": pkg.file}), |r| if r.ok { Event::Done } else { failed(r) });
        }
    }

    /// Centre of the nth tile in the grid view (for tests).
    #[cfg(test)]
    pub fn tile_center(&self, l: &Lay, area: Rect, bar_top: f32, n: usize) -> Pos2 {
        let g = Grid::new(l, area, bar_top, l.v(170.0, 132.0));
        let s = &self.slots(&g)[n];
        g.rect(s.row, s.col, s.span, self.row).center()
    }
}

fn load_icon(ctx: &egui::Context, app: &InstalledApp) -> Option<TextureHandle> {
    let bytes = std::fs::read(app.icon.as_ref()?).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ctx.load_texture(format!("app-icon-{}", app.id), egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR))
}
