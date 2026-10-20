//! The screens behind the Store tile: the admin PIN keypad, the catalog, an
//! app's detail with the install progress, and the rollback notice.
//!
//! They are drawn on the 800x480 design grid and scaled up for larger screens
//! (`D::s`), so one set of coordinates serves both layouts.

use egui::{Align2, Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, Sense, Ui, pos2, vec2};
use kiosk_kit::Lay;
use kiosk_kit::glass::Glass;
use kiosk_kit::widgets::{self as w, ACCENT, GREEN, Icon, Sc, text};

use crate::store::{CatalogApp, Event, InstalledApp, LocalPackage, Store, View, initial_tile};

/// 800x480 design coordinates -> screen.
#[derive(Clone, Copy)]
struct D {
    x0: f32,
    s: f32,
}

impl D {
    fn new(l: &Lay, area: Rect) -> D {
        let s = l.v(1.5, 1.0);
        D { x0: area.center().x - 400.0 * s, s }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(self.x0 + x * self.s, y * self.s)
    }

    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), vec2(w * self.s, h * self.s))
    }

    fn f(&self, size: f32) -> f32 {
        size * self.s
    }
}

/// Wrapped text; returns its height.
fn para(p: &Painter, pos: Pos2, s: &str, size: f32, color: Color32, width: f32) -> f32 {
    let galley = p.layout(s.to_owned(), FontId::proportional(size), color, width);
    let h = galley.size().y;
    p.galley(pos, galley, color);
    h
}

fn size_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

fn permissions_text(perms: &[String]) -> String {
    if perms.is_empty() {
        "No permissions".to_string()
    } else {
        let mut s = perms.join(", ");
        s[..1].make_ascii_uppercase();
        s
    }
}

/// A round glass button with an icon.
fn icon_button(ui: &mut Ui, sc: Sc, rect: Rect, id: Id, icon: Icon) -> egui::Response {
    let r = ui.interact(rect, id, if sc.enabled { Sense::click() } else { Sense::hover() });
    let press = ui.ctx().animate_bool_with_time(id.with("press"), r.is_pointer_button_down_on(), 0.1);
    sc.glass(ui.painter(), rect, Glass::control(rect.height() * 0.5).highlight(1.0 + 0.6 * press));
    w::icon(ui.painter(), icon, rect.center(), rect.height() * 0.45, sc.white(1.0));
    r
}

/// One line in the catalog: an app from the store, an installed app, or a package on local media.
struct Card {
    id: String,
    name: String,
    line2: String,
    line3: String,
    button: CardButton,
    /// the package file, for packages on local media
    file: Option<String>,
    /// the card opens the app's detail
    detail: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum CardButton {
    Install,
    Update,
    Installed,
    Manage,
}

#[derive(Clone, PartialEq)]
enum Tab {
    All,
    Category(String),
    Installed,
    Local,
    Updates,
}

impl Store {
    fn tabs(&self) -> Vec<(String, Tab)> {
        let mut tabs = Vec::new();
        if self.info.enabled {
            tabs.push(("All".to_string(), Tab::All));
            let mut cats: Vec<&str> = self.info.apps.iter().map(|a| a.category.as_str()).collect();
            cats.sort();
            cats.dedup();
            tabs.extend(cats.into_iter().map(|c| (c.to_string(), Tab::Category(c.to_string()))));
        }
        tabs.push(("Installed".to_string(), Tab::Installed));
        if self.allow_unsigned && !self.local.is_empty() {
            tabs.push(("Local".to_string(), Tab::Local));
        }
        let updates = self.info.apps.iter().filter(|a| a.update).count();
        if updates > 0 {
            tabs.push((format!("Updates · {updates}"), Tab::Updates));
        }
        tabs
    }

