//! The Wi-Fi part of Settings > Network: the networks in range, and the dialogs
//! to join one (on-screen keyboard), join it again or forget it. kioskd does the
//! work; joining or forgetting needs the admin PIN, asked for when it is needed.

use egui::{Align2, Color32, CornerRadius, Id, Rect, Sense, Stroke, Ui, pos2, vec2};
use serde::Deserialize;

use crate::keyboard::{Action, Keyboard};
use crate::store::{Event, Reply, Store, View};
use kiosk_kit::Lay;
use kiosk_kit::glass::Glass;
use kiosk_kit::widgets::{self as w, ACCENT, GREEN, Icon, Sc, text};

/// seconds between scans while the Network section is open
const RESCAN_SECS: f64 = 30.0;
/// the password and name limits kioskd accepts
const PSK_MAX: usize = 63;
const SSID_MAX: usize = 32;
const RED: Color32 = Color32::from_rgb(255, 69, 58);

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(crate) struct Net {
    pub ssid: String,
    #[serde(default)]
    pub signal: u8,
    #[serde(default)]
    pub secured: bool,
    #[serde(default)]
    pub saved: bool,
    #[serde(default)]
    pub connected: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScanData {
    #[serde(default)]
    pub networks: Vec<Net>,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Ssid,
    Password,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// type the name (hidden network) and the password
    Enter,
    /// a saved or connected network: connect again, change the password or forget
    Manage,
}

struct Dialog {
    mode: Mode,
    ssid: String,
    password: String,
    hidden: bool,
    secured: bool,
    connected: bool,
    focus: Field,
    show: bool,
    kb: Keyboard,
}

impl Dialog {
    fn for_net(n: &Net) -> Self {
        Dialog {
            mode: if n.saved || n.connected { Mode::Manage } else { Mode::Enter },
            ssid: n.ssid.clone(),
            password: String::new(),
            hidden: false,
            secured: n.secured,
            connected: n.connected,
            focus: Field::Password,
            show: false,
            kb: Keyboard::default(),
        }
    }

    fn hidden() -> Self {
        Dialog { mode: Mode::Enter, ssid: String::new(), hidden: true, secured: true, focus: Field::Ssid, ..Dialog::for_net(&Net { ssid: String::new(), signal: 0, secured: true, saved: false, connected: false }) }
    }

    /// Whether Connect can be pressed.
    fn ready(&self) -> bool {
        let pw = self.password.len();
        !self.ssid.is_empty() && if self.hidden { pw == 0 || pw >= 8 } else { !self.secured || pw >= 8 }
    }
}

/// What the PIN is being asked for.
#[derive(Clone)]
enum Req {
    Connect { ssid: String, psk: Option<String>, hidden: bool },
    Forget { ssid: String },
}

pub(crate) struct Wifi {
    nets: Vec<Net>,
    scanned: bool,
    scanning: bool,
    scan_error: Option<String>,
    last_scan: f64,
    /// first row shown when the list is longer than the card
    row: usize,
    vdrag: f32,
    dialog: Option<Dialog>,
    /// the network being joined
    pub(crate) connecting: Option<String>,
    /// waiting for the admin PIN
    pending: Option<Req>,
    error: Option<String>,
    /// a short message under the title: "Connected to ..."
    notice: Option<(String, f64)>,
}

impl Default for Wifi {
    fn default() -> Self {
        Wifi {
            nets: Vec::new(),
            scanned: false,
            scanning: false,
            scan_error: None,
            last_scan: f64::NEG_INFINITY,
            row: 0,
            vdrag: 0.0,
            dialog: None,
            connecting: None,
            pending: None,
            error: None,
            notice: None,
        }
    }
}

impl Wifi {
    /// An answer to a scan.
    pub(crate) fn scanned(&mut self, d: ScanData) {
        self.scanning = false;
        self.scanned = true;
        self.scan_error = None;
        self.nets = d.networks;
        self.row = self.row.min(self.nets.len());
    }

    pub(crate) fn scan_failed(&mut self, e: String) {
        self.scanning = false;
        self.scan_error = Some(e);
    }

    /// Joining or forgetting worked.
    pub(crate) fn done(&mut self, time: f64) {
        if let Some(s) = self.connecting.take() {
            self.notice = Some((format!("Connected to {s}"), time + 6.0));
        }
        self.dialog = None;
        self.error = None;
        self.last_scan = f64::NEG_INFINITY;
    }

    pub(crate) fn failed(&mut self, e: String) {
        self.connecting = None;
        self.error = Some(e);
    }

    /// kioskd did not answer.
    pub(crate) fn unreachable(&mut self) {
        self.scanning = false;
        self.connecting = None;
    }

    pub(crate) fn modal(&self) -> bool {
        self.dialog.is_some()
    }

    #[cfg(test)]
    pub(crate) fn net_count(&self) -> usize {
        self.nets.len()
    }

    pub(crate) fn tick(&mut self, time: f64) {
        if self.notice.as_ref().is_some_and(|(_, until)| time > *until) {
            self.notice = None;
        }
    }
}

fn scan_done(r: Reply) -> Event {
    match (r.ok, r.data) {
        (true, Some(d)) => serde_json::from_value(d).map(Event::WifiScan).unwrap_or_else(|e| Event::WifiScanFailed(e.to_string())),
        _ => Event::WifiScanFailed(r.error.unwrap_or_else(|| "scan failed".into())),
    }
}

fn change_done(r: Reply) -> Event {
    if r.ok { Event::WifiDone } else { Event::WifiFailed(r.error.unwrap_or_else(|| "request failed".into())) }
}

impl Store {
    pub(crate) fn wifi_modal(&self) -> bool {
        self.pin.is_some() || self.wifi.modal()
    }

    fn wifi_scan(&mut self, ctx: &egui::Context, time: f64) {
        if self.wifi.scanning {
            return;
        }
        self.wifi.scanning = true;
        self.wifi.last_scan = time;
        self.client.send(ctx, serde_json::json!({"cmd": "wifi_scan"}), scan_done);
    }

    /// Sends the change now when the admin session is open, otherwise asks for the PIN first.
    fn wifi_change(&mut self, ctx: &egui::Context, time: f64, req: Req) {
        if self.is_admin() {
            self.wifi_send(ctx, &req);
        } else {
            self.wifi.pending = Some(req);
            self.begin_admin(View::Apps, time);
        }
    }

    fn wifi_send(&mut self, ctx: &egui::Context, req: &Req) {
        let msg = match req {
            Req::Connect { ssid, psk, hidden } => {
                self.wifi.connecting = Some(ssid.clone());
                serde_json::json!({"cmd": "wifi_connect", "ssid": ssid, "psk": psk, "hidden": hidden})
            }
            Req::Forget { ssid } => serde_json::json!({"cmd": "wifi_forget", "ssid": ssid}),
        };
        self.wifi.error = None;
        let msg = self.authed(msg);
        self.client.send(ctx, msg, change_done);
    }

    /// Once a frame while the Network section is open: the PIN answer, and scanning now and then.
    pub(crate) fn wifi_frame(&mut self, ctx: &egui::Context, time: f64, active: bool) {
        self.wifi.tick(time);
        if let Some(req) = self.wifi.pending.clone()
            && self.pin.is_none()
        {
            self.wifi.pending = None;
            // a PIN that was cancelled leaves the dialog open, to try again
            if self.is_admin() {
                self.wifi_send(ctx, &req);
            }
        }
        let busy = self.wifi.connecting.is_some() || self.wifi.pending.is_some() || self.pin.is_some();
        if active && !busy && self.wifi.dialog.is_none() && time - self.wifi.last_scan > RESCAN_SECS {
            self.wifi_scan(ctx, time);
        }
    }

    /// The card with the networks in range.
    pub(crate) fn wifi_card(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, card: Rect, time: f64) {
        let p = ui.painter().clone();
        sc.glass(&p, card, Glass::panel(l.v(34.0, 26.0)));
        let pad = l.v(30.0, 20.0);
        let (x, xr) = (card.left() + pad, card.right() - pad);
        let ty = card.top() + l.v(46.0, 32.0);
        text(&p, pos2(x, ty), Align2::LEFT_CENTER, "Wi-Fi", l.v(32.0, 22.0), sc.white(1.0));

        let idle = sc.enabled && !self.wifi_modal();
        let sc_in = Sc { enabled: idle, ..sc };
        let rescan = Rect::from_center_size(pos2(xr - l.v(50.0, 40.0), ty), vec2(l.v(100.0, 80.0), l.v(44.0, 36.0)));
        let label = if self.wifi.scanning { "Scanning" } else { "Rescan" };
        if w::button(ui, Sc { enabled: idle && !self.wifi.scanning, ..sc }, rescan, Id::unique("wifi-rescan"), label, None).clicked() {
            self.wifi_scan(ui.ctx(), time);
        }
        let status = match (&self.wifi.connecting, &self.wifi.notice) {
            (Some(s), _) => Some((format!("Connecting to {s}..."), sc.white(0.85))),
            (None, Some((m, _))) => Some((m.clone(), sc.fade(GREEN))),
            _ => None,
        };
        if let Some((m, c)) = status {
            let room = rescan.left() - x - l.v(100.0, 70.0);
            let size = l.v(16.0, 13.0);
            let shown = fit(&p, &m, size, room);
            text(&p, pos2(rescan.left() - l.v(14.0, 10.0), ty), Align2::RIGHT_CENTER, &shown, size, c);
        }

        // the list: one row per network, then "Hidden network"
        let top = card.top() + l.v(84.0, 62.0);
        let area = Rect::from_min_max(pos2(card.left() + pad * 0.5, top), pos2(card.right() - pad * 0.5, card.bottom() - l.v(14.0, 8.0)));
        p.line_segment([pos2(x, top), pos2(xr, top)], Stroke::new(1.0, sc.white(0.18)));
        if let Some(e) = &self.wifi.scan_error
            && self.wifi.nets.is_empty()
        {
            let msg = if e.contains("wpa_cli") || e.contains("wpa_supplicant") { "No Wi-Fi adapter found" } else { "Could not scan" };
            text(&p, area.center(), Align2::CENTER_CENTER, msg, l.v(20.0, 16.0), sc.white(0.7));
            return;
        }
        if !self.wifi.scanned {
            text(&p, area.center(), Align2::CENTER_CENTER, "Looking for networks...", l.v(20.0, 16.0), sc.white(0.7));
            return;
        }

        let row_h = l.v(66.0, 52.0);
        let total = self.wifi.nets.len() + 1;
        let visible = ((area.height() / row_h).floor() as usize).max(1);
        let max_row = total.saturating_sub(visible);
        self.wifi.row = self.wifi.row.min(max_row);
        // swipe the list up or down one row at a time
        let drag = ui.interact(area, Id::unique("wifi-list"), if idle { Sense::drag() } else { Sense::hover() });
        if drag.dragged() {
            self.wifi.vdrag += drag.drag_delta().y;
        }
        if drag.drag_stopped() {
            let threshold = l.v(40.0, 26.0);
            if self.wifi.vdrag < -threshold {
                self.wifi.row = (self.wifi.row + 1).min(max_row);
            } else if self.wifi.vdrag > threshold {
                self.wifi.row = self.wifi.row.saturating_sub(1);
            }
            self.wifi.vdrag = 0.0;
        }

        let mut open: Option<Dialog> = None;
        for slot in 0..visible.min(total - self.wifi.row) {
            let i = self.wifi.row + slot;
            let r = Rect::from_min_size(pos2(area.left(), area.top() + slot as f32 * row_h), vec2(area.width(), row_h));
            let id = Id::new(("wifi-row", i));
            let resp = ui.interact(r, id, if sc_in.enabled { Sense::click() } else { Sense::hover() });
            let press = ui.ctx().animate_bool_with_time(id.with("press"), resp.is_pointer_button_down_on(), 0.08);
            if press > 0.0 {
                p.rect_filled(r.shrink2(vec2(l.v(4.0, 2.0), l.v(3.0, 2.0))), CornerRadius::same(l.v(16.0, 12.0) as u8), sc.white(0.14 * press));
            }
            let cy = r.center().y;
            if let Some(n) = self.wifi.nets.get(i) {
                let sub = match (n.connected, n.saved, n.secured) {
                    (true, _, _) => "Connected",
                    (_, true, _) => "Saved",
                    (_, _, true) => "Secured",
                    _ => "Open",
                };
                row_text(&p, l, sc, r, &n.ssid, sub, n.connected);
                let right = r.right() - l.v(30.0, 18.0);
                if n.connected {
                    w::icon(&p, Icon::Check, pos2(right - l.v(6.0, 4.0), cy), l.v(24.0, 18.0), sc.fade(GREEN));
                } else if n.secured {
                    w::icon(&p, Icon::Lock, pos2(right - l.v(6.0, 4.0), cy), l.v(22.0, 17.0), sc.white(0.7));
                }
                crate::app::bars(&p, pos2(right - l.v(44.0, 34.0), cy + l.v(9.0, 7.0)), sc.u, n.signal as f32 / 100.0, sc);
                if resp.clicked() {
                    open = Some(Dialog::for_net(n));
                }
            } else {
                row_text(&p, l, sc, r, "Hidden network", "Enter the name", false);
                if resp.clicked() {
                    open = Some(Dialog::hidden());
                }
            }
            if slot + 1 < visible && i + 1 < total {
                p.line_segment([pos2(x, r.bottom()), pos2(xr, r.bottom())], Stroke::new(1.0, sc.white(0.1)));
            }
        }
        if let Some(d) = open {
            self.wifi.error = None;
            self.wifi.dialog = Some(d);
        }
        // a thin bar on the right shows there is more to swipe to
        if total > visible {
            let track = Rect::from_min_max(pos2(card.right() - l.v(10.0, 7.0), area.top() + 4.0), pos2(card.right() - l.v(6.0, 4.0), area.bottom() - 4.0));
            let th = track.height() * visible as f32 / total as f32;
            let ty = track.top() + (track.height() - th) * self.wifi.row as f32 / max_row.max(1) as f32;
            p.rect_filled(Rect::from_min_size(pos2(track.left(), ty), vec2(track.width(), th)), CornerRadius::same(2), sc.white(0.4));
        }
    }

    /// The dialog over the page: the keyboard to join a network, or the choices for a saved one.
    pub(crate) fn wifi_dialog(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, time: f64) {
        let Some(mut d) = self.wifi.dialog.take() else { return };
        let p = ui.painter().clone();
        let screen = ui.ctx().content_rect();
        p.rect_filled(screen, 0.0, Color32::from_black_alpha(115));
        // the PIN dialog sits on top of this one and takes the touches
        let live = sc.enabled && self.pin.is_none();
        let connecting = self.wifi.connecting.clone();
        let live = live && connecting.is_none();
        let sc_live = Sc { enabled: live, ..sc };
        let mut keep = true;
        let mut request: Option<Req> = None;

        match d.mode {
            Mode::Manage => {
                let w_ = l.v(560.0, 520.0).min(area.width() - l.v(40.0, 20.0));
                let h = l.v(330.0, 290.0);
                let panel = Rect::from_center_size(pos2(area.center().x, area.top() + l.v(90.0, 50.0) + h * 0.5), vec2(w_, h));
                sc.glass(&p, panel, Glass::panel(l.v(30.0, 24.0)).opacity(1.0));
                let pad = l.v(30.0, 22.0);
                text(&p, pos2(panel.left() + pad, panel.top() + l.v(44.0, 34.0)), Align2::LEFT_CENTER, &fit(&p, &d.ssid, l.v(28.0, 22.0), panel.width() - 2.0 * pad), l.v(28.0, 22.0), sc.white(1.0));
                let sub = if d.connected { "Connected" } else { "Saved network" };
                text(&p, pos2(panel.left() + pad, panel.top() + l.v(80.0, 62.0)), Align2::LEFT_CENTER, sub, l.v(17.0, 14.0), sc.white(0.7));
                if let Some(e) = &self.wifi.error {
                    text(&p, pos2(panel.left() + pad, panel.top() + l.v(110.0, 86.0)), Align2::LEFT_CENTER, &fit(&p, e, l.v(15.0, 13.0), panel.width() - 2.0 * pad), l.v(15.0, 13.0), sc.fade(Color32::from_rgb(255, 176, 168)));
                }
                let bh = l.v(60.0, 54.0);
                let gap = l.v(14.0, 10.0);
                let bw = (panel.width() - 2.0 * pad - gap) * 0.5;
                let y2 = panel.bottom() - pad - bh;
                let y1 = y2 - gap - bh;
                let b = |col: f32, y: f32| Rect::from_min_size(pos2(panel.left() + pad + col * (bw + gap), y), vec2(bw, bh));
                let connect_label = if connecting.is_some() { "Connecting..." } else if d.connected { "Reconnect" } else { "Connect" };
                if w::button(ui, sc_live, b(0.0, y1), Id::unique("wifi-connect"), connect_label, Some(ACCENT)).clicked() {
                    // no password sent: kioskd uses the saved one
                    request = Some(Req::Connect { ssid: d.ssid.clone(), psk: None, hidden: false });
                }
                if w::button(ui, sc_live, b(1.0, y1), Id::unique("wifi-forget"), "Forget", Some(RED)).clicked() {
                    request = Some(Req::Forget { ssid: d.ssid.clone() });
                }
                if w::button(ui, sc_live, b(0.0, y2), Id::unique("wifi-change"), "Change password", None).clicked() {
                    d.mode = Mode::Enter;
                    d.focus = Field::Password;
                }
                if w::button(ui, Sc { enabled: sc.enabled && self.pin.is_none(), ..sc }, b(1.0, y2), Id::unique("wifi-close"), "Close", None).clicked() {
                    keep = false;
                }
            }
            Mode::Enter => {
                let width = (area.width() - l.v(40.0, 20.0)).min(l.v(1040.0, 780.0));
                let top = area.top() + l.v(60.0, 34.0);
                let bottom = area.bottom() - l.v(24.0, 8.0);
                let panel = Rect::from_min_max(pos2(area.center().x - width * 0.5, top), pos2(area.center().x + width * 0.5, bottom));
                sc.glass(&p, panel, Glass::panel(l.v(30.0, 22.0)).opacity(1.0));
                let pad = l.v(26.0, 14.0);
                let gap = l.v(10.0, 6.0);
                let inner = Rect::from_min_max(panel.min + vec2(pad, pad * 0.8), panel.max - vec2(pad, pad));

                let mut y = inner.top();
                let title = if d.hidden { "Join a hidden network".to_string() } else { format!("Join {}", d.ssid) };
                let tsz = l.v(26.0, 20.0);
                let th = l.v(40.0, 30.0);
                w::icon(&p, Icon::Wifi, pos2(inner.left() + th * 0.4, y + th * 0.5), th * 0.6, sc.white(1.0));
                text(&p, pos2(inner.left() + th, y + th * 0.5), Align2::LEFT_CENTER, &fit(&p, &title, tsz, inner.width() - th), tsz, sc.white(1.0));
                y += th + gap;

                let fh = l.v(54.0, 44.0);
                if d.hidden {
                    let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), fh));
                    if field(ui, sc_live, r, Id::unique("wifi-f-ssid"), "Network name", &d.ssid, false, d.focus == Field::Ssid) {
                        d.focus = Field::Ssid;
                    }
                    y += fh + gap;
                }
                let r = Rect::from_min_size(pos2(inner.left(), y), vec2(inner.width(), fh));
                let hint = if d.hidden { "Password (empty if open)" } else { "Password" };
                if field(ui, sc_live, r, Id::unique("wifi-f-psk"), hint, &d.password, !d.show, d.focus == Field::Password) {
                    d.focus = Field::Password;
                }
                y += fh + gap * 0.6;

                // one line for the progress or the reason it failed
                let msg_h = l.v(24.0, 18.0);
                let size = l.v(16.0, 13.0);
                let (msg, col) = match (&connecting, &self.wifi.error) {
                    (Some(s), _) => (format!("Connecting to {s}..."), sc.white(0.85)),
                    (None, Some(e)) => (e.clone(), sc.fade(Color32::from_rgb(255, 176, 168))),
                    _ if d.secured && d.password.len() < 8 && !d.password.is_empty() => ("The password needs at least 8 characters".to_string(), sc.white(0.6)),
                    _ => (String::new(), sc.white(0.6)),
                };
                text(&p, pos2(inner.left() + 4.0, y + msg_h * 0.5), Align2::LEFT_CENTER, &fit(&p, &msg, size, inner.width()), size, col);
                y += msg_h + gap * 0.6;

                let kb_area = Rect::from_min_max(pos2(inner.left(), y), inner.max);
                let actions = [
                    Action { label: if d.show { "Hide" } else { "Show" }, accent: false, enabled: true },
                    Action { label: "Cancel", accent: false, enabled: true },
                    Action { label: "Connect", accent: true, enabled: d.ready() },
                ];
                // the keyboard types into the focused field
                let focus = d.focus;
                let Dialog { ssid, password, kb, .. } = &mut d;
                let (target, max) = if focus == Field::Ssid { (ssid, SSID_MAX) } else { (password, PSK_MAX) };
                let before = target.clone();
                let tapped = kb.ui(ui, l, sc_live, kb_area, target, max, &actions);
                if *target != before {
                    self.wifi.error = None;
                }
                match tapped {
                    Some(0) => d.show = !d.show,
                    Some(1) => keep = false,
                    Some(2) => {
                        let psk = (!d.password.is_empty()).then(|| d.password.clone());
                        request = Some(Req::Connect { ssid: d.ssid.clone(), psk, hidden: d.hidden });
                    }
                    _ => {}
                }
                // the caret blinks
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        }

        if let Some(req) = request {
            // the dialog stays until kioskd has answered
            self.wifi_change(ui.ctx(), time, req);
        }
        if keep {
            self.wifi.dialog = Some(d);
        } else {
            self.wifi.error = None;
        }
    }
}