    fn cards(&self, tab: &Tab) -> Vec<Card> {
        let from_catalog = |a: &CatalogApp| {
            let installed = a.installed.as_deref();
            let (button, line2) = match installed {
                Some(v) if a.update => (CardButton::Update, format!("{v} → {} · {}", a.version, size_mb(a.size))),
                Some(v) => (CardButton::Installed, format!("{v} · installed")),
                None => (CardButton::Install, format!("{} · {}", a.version, size_mb(a.size))),
            };
            Card { id: a.id.clone(), name: a.name.clone(), line2, line3: permissions_text(&a.permissions), button, file: None, detail: true }
        };
        match tab {
            Tab::All => self.info.apps.iter().map(from_catalog).collect(),
            Tab::Category(c) => self.info.apps.iter().filter(|a| a.category == *c).map(from_catalog).collect(),
            Tab::Updates => self.info.apps.iter().filter(|a| a.update).map(from_catalog).collect(),
            Tab::Installed => self
                .apps
                .iter()
                .map(|a| Card { id: a.id.clone(), name: a.name.clone(), line2: format!("{} · installed", a.version), line3: a.previous.as_ref().map(|p| format!("Can go back to {p}")).unwrap_or_default(), button: CardButton::Manage, file: None, detail: true })
                .collect(),
            Tab::Local => self
                .local
                .iter()
                .map(|p: &LocalPackage| {
                    let (button, line2) = match p.installed.as_deref() {
                        Some(v) if v == p.version => (CardButton::Installed, format!("{} · installed", p.version)),
                        Some(v) => (CardButton::Update, format!("{v} → {}", p.version)),
                        None => (CardButton::Install, p.version.clone()),
                    };
                    Card { id: p.id.clone(), name: p.name.clone(), line2, line3: "On the SD card".into(), button, file: Some(p.file.clone()), detail: false }
                })
                .collect(),
        }
    }

    /// The header every store screen shares: back button, title, session timer.
    fn header(&mut self, ui: &mut Ui, sc: Sc, d: D, title: &str, time: f64) -> bool {
        let p = ui.painter().clone();
        let back = icon_button(ui, sc, d.r(24.0, 44.0, 44.0, 44.0), Id::unique(("store-back", title)), Icon::Back).clicked();
        text(&p, d.p(80.0, 66.0), Align2::LEFT_CENTER, title, d.f(26.0), sc.white(1.0));
        if self.is_admin() {
            let left = (self.admin_until - time).max(0.0).ceil() as u64;
            let label = format!("Admin · locks in {left} s");
            let wd = w::text_width(&p, &label, d.f(12.0)) + d.f(44.0);
            let chip = Rect::from_min_size(d.p(776.0, 52.0) - vec2(wd, 0.0), vec2(wd, d.f(30.0)));
            sc.glass(&p, chip, Glass::control(chip.height() * 0.5).no_shadow());
            w::icon(&p, Icon::Lock, pos2(chip.left() + d.f(16.0), chip.center().y), d.f(14.0), sc.white(1.0));
            text(&p, pos2(chip.left() + d.f(30.0), chip.center().y), Align2::LEFT_CENTER, &label, d.f(12.0), sc.white(1.0));
        }
        back
    }

    pub(crate) fn catalog_view(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, bar_top: f32, time: f64) {
        let d = D::new(l, area);
        let p = ui.painter().clone();
        if self.header(ui, sc, d, "Store", time) {
            self.back_to_grid();
            return;
        }
        // synced time and the Check now button
        if self.info.enabled {
            let synced = self
                .info
                .synced_at
                .and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0))
                .map(|t| t.with_timezone(&chrono::Local).format("synced %H:%M").to_string())
                .unwrap_or_else(|| "not synced yet".into());
            let info = format!("{} · {synced}", self.info.channel);
            let chip_w = if self.is_admin() { 190.0 } else { 0.0 };
            let refresh = d.r(776.0 - chip_w - 36.0, 52.0, 30.0, 30.0);
            text(&p, pos2(refresh.left() - d.f(10.0), refresh.center().y), Align2::RIGHT_CENTER, &info, d.f(12.0), sc.white(0.82));
            if icon_button(ui, sc, refresh, Id::unique("store-check"), Icon::Refresh).clicked() {
                self.busy = Some("Checking the store…".into());
                self.client.send(ui.ctx(), serde_json::json!({"cmd": "store_refresh"}), |r| if r.ok { Event::Done } else { Event::Failed(r.error.unwrap_or_default()) });
            }
        }

        // category tabs
        let tabs = self.tabs();
        self.tab = self.tab.min(tabs.len().saturating_sub(1));
        let labels: Vec<&str> = tabs.iter().map(|(n, _)| n.as_str()).collect();
        let strip = d.r(24.0, 100.0, (labels.iter().map(|l| w::text_width(&p, l, d.f(13.0)) / d.s + 36.0).sum::<f32>() + 8.0).min(752.0), 48.0);
        let before = self.tab;
        w::segmented(ui, sc, strip, Id::unique("store-tabs"), &labels, &mut self.tab);
        if before != self.tab {
            self.row = 0;
        }
        let tab = tabs.get(self.tab).map(|t| t.1.clone()).unwrap_or(Tab::All);
        let cards = self.cards(&tab);
        if cards.is_empty() {
            let text_ = match tab {
                Tab::Updates => "Everything is up to date.",
                Tab::Installed => "No apps installed.",
                _ => "Nothing here yet.",
            };
            text(&p, d.p(400.0, 260.0), Align2::CENTER_CENTER, text_, d.f(18.0), sc.white(0.75));
            return;
        }

        // two columns of cards; rows move one at a time
        let (card_w, card_h, gap) = (370.0, 112.0, 12.0);
        let rows_visible = ((((bar_top / d.s) - 162.0 + gap) / (card_h + gap)) as usize).max(1);
        let total_rows = cards.len().div_ceil(2);
        self.row = self.row.min(total_rows.saturating_sub(rows_visible));
        let mut act: Option<(usize, bool)> = None;
        let mut dragged = None;
        for (i, c) in cards.iter().enumerate() {
            let row = i / 2;
            if row < self.row || row >= self.row + rows_visible {
                continue;
            }
            let r = d.r(24.0 + (i % 2) as f32 * (card_w + gap), 162.0 + (row - self.row) as f32 * (card_h + gap), card_w, card_h);
            let id = Id::unique(("card", &c.id, &c.file));
            let body = ui.interact(r, id, if sc.enabled { Sense::click_and_drag() } else { Sense::hover() });
            sc.glass(&p, r, Glass::panel(d.f(18.0)));
            let ib = Rect::from_center_size(pos2(r.left() + d.f(14.0 + 26.0), r.center().y), vec2(d.f(52.0), d.f(52.0)));
            initial_tile(&p, sc, ib, &c.id, &c.name);
            let tx = ib.right() + d.f(14.0);
            let btn_w = d.f(104.0);
            let fit = |s: &str, size: f32| size * (((r.right() - btn_w - d.f(24.0)) - tx) / w::text_width(&p, s, size)).min(1.0);
            text(&p, pos2(tx, r.center().y - d.f(26.0)), Align2::LEFT_CENTER, &c.name, fit(&c.name, d.f(16.0)), sc.white(1.0));
            text(&p, pos2(tx, r.center().y), Align2::LEFT_CENTER, &c.line2, fit(&c.line2, d.f(12.0)), sc.white(0.82));
            text(&p, pos2(tx, r.center().y + d.f(24.0)), Align2::LEFT_CENTER, &c.line3, fit(&c.line3, d.f(12.0)), sc.white(0.82));

            let br = Rect::from_center_size(pos2(r.right() - d.f(16.0) - btn_w * 0.5, r.center().y), vec2(btn_w, d.f(44.0)));
            let clicked = match c.button {
                CardButton::Installed => {
                    sc.glass(&p, br, Glass::control(br.height() * 0.5).no_shadow().opacity(0.35));
                    text(&p, br.center(), Align2::CENTER_CENTER, "Installed", d.f(14.0), sc.white(0.8));
                    false
                }
                CardButton::Install => w::button(ui, sc, br, Id::unique(("card-btn", &c.id, &c.file)), "Install", None).clicked(),
                CardButton::Update => w::button(ui, sc, br, Id::unique(("card-btn", &c.id, &c.file)), "Update", Some(ACCENT)).clicked(),
                CardButton::Manage => w::button(ui, sc, br, Id::unique(("card-btn", &c.id, &c.file)), "Manage", None).clicked(),
            };
            if clicked {
                act = Some((i, true));
            } else if body.clicked() && c.detail {
                act = Some((i, false));
            }
            if body.dragged() || body.drag_stopped() {
                dragged = Some(body);
            }
        }
        if let Some(r) = dragged {
            self.swipe_rows(&r, l, total_rows, rows_visible);
        }
        if let Some((i, button)) = act {
            let c = &cards[i];
            match (button, c.button) {
                (true, CardButton::Manage) | (false, _) => self.view = View::Detail(c.id.clone()),
                (true, _) => self.install(ui.ctx(), &c.id, &c.name, c.button == CardButton::Update, c.file.clone()),
            }
        }
    }

    /// Starts an install or update: from the store, or from a package file on local media.
    pub(crate) fn install(&mut self, ctx: &egui::Context, id: &str, name: &str, update: bool, file: Option<String>) {
        use crate::store::Job;
        self.job = Some(Job { id: id.to_string(), name: name.to_string(), update });
        self.progress = Default::default();
        let req = match file {
            Some(path) => serde_json::json!({"cmd": "install", "path": path}),
            None => serde_json::json!({"cmd": "store_install", "id": id}),
        };
        self.client.send(ctx, self.authed(req), |r| if r.ok { Event::Done } else { Event::Failed(r.error.unwrap_or_else(|| "install failed".into())) });
        if self.is_admin() && !matches!(self.view, View::Detail(_)) {
            self.view = View::Detail(id.to_string());
        }
    }

    pub(crate) fn detail_view(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, time: f64, id: &str) {
        let d = D::new(l, area);
        let p = ui.painter().clone();
        let installed: Option<InstalledApp> = self.apps.iter().find(|a| a.id == id).cloned();
        let cat: Option<CatalogApp> = self.info.apps.iter().find(|a| a.id == id).cloned();
        let name = cat.as_ref().map(|c| c.name.clone()).or(installed.as_ref().map(|a| a.name.clone())).unwrap_or_else(|| id.to_string());
        if self.header(ui, sc, d, &name, time) {
            self.view = View::Catalog;
            return;
        }
        let working = self.job.as_ref().is_some_and(|j| j.id == id);

        // left card: who it is, and what you can do with it
        let left = d.r(24.0, 100.0, 360.0, 296.0);
        sc.glass(&p, left, Glass::panel(d.f(22.0)));
        let ib = Rect::from_min_size(left.min + vec2(d.f(20.0), d.f(20.0)), vec2(d.f(64.0), d.f(64.0)));
        initial_tile(&p, sc, ib, id, &name);
        let version = match (&installed, &cat) {
            (Some(i), Some(c)) if c.update => format!("{} → {}", i.version, c.version),
            (Some(i), _) => i.version.clone(),
            (None, Some(c)) => c.version.clone(),
            _ => String::new(),
        };
        text(&p, pos2(ib.right() + d.f(16.0), ib.center().y - d.f(12.0)), Align2::LEFT_CENTER, &version, d.f(18.0), sc.white(1.0));
        let meta = match &cat {
            Some(c) => format!("{} · {}{}", id, size_mb(c.size), if c.boards.is_empty() { String::new() } else { format!(" · {}", c.boards.join(", ")) }),
            None => id.to_string(),
        };
        let fit = (left.right() - d.f(20.0) - (ib.right() + d.f(16.0))) / w::text_width(&p, &meta, d.f(12.0));
        text(&p, pos2(ib.right() + d.f(16.0), ib.center().y + d.f(14.0)), Align2::LEFT_CENTER, &meta, d.f(12.0) * fit.min(1.0), sc.white(0.82));

        let mut cx = left.left() + d.f(20.0);
        let cy = left.top() + d.f(116.0);
        if let Some(c) = &cat {
            let label = format!("Signed, {} channel", self.info.channel);
            let wd = w::text_width(&p, &label, d.f(12.0)) + d.f(36.0);
            let chip = Rect::from_min_size(pos2(cx, cy), vec2(wd, d.f(28.0)));
            sc.glass(&p, chip, Glass::control(chip.height() * 0.5).tinted(GREEN, 0.5).no_shadow());
            w::icon(&p, Icon::Shield, pos2(chip.left() + d.f(16.0), chip.center().y), d.f(14.0), sc.white(1.0));
            text(&p, pos2(chip.left() + d.f(28.0), chip.center().y), Align2::LEFT_CENTER, &label, d.f(12.0), sc.white(1.0));
            cx = chip.right() + d.f(8.0);
            let _ = c;
        }
        if let Some(c) = &cat {
            if c.permissions.is_empty() {
                text(&p, pos2(cx + d.f(4.0), cy + d.f(14.0)), Align2::LEFT_CENTER, "No permissions", d.f(12.0), sc.white(0.82));
            }
            for perm in &c.permissions {
                let label = permissions_text(std::slice::from_ref(perm));
                let wd = w::text_width(&p, &label, d.f(12.0)) + d.f(20.0);
                let chip = Rect::from_min_size(pos2(cx, cy), vec2(wd, d.f(28.0)));
                p.rect_stroke(chip, CornerRadius::same((chip.height() * 0.5) as u8), egui::Stroke::new(1.0, sc.white(0.45)), egui::StrokeKind::Middle);
                text(&p, chip.center(), Align2::CENTER_CENTER, &label, d.f(12.0), sc.white(1.0));
                cx = chip.right() + d.f(8.0);
            }
        }
        let note = match (&cat, &installed) {
            (Some(c), _) => format!("{} · {}", c.category, if installed.is_some() { "installed on this kiosk" } else { "not installed" }),
            (None, Some(_)) => "Installed from local media".to_string(),
            _ => String::new(),
        };
        para(&p, pos2(left.left() + d.f(20.0), left.top() + d.f(164.0)), &note, d.f(13.0), sc.white(0.88), left.width() - d.f(40.0));

        if !working {
            let by = left.bottom() - d.f(20.0 + 44.0);
            let half = (left.width() - d.f(40.0 + 10.0)) / 2.0;
            let can_back = installed.as_ref().is_some_and(|i| i.previous.is_some());
            if installed.is_some() {
                let rb = Rect::from_min_size(pos2(left.left() + d.f(20.0), by), vec2(half, d.f(44.0)));
                if can_back && w::button(ui, sc, rb, Id::unique("roll-back"), "Roll back", None).clicked() {
                    self.busy = Some(format!("Rolling {name} back…"));
                    self.client.send(ui.ctx(), self.authed(serde_json::json!({"cmd": "rollback", "id": id})), |r| if r.ok { Event::Done } else { Event::Failed(r.error.unwrap_or_default()) });
                } else if !can_back {
                    sc.glass(&p, rb, Glass::control(rb.height() * 0.5).no_shadow().opacity(0.3));
                    text(&p, rb.center(), Align2::CENTER_CENTER, "Roll back", d.f(14.0), sc.white(0.45));
                }
                let rm = Rect::from_min_size(rb.right_top() + vec2(d.f(10.0), 0.0), vec2(half, d.f(44.0)));
                let confirming = self.confirm_remove.as_ref().is_some_and(|(i, _)| i == id);
                let label = if confirming { "Tap to confirm" } else { "Remove" };
                if w::button(ui, sc, rm, Id::unique("remove"), label, Some(Color32::from_rgb(200, 50, 80))).clicked() {
                    if confirming {
                        self.confirm_remove = None;
                        self.busy = Some(format!("Removing {name}…"));
                        self.client.send(ui.ctx(), self.authed(serde_json::json!({"cmd": "remove", "id": id})), |r| if r.ok { Event::Done } else { Event::Failed(r.error.unwrap_or_default()) });
                        self.view = View::Catalog;
                    } else {
                        self.confirm_remove = Some((id.to_string(), time + 4.0));
                    }
                }
            } else if let Some(c) = &cat {
                let b = Rect::from_min_size(pos2(left.left() + d.f(20.0), by), vec2(left.width() - d.f(40.0), d.f(44.0)));
                if w::button(ui, sc, b, Id::unique("detail-install"), "Install", Some(ACCENT)).clicked() {
                    let name = c.name.clone();
                    self.install(ui.ctx(), id, &name, false, None);
                }
            }
            if let (Some(c), Some(_)) = (&cat, &installed)
                && c.update
            {
                let b = Rect::from_min_size(pos2(left.left() + d.f(20.0), by - d.f(54.0)), vec2(left.width() - d.f(40.0), d.f(44.0)));
                if w::button(ui, sc, b, Id::unique("detail-update"), &format!("Update to {}", c.version), Some(ACCENT)).clicked() {
                    let name = c.name.clone();
                    self.install(ui.ctx(), id, &name, true, None);
                }
            }
        }

        // right card: what is happening, or how updates stay safe
        let right = d.r(400.0, 100.0, 376.0, 296.0);
        sc.glass(&p, right, Glass::panel(d.f(22.0)));
        let x = right.left() + d.f(20.0);
        if working {
            let job = self.job.as_ref().unwrap();
            text(&p, pos2(x, right.top() + d.f(30.0)), Align2::LEFT_CENTER, if job.update { "Updating" } else { "Installing" }, d.f(18.0), sc.white(1.0));
            let phase = self.progress.phase.as_str();
            let frac = if self.progress.total > 0 { (self.progress.done as f32 / self.progress.total as f32).clamp(0.0, 1.0) } else { 0.0 };
            let step = |p: &Painter, y: f32, label: &str, state: i8, extra: &str| {
                let c = pos2(x + d.f(13.0), right.top() + d.f(y));
                match state {
                    2 => {
                        p.circle_filled(c, d.f(13.0), sc.fade(Color32::from_rgb(47, 163, 107)));
                        w::icon(p, Icon::Check, c, d.f(14.0), sc.white(1.0));
                    }
                    1 => {
                        p.circle_filled(c, d.f(13.0), sc.fade(Color32::from_rgb(47, 111, 224)));
                        p.circle_stroke(c, d.f(12.0), egui::Stroke::new(2.0, sc.white(0.8)));
                    }
                    _ => {
                        p.circle_stroke(c, d.f(12.0), egui::Stroke::new(2.0, sc.white(0.5)));
                    }
                }
                text(p, pos2(x + d.f(38.0), c.y), Align2::LEFT_CENTER, label, d.f(14.0), sc.white(if state == 0 { 0.8 } else { 1.0 }));
                if !extra.is_empty() {
                    text(p, pos2(right.right() - d.f(20.0), c.y), Align2::RIGHT_CENTER, extra, d.f(12.0), sc.white(0.82));
                }
            };
            let installing = phase == "install";
            step(&p, 74.0, "Download", if installing { 2 } else { 1 }, &if installing || self.progress.total == 0 { String::new() } else { format!("{} of {}", size_mb(self.progress.done), size_mb(self.progress.total)) });
            step(&p, 114.0, "Verify signature and hash", if installing { 2 } else { 0 }, "");
            step(&p, 154.0, "Install", if installing { 1 } else { 0 }, &if installing { format!("{} %", (frac * 100.0) as u32) } else { String::new() });
            let bar = Rect::from_min_size(pos2(x + d.f(38.0), right.top() + d.f(172.0)), vec2(right.width() - d.f(78.0), d.f(8.0)));
            p.rect_filled(bar, CornerRadius::same((d.f(4.0)) as u8), sc.white(0.2));
            let fill = if installing { frac } else { frac.min(1.0) * 0.0 };
            p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * fill, bar.height())), CornerRadius::same((d.f(4.0)) as u8), sc.white(1.0));
            step(&p, 210.0, "Health check", 0, "first launch · 30 s");
            let old = installed.as_ref().map(|i| i.version.clone());
            let note = match old {
                Some(v) => format!("{v} stays on disk until the new version has passed its health check. Losing power now is safe."),
                None => "Losing power now is safe: nothing is installed until every file is on disk.".to_string(),
            };
            para(&p, pos2(x, right.top() + d.f(238.0)), &note, d.f(12.0), sc.white(0.82), right.width() - d.f(40.0));
        } else {
            text(&p, pos2(x, right.top() + d.f(30.0)), Align2::LEFT_CENTER, "How updates stay safe", d.f(18.0), sc.white(1.0));
            let mut y = right.top() + d.f(62.0);
            for line in [
                "Every package is signed. The kiosk checks the signature and the hash before it unpacks anything.",
                "A new version replaces the old one in one step. If power is lost, the old version still runs.",
                "The first time a new version runs it has 30 s to prove itself. If it closes early, the kiosk goes back to the old version.",
            ] {
                y += para(&p, pos2(x, y), line, d.f(13.0), sc.white(0.88), right.width() - d.f(40.0)) + d.f(12.0);
            }
        }
    }

    pub(crate) fn pin_modal(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, time: f64) {
        let d = D::new(l, area);
        let p = ui.painter().clone();
        let screen = ui.ctx().content_rect();
        p.rect_filled(screen, 0.0, Color32::from_black_alpha(115));
        let Some(pin) = &self.pin else { return };
        let (locked, wrong, waiting, len) = (pin.locked_until.map(|t| (t - time).max(0.0)), pin.wrong, pin.waiting, pin.digits.len());
        let pin_set = self.admin.pin_set;

        let dialog = d.r(100.0, 60.0, 600.0, 360.0);
        sc.glass(&p, dialog, Glass::panel(d.f(26.0)).opacity(1.0));
        let ib = d.r(128.0, 88.0, 52.0, 52.0);
        p.rect_filled(ib, CornerRadius::same(d.f(14.0) as u8), sc.white(0.2));
        w::icon(&p, Icon::Lock, ib.center(), d.f(26.0), sc.white(1.0));
        text(&p, d.p(128.0, 166.0), Align2::LEFT_CENTER, "Admin PIN", d.f(24.0), sc.white(1.0));
        let help = if pin_set { "Installing, updating and removing apps needs the PIN set in kiosk.conf." } else { "No admin PIN is set. Add STORE_PIN to kiosk.conf to install apps." };
        para(&p, d.p(128.0, 188.0), help, d.f(13.0), sc.white(0.86), d.f(250.0));

        let dots = len.max(4);
        match (locked, wrong) {
            (Some(left), _) if left > 0.0 => {
                let secs = left.ceil() as u64;
                text(&p, d.p(128.0, 262.0), Align2::LEFT_CENTER, &format!("{}:{:02}", secs / 60, secs % 60), d.f(34.0), sc.white(1.0));
                para(&p, d.p(128.0, 284.0), "Locked after 3 wrong PINs. The keypad unlocks when the timer runs out.", d.f(13.0), Color32::from_rgb(255, 210, 204).gamma_multiply(sc.alpha), d.f(250.0));
            }
            (_, Some(tries)) => {
                for i in 0..dots {
                    p.circle_stroke(d.p(136.0 + i as f32 * 30.0, 250.0), d.f(8.0), egui::Stroke::new(2.0, Color32::from_rgb(255, 176, 168).gamma_multiply(sc.alpha)));
                }
                let lock_min = (self.admin.lockout.max(1) as f32 / 60.0).round().max(1.0) as u32;
                let msg = format!("Wrong PIN. {tries} {} left before a {lock_min}-minute lock.", if tries == 1 { "try" } else { "tries" });
                para(&p, d.p(128.0, 268.0), &msg, d.f(13.0), Color32::from_rgb(255, 210, 204).gamma_multiply(sc.alpha), d.f(250.0));
            }
            _ => {
                for i in 0..dots {
                    let c = d.p(136.0 + i as f32 * 30.0, 250.0);
                    if i < len {
                        p.circle_filled(c, d.f(8.0), sc.white(1.0));
                    }
                    p.circle_stroke(c, d.f(8.0), egui::Stroke::new(2.0, sc.white(0.85)));
                }
            }
        }

        let cancel = d.r(128.0, 350.0, 140.0, 56.0);
        if w::button(ui, Sc { enabled: sc.enabled, ..sc }, cancel, Id::unique("pin-cancel"), "Cancel", None).clicked() {
            self.pin = None;
            return;
        }

        // keypad
        let active = pin_set && locked.is_none_or(|t| t <= 0.0) && !waiting;
        let pad_sc = Sc { enabled: sc.enabled && active, alpha: sc.alpha * if active { 1.0 } else { 0.35 }, ..sc };
        let (kw, kh, kgap) = ((252.0 - 20.0) / 3.0, 56.0, 10.0);
        let mut pressed: Option<char> = None;
        for (i, key) in ['1', '2', '3', '4', '5', '6', '7', '8', '9', '<', '0', '>'].into_iter().enumerate() {
            let r = d.r(420.0 + (i % 3) as f32 * (kw + kgap), 113.0 + (i / 3) as f32 * (kh + kgap), kw, kh);
            let id = Id::unique(("pin-key", key));
            let resp = ui.interact(r, id, if pad_sc.enabled { Sense::click() } else { Sense::hover() });
            let press = ui.ctx().animate_bool_with_time(id.with("press"), resp.is_pointer_button_down_on(), 0.1);
            let g = match key {
                '<' => Glass::control(r.height() * 0.5).tinted(Color32::WHITE, 0.0).no_shadow().opacity(0.0),
                '>' => Glass::control(r.height() * 0.5).tinted(ACCENT, 0.6).highlight(1.0 + 0.6 * press),
                _ => Glass::control(r.height() * 0.5).highlight(1.0 + 0.6 * press),
            };
            pad_sc.glass(&p, r, g);
            match key {
                '<' => w::icon(&p, Icon::Backspace, r.center(), d.f(26.0), pad_sc.white(1.0)),
                '>' => w::icon(&p, Icon::Check, r.center(), d.f(26.0), pad_sc.white(1.0)),
                k => {
                    text(&p, r.center(), Align2::CENTER_CENTER, &k.to_string(), d.f(22.0), pad_sc.white(1.0));
                }
            }
            if resp.clicked() {
                pressed = Some(key);
            }
        }
        if let (Some(k), Some(pin)) = (pressed, &mut self.pin) {
            match k {
                '<' => {
                    pin.digits.pop();
                    pin.wrong = None;
                }
                '>' if pin.digits.len() >= 4 => {
                    self.submit_pin(ui.ctx());
                }
                '>' => {}
                digit if pin.digits.len() < 8 => {
                    pin.digits.push(digit);
                    pin.wrong = None;
                }
                _ => {}
            }
        }
    }

    /// An update was rolled back: say so until the admin dismisses it.
    pub(crate) fn notice_dialog(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect) {
        let d = D::new(l, area);
        let p = ui.painter().clone();
        let Some(n) = self.notices.first().cloned() else { return };
        p.rect_filled(ui.ctx().content_rect(), 0.0, Color32::from_black_alpha(90));
        let r = d.r(60.0, 160.0, 680.0, 216.0);
        sc.glass(&p, r, Glass::panel(d.f(24.0)).tinted(Color32::from_rgb(255, 160, 60), 0.18).opacity(1.0));
        let ib = d.r(84.0, 184.0, 56.0, 56.0);
        p.rect_filled(ib, CornerRadius::same(d.f(15.0) as u8), sc.fade(Color32::from_rgb(224, 138, 42)));
        w::icon(&p, Icon::Undo, ib.center(), d.f(28.0), sc.white(1.0));
        let x = d.p(160.0, 0.0).x;
        text(&p, d.p(160.0, 194.0), Align2::LEFT_CENTER, &format!("{} is back on {}", n.name, n.to), d.f(20.0), sc.white(1.0));
        let body = format!("Version {} {}, so the kiosk switched back to the version that worked. {} won't be offered again on this kiosk.", n.from, if n.reason.is_empty() { "failed its health check".to_string() } else { n.reason.clone() }, n.from);
        let h = para(&p, pos2(x, d.p(0.0, 212.0).y), &body, d.f(14.0), sc.white(0.9), d.f(500.0));
        para(&p, pos2(x, d.p(0.0, 212.0).y + h + d.f(6.0)), &format!("Details are in the journal: journalctl -u kiosk-app@{}", n.id), d.f(12.0), sc.white(0.78), d.f(500.0));

        let ok = d.r(632.0 - 120.0 + 0.0, 320.0, 100.0, 44.0);
        let open = d.r(632.0 - 120.0 - 150.0 - 10.0, 320.0, 150.0, 44.0);
        let en = Sc { enabled: sc.enabled, ..sc };
        let idx = self.apps.iter().position(|a| a.id == n.id);
        if idx.is_some() && w::button(ui, en, open, Id::unique("notice-open"), &format!("Open {}", n.name.chars().take(10).collect::<String>()), None).clicked() {
            self.client.send(ui.ctx(), serde_json::json!({"cmd": "ack_notice", "id": n.id}), |_| Event::Done);
            self.notices.retain(|x| x.id != n.id);
            if let Some(i) = idx {
                self.launch(ui.ctx(), i);
            }
        }
        if w::button(ui, en, ok, Id::unique("notice-ok"), "OK", Some(ACCENT)).clicked() {
            self.client.send(ui.ctx(), serde_json::json!({"cmd": "ack_notice", "id": n.id}), |_| Event::Done);
            self.notices.retain(|x| x.id != n.id);
        }
    }
}