/// A network row: name and a line under it.
fn row_text(p: &egui::Painter, l: &Lay, sc: Sc, r: Rect, name: &str, sub: &str, connected: bool) {
    let x = r.left() + l.v(30.0, 14.0);
    // leave room for the lock or check, the signal bars and the margin
    let room = r.width() - l.v(30.0, 14.0) - l.v(100.0, 78.0);
    let size = l.v(22.0, 18.0);
    text(p, pos2(x, r.center().y - l.v(11.0, 9.0)), Align2::LEFT_CENTER, &fit(p, name, size, room), size, sc.white(1.0));
    let col = if connected { sc.fade(GREEN) } else { sc.white(0.6) };
    text(p, pos2(x, r.center().y + l.v(14.0, 11.0)), Align2::LEFT_CENTER, sub, l.v(15.0, 12.0), col);
}

/// Shortens `s` with an ellipsis until it is at most `max` wide.
fn fit(p: &egui::Painter, s: &str, size: f32, max: f32) -> String {
    if w::text_width(p, s, size) <= max {
        return s.to_string();
    }
    let mut out: Vec<char> = s.chars().collect();
    while !out.is_empty() && w::text_width(p, &format!("{}...", out.iter().collect::<String>()), size) > max {
        out.pop();
    }
    format!("{}...", out.into_iter().collect::<String>())
}

/// A text field that shows what the keyboard types. Returns true when tapped.
#[allow(clippy::too_many_arguments)]
fn field(ui: &mut Ui, sc: Sc, r: Rect, id: Id, hint: &str, value: &str, masked: bool, focused: bool) -> bool {
    let resp = ui.interact(r, id, if sc.enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter().clone();
    let u = sc.u;
    let g = Glass::control(r.height() * 0.3).frost(0.7).no_shadow();
    sc.glass(&p, r, if focused { g.tinted(ACCENT, 0.35) } else { g });
    if focused {
        p.rect_stroke(r, CornerRadius::same((r.height() * 0.3) as u8), Stroke::new(2.0 * u, sc.fade(ACCENT)), egui::StrokeKind::Inside);
    }
    let x = r.left() + 18.0 * u;
    let size = r.height() * 0.42;
    let max = r.width() - 36.0 * u;
    let end = if value.is_empty() {
        text(&p, pos2(x, r.center().y), Align2::LEFT_CENTER, hint, size, sc.white(0.45));
        x
    } else if masked {
        // dots instead of letters; only the last ones that fit
        let step = size * 0.62;
        let n = value.chars().count();
        let fit_n = ((max / step) as usize).max(1).min(n);
        for i in 0..fit_n {
            p.circle_filled(pos2(x + step * 0.5 + i as f32 * step, r.center().y), size * 0.2, sc.white(0.95));
        }
        x + step * fit_n as f32
    } else {
        // the end of the text stays visible
        let mut shown: &str = value;
        while w::text_width(&p, shown, size) > max && shown.len() > 1 {
            shown = &shown[shown.char_indices().nth(1).map(|(i, _)| i).unwrap_or(shown.len())..];
        }
        let rect = text(&p, pos2(x, r.center().y), Align2::LEFT_CENTER, shown, size, sc.white(1.0));
        rect.right().min(x + max)
    };
    if focused && (ui.ctx().input(|i| i.time) * 2.0) as i64 % 2 == 0 {
        let cx = end + 2.0 * u;
        p.line_segment([pos2(cx, r.center().y - size * 0.6), pos2(cx, r.center().y + size * 0.6)], Stroke::new(2.0 * u, sc.white(0.9)));
    }
    resp.clicked()
}
