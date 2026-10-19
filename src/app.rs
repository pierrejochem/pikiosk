//! The kiosk screen: wallpaper, status bar, three pages and a floating tab bar.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{Local, Timelike};
use egui::{Align2, Color32, CornerRadius, Id, Key, LayerId, Order, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};

use crate::glass::{BackdropParams, Glass, GlassSystem, Optics, Theme};
use crate::backlight::Backlight;
use crate::sysinfo::{self, Monitor};
use crate::widgets::{self as w, ACCENT, GREEN, Icon, Sc, text};

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub animate: bool,
    pub show_fps: bool,
    /// 0..1, 0.625 = default frost
    pub frost: f32,
    /// 0..1, 0.5 = default refraction
    pub refraction: f32,
    pub theme: Theme,
    /// display backlight 0..1 (only used when the panel has one)
    pub brightness: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { animate: true, show_fps: false, frost: 0.625, refraction: 0.5, theme: Theme::Aurora, brightness: 1.0 }
    }
}

impl Settings {
    fn load(path: &PathBuf) -> Self {
        let mut s = Settings::default();
        let Ok(text) = std::fs::read_to_string(path) else { return s };
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "animate" => s.animate = v == "true",
                "show_fps" => s.show_fps = v == "true",
                "frost" => s.frost = v.parse().unwrap_or(s.frost),
                "refraction" => s.refraction = v.parse().unwrap_or(s.refraction),
                "theme" => s.theme = Theme::from_name(v).unwrap_or(s.theme),
                "brightness" => s.brightness = v.parse::<f32>().unwrap_or(s.brightness).clamp(0.0, 1.0),
                _ => {}
            }
        }
        s
    }

    fn save(&self, path: &PathBuf) {
        let text = format!(
            "animate={}\nshow_fps={}\nfrost={:.3}\nrefraction={:.3}\ntheme={}\nbrightness={:.3}\n",
            self.animate,
            self.show_fps,
            self.frost,
            self.refraction,
            self.theme.name(),
            self.brightness
        );
        if let Err(e) = std::fs::write(path, text) {
            log::warn!("could not save settings to {}: {e}", path.display());
        }
    }
}

const PAGES: [(Icon, &str); 3] = [(Icon::Home, "Home"), (Icon::Wifi, "Network"), (Icon::Sliders, "Controls")];

pub struct KioskApp {
    pub glass: GlassSystem,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    pub page: usize,
    monitor: Monitor,
    has_photo: bool,
    swipe: f32,
    fps: f32,
    last_time: Option<f64>,
    /// pointer position to draw a cursor at (mouse input on the kiosk)
    pub cursor: Option<Pos2>,
    backlight: Option<Backlight>,
    /// called by the "Restart app" button
    pub on_restart: Option<Box<dyn Fn()>>,
}

impl KioskApp {
    pub fn new() -> Self {
        let data_dir = std::env::var_os("KIOSK_DATA").map(PathBuf::from);
        let settings_path = data_dir.as_ref().map(|d| d.join("kiosk-ui.conf"));
        let mut settings = settings_path.as_ref().map(Settings::load).unwrap_or_default();
        if let Ok(t) = std::env::var("KIOSK_THEME")
            && let Some(t) = Theme::from_name(&t)
        {
            settings.theme = t;
        }

        let glass = GlassSystem::default();
        let photo_path = std::env::var_os("KIOSK_WALLPAPER").map(PathBuf::from).or_else(|| {
            let d = data_dir.as_ref()?;
            ["wallpaper.jpg", "wallpaper.jpeg", "wallpaper.png"].iter().map(|n| d.join(n)).find(|p| p.exists())
        });
        let has_photo = photo_path.and_then(|p| load_photo(&glass, &p)).is_some();
        if settings.theme == Theme::Photo && !has_photo {
            settings.theme = Theme::Aurora;
        }

        let backlight = Backlight::find();
        if let Some(b) = &backlight {
            b.set(settings.brightness);
        }

        Self {
            glass,
            backlight,
            settings,
            settings_path,
            page: 0,
            monitor: Monitor::default(),
            has_photo,
            swipe: 0.0,
            fps: 0.0,
            last_time: None,
            cursor: None,
            on_restart: None,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let screen = ctx.content_rect();
        let l = Lay::new(screen);
        let time = ctx.input(|i| i.time);
        if let Some(prev) = self.last_time {
            let dt = (time - prev) as f32;
            if dt > 0.0 {
                let inst = 1.0 / dt;
                self.fps = if self.fps == 0.0 { inst } else { self.fps * 0.9 + inst * 0.1 };
            }
        }
        self.last_time = Some(time);

        let before = self.settings.clone();
        self.glass.optics = Optics { frost: self.settings.frost * 1.6, refraction: self.settings.refraction * 2.0 };
        let painter = ui.painter().clone();
        self.glass.backdrop(
            &painter,
            BackdropParams { time: time as f32, theme: self.settings.theme, animate: self.settings.animate },
        );

        // page navigation: arrow keys and horizontal swipes anywhere on the background
        ctx.input(|i| {
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::Tab) {
                self.page = (self.page + 1) % PAGES.len();
            }
            if i.key_pressed(Key::ArrowLeft) {
                self.page = (self.page + PAGES.len() - 1) % PAGES.len();
            }
        });
        let bg = ui.interact(screen, Id::unique("swipe"), Sense::drag());
        if bg.dragged() {
            self.swipe += bg.drag_delta().x;
        }
        if bg.drag_stopped() {
            let threshold = l.v(90.0, 70.0);
            if self.swipe < -threshold && self.page + 1 < PAGES.len() {
                self.page += 1;
            } else if self.swipe > threshold && self.page > 0 {
                self.page -= 1;
            }
            self.swipe = 0.0;
        }

        let glass = self.glass.clone();
        let base = Sc { gs: &glass, u: l.wu, alpha: 1.0, enabled: true };
        let info = self.monitor.get().clone();
        self.status_bar(ui, &l, base, screen, &info);

        // pages slide and fade into each other
        let anim = ctx.animate_value_with_time(Id::unique("page"), self.page as f32, 0.38);
        for i in 0..PAGES.len() {
            let d = i as f32 - anim;
            if d.abs() >= 0.999 {
                continue;
            }
            let alpha = (1.0 - d.abs()).powf(1.6);
            let area = screen.translate(vec2(d * screen.width() * 0.22, 0.0));
            let sc = Sc { gs: &glass, u: l.wu, alpha, enabled: i == self.page };
            match i {
                0 => self.home(ui, &l, sc, area, &info),
                1 => self.network(ui, &l, sc, area, &info),
                _ => self.controls(ui, &l, sc, area),
            }
        }

        // tab bar
        let bar_w = l.v(400.0, 360.0);
        let bar_h = l.v(70.0, 60.0);
        let bar = Rect::from_min_size(pos2(screen.center().x - bar_w * 0.5, screen.bottom() - bar_h - l.v(24.0, 10.0)), vec2(bar_w, bar_h));
        w::tab_bar(ui, base, bar, Id::unique("tabs"), &PAGES, &mut self.page);

        if self.settings.show_fps {
            let r = Rect::from_center_size(pos2(screen.center().x, l.v(34.0, 22.0)), vec2(l.v(96.0, 76.0), l.v(30.0, 24.0)));
            glass.paint(&painter, r, Glass::control(r.height() * 0.5).no_shadow());
            text(&painter, r.center(), Align2::CENTER_CENTER, &format!("{:.0} fps", self.fps), l.v(15.0, 12.0), Color32::WHITE);
        }

        if let Some(pos) = self.cursor {
            let p = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("cursor")));
            let d = l.v(22.0, 18.0);
            let r = Rect::from_center_size(pos, vec2(d, d));
            glass.paint(&p, r, Glass::lens(r.height() * 0.5).tinted(Color32::WHITE, 0.25));
            p.circle_stroke(pos, d * 0.5, Stroke::new(1.0, Color32::from_white_alpha(160)));
        }

        if self.settings.brightness != before.brightness
            && let Some(b) = &self.backlight
        {
            b.set(self.settings.brightness);
        }
        if self.settings != before
            && let Some(path) = &self.settings_path
        {
            self.settings.save(path);
        }

        // repaint: every frame while animating, otherwise on the next full second
        if self.settings.animate {
            ctx.request_repaint();
        } else {
            let ms = 1000 - Local::now().timestamp_subsec_millis().min(999);
            ctx.request_repaint_after(Duration::from_millis(ms as u64 + 5));
        }
    }

    fn status_bar(&self, ui: &Ui, l: &Lay, sc: Sc, screen: Rect, info: &sysinfo::SysInfo) {
        let p = ui.painter();
        let y = l.v(34.0, 22.0);
        let x0 = screen.left() + l.v(40.0, 22.0);
        let x1 = screen.right() - l.v(40.0, 22.0);
        let size = l.v(18.0, 15.0);
        text(p, pos2(x0, y), Align2::LEFT_CENTER, &info.hostname, size, sc.white(0.92));
        let now = Local::now();
        let r = text(p, pos2(x1, y), Align2::RIGHT_CENTER, &now.format("%H:%M").to_string(), size, sc.white(0.92));
        let (ic, ok) = match info.primary() {
            Some(i) if i.wireless => (Icon::Wifi, true),
            Some(_) => (Icon::Ethernet, true),
            None => (Icon::Globe, false),
        };
        w::icon(p, ic, pos2(r.left() - l.v(22.0, 18.0), y), l.v(20.0, 17.0), sc.white(if ok { 0.92 } else { 0.4 }));
    }

    fn home(&self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, info: &sysinfo::SysInfo) {
        let p = ui.painter().clone();
        let now = Local::now();

        // clock column
        let x = area.left() + l.v(72.0, 30.0);
        let greeting = match now.hour() {
            5..=11 => "Good morning",
            12..=17 => "Good afternoon",
            _ => "Good evening",
        };
        text(&p, pos2(x, l.v(190.0, 128.0)), Align2::LEFT_CENTER, greeting, l.v(26.0, 18.0), sc.white(0.78));
        text(&p, pos2(x - l.v(6.0, 4.0), l.v(300.0, 200.0)), Align2::LEFT_CENTER, &now.format("%H:%M").to_string(), l.v(168.0, 106.0), sc.white(1.0));
        text(&p, pos2(x, l.v(408.0, 272.0)), Align2::LEFT_CENTER, &now.format("%A, %-d %B").to_string(), l.v(28.0, 19.0), sc.white(0.85));

        // 2 x 2 tiles
        let (tw, th, gap) = (l.v(236.0, 170.0), l.v(158.0, 118.0), l.v(20.0, 12.0));
        let gx = area.right() - l.v(72.0, 22.0) - 2.0 * tw - gap;
        let gy = l.v(132.0, 92.0);
        let tile = |col: usize, row: usize| {
            Rect::from_min_size(pos2(gx + col as f32 * (tw + gap), gy + row as f32 * (th + gap)), vec2(tw, th))
        };

        let (net_icon, net_val, net_sub) = match info.primary() {
            Some(i) => (
                if i.wireless { Icon::Wifi } else { Icon::Ethernet },
                i.addrs.first().map(|a| a.split('/').next().unwrap_or(a).to_string()).unwrap_or_default(),
                format!("{} connected", i.name),
            ),
            None => (Icon::Globe, "Offline".into(), "no address yet".into()),
        };
        tile_card(&p, l, sc, tile(0, 0), net_icon, "Network", &net_val, &net_sub);

        let up = info.uptime.map(sysinfo::format_uptime).unwrap_or_else(|| "—".into());
        let load = info.load.map(|v| format!("load {v:.2}")).unwrap_or_else(|| "since boot".into());
        tile_card(&p, l, sc, tile(1, 0), Icon::Clock, "Uptime", &up, &load);

        let r = tile(0, 1);
        let temp = info.temp_c.map(|t| format!("{t:.0}°C")).unwrap_or_else(|| "—".into());
        tile_card(&p, l, sc, r, Icon::Thermometer, "CPU", &temp, "SoC temperature");
        if let Some(t) = info.temp_c {
            let frac = ((t - 30.0) / 55.0).clamp(0.0, 1.0);
            let col = if t < 60.0 { GREEN } else if t < 75.0 { Color32::from_rgb(255, 159, 10) } else { Color32::from_rgb(255, 69, 58) };
            let c = pos2(r.right() - l.v(46.0, 32.0), r.top() + l.v(46.0, 32.0));
            ring(&p, c, l.v(24.0, 17.0), l.v(5.0, 4.0), frac, sc.fade(col), sc.white(0.18));
        }

        let r = tile(1, 1);
        let (mem, frac) = match info.mem_mb {
            Some((used, total)) => (format!("{used} MB"), used as f32 / total.max(1) as f32),
            None => ("—".into(), 0.0),
        };
        let sub = info.mem_mb.map(|(_, t)| format!("of {t} MB")).unwrap_or_default();
        tile_card(&p, l, sc, r, Icon::Chip, "Memory", &mem, &sub);
        let pad = l.v(22.0, 16.0);
        let bh = l.v(6.0, 5.0);
        let bar = Rect::from_min_size(pos2(r.left() + pad, r.top() + l.v(64.0, 48.0)), vec2(r.width() - 2.0 * pad, bh));
        let cr = CornerRadius::same((bh * 0.5) as u8);
        p.rect_filled(bar, cr, sc.white(0.2));
        p.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, bh)), cr, sc.white(0.9));
    }

    fn network(&self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, info: &sysinfo::SysInfo) {
        let p = ui.painter().clone();
        let cw = (area.width() - l.v(144.0, 44.0)).min(l.v(940.0, 760.0));
        let shown: Vec<_> = info.ifaces.iter().take(if l.compact { 3 } else { 4 }).collect();
        let row_h = l.v(66.0, 50.0);
        let kv_h = l.v(46.0, 32.0);
        let ch = l.v(108.0, 78.0) + shown.len().max(1) as f32 * row_h + 2.0 * kv_h + l.v(18.0, 8.0);
        let card = Rect::from_min_size(pos2(area.center().x - cw * 0.5, l.v(84.0, 46.0)), vec2(cw, ch));
        sc.glass(&p, card, Glass::panel(l.v(34.0, 26.0)));

        let x = card.left() + l.v(34.0, 22.0);
        let xr = card.right() - l.v(34.0, 22.0);
        let ty = card.top() + l.v(46.0, 32.0);
        text(&p, pos2(x, ty), Align2::LEFT_CENTER, "Network", l.v(32.0, 22.0), sc.white(1.0));
        text(&p, pos2(xr, ty), Align2::RIGHT_CENTER, &format!("{}.local", info.hostname), l.v(18.0, 14.0), sc.white(0.7));

        let mut y = card.top() + l.v(108.0, 78.0);
        if shown.is_empty() {
            text(&p, pos2(x, y), Align2::LEFT_CENTER, "No network interfaces", l.v(20.0, 16.0), sc.white(0.7));
            y += row_h;
        }
        for i in shown {
            let ic = if i.wireless { Icon::Wifi } else { Icon::Ethernet };
            let d = l.v(44.0, 34.0);
            let ib = Rect::from_center_size(pos2(x + d * 0.5, y), vec2(d, d));
            sc.glass(&p, ib, Glass::control(d * 0.5).frost(0.6).no_shadow());
            w::icon(&p, ic, ib.center(), d * 0.5, sc.white(i.up as u8 as f32 * 0.6 + 0.4));
            let tx = x + d + l.v(20.0, 14.0);
            text(&p, pos2(tx, y - l.v(11.0, 9.0)), Align2::LEFT_CENTER, &i.name, l.v(21.0, 16.0), sc.white(1.0));
            let addrs = if i.addrs.is_empty() { "no address".to_string() } else { i.addrs.iter().take(2).cloned().collect::<Vec<_>>().join("   ") };
            text(&p, pos2(tx, y + l.v(13.0, 10.0)), Align2::LEFT_CENTER, &addrs, l.v(15.0, 13.0), sc.white(0.65));

            let connected = i.up && !i.addrs.is_empty();
            let (label, col) = match (connected, i.up) {
                (true, _) => ("Connected", GREEN),
                (false, true) => ("No address", Color32::from_rgb(255, 159, 10)),
                _ => ("Down", Color32::from_gray(120)),
            };
            let pr = w::pill(ui, sc, pos2(xr, y), label, col);
            if let Some(q) = i.quality {
                bars(&p, pos2(pr.left() - l.v(16.0, 12.0), y + l.v(9.0, 7.0)), sc.u, q, sc);
            }
            y += row_h;
        }

        y += l.v(4.0, 2.0);
        p.line_segment([pos2(x, y - row_h * 0.5), pos2(xr, y - row_h * 0.5)], Stroke::new(1.0, sc.white(0.18)));
        let dns = if info.dns.is_empty() { "—".to_string() } else { info.dns.iter().take(3).cloned().collect::<Vec<_>>().join("   ") };
        for (k, v) in [("Gateway", info.gateway.clone().unwrap_or_else(|| "—".into())), ("DNS", dns)] {
            text(&p, pos2(x, y), Align2::LEFT_CENTER, k, l.v(19.0, 15.0), sc.white(0.75));
            text(&p, pos2(xr, y), Align2::RIGHT_CENTER, &v, l.v(19.0, 15.0), sc.white(1.0));
            y += kv_h;
        }
    }

    /// A label with its value on the right and a slider underneath. Returns the y for the next row.
    #[allow(clippy::too_many_arguments)]
    fn slider_row(ui: &mut Ui, l: &Lay, sc: Sc, x: f32, xr: f32, y: f32, label: &str, id: &str, value: &mut f32) -> f32 {
        let p = ui.painter().clone();
        text(&p, pos2(x, y), Align2::LEFT_CENTER, label, l.v(20.0, 16.0), sc.white(0.95));
        text(&p, pos2(xr, y), Align2::RIGHT_CENTER, &format!("{:.0}%", *value * 100.0), l.v(17.0, 13.0), sc.white(0.65));
        let r = Rect::from_min_max(pos2(x - l.v(6.0, 4.0), y + l.v(18.0, 12.0)), pos2(xr + l.v(6.0, 4.0), y + l.v(58.0, 48.0)));
        w::slider(ui, sc, r, Id::unique(id), value);
        y + l.v(98.0, 76.0)
    }

    fn controls(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect) {
        let p = ui.painter().clone();
        let gap = l.v(24.0, 12.0);
        let total = (area.width() - l.v(144.0, 44.0)).min(l.v(1000.0, 760.0));
        let cw = (total - gap) * 0.5;
        let top = l.v(84.0, 46.0);
        let ch = l.v(470.0, 350.0);
        let left = Rect::from_min_size(pos2(area.center().x - total * 0.5, top), vec2(cw, ch));
        let right = left.translate(vec2(cw + gap, 0.0));
        let pad = l.v(30.0, 20.0);
        let first_row = l.v(112.0, 76.0);

        // --- Glass card
        sc.glass(&p, left, Glass::panel(l.v(34.0, 26.0)));
        let x = left.left() + pad;
        let xr = left.right() - pad;
        text(&p, pos2(x, left.top() + l.v(44.0, 30.0)), Align2::LEFT_CENTER, "Glass", l.v(30.0, 22.0), sc.white(1.0));
        let mut y = left.top() + first_row;
        y = Self::slider_row(ui, l, sc, x, xr, y, "Frost", "frost", &mut self.settings.frost);
        y = Self::slider_row(ui, l, sc, x, xr, y, "Refraction", "refr", &mut self.settings.refraction);

        text(&p, pos2(x, y), Align2::LEFT_CENTER, "Wallpaper", l.v(20.0, 16.0), sc.white(0.95));
        let mut themes: Vec<Theme> = Theme::BUILTIN.to_vec();
        if self.has_photo {
            themes.push(Theme::Photo);
        }
        let names: Vec<&str> = themes.iter().map(|t| t.name()).collect();
        let mut sel = themes.iter().position(|t| *t == self.settings.theme).unwrap_or(0);
        let seg = Rect::from_min_max(pos2(x - l.v(6.0, 4.0), y + l.v(24.0, 16.0)), pos2(xr + l.v(6.0, 4.0), y + l.v(70.0, 56.0)));
        if w::segmented(ui, sc, seg, Id::unique("theme"), &names, &mut sel) {
            self.settings.theme = themes[sel];
        }

        // --- Display card
        sc.glass(&p, right, Glass::panel(l.v(34.0, 26.0)));
        let x = right.left() + pad;
        let xr = right.right() - pad;
        text(&p, pos2(x, right.top() + l.v(44.0, 30.0)), Align2::LEFT_CENTER, "Display", l.v(30.0, 22.0), sc.white(1.0));
        let mut y = right.top() + first_row;
        if self.backlight.is_some() {
            y = Self::slider_row(ui, l, sc, x, xr, y, "Brightness", "bright", &mut self.settings.brightness);
            y += l.v(10.0, 12.0);
        } else {
            y += l.v(8.0, 14.0);
        }
        for (label, sub, id, v) in [
            ("Animated wallpaper", "Uses more GPU", "anim", &mut self.settings.animate),
            ("Show frame rate", "Overlay at the top", "fps", &mut self.settings.show_fps),
        ] {
            text(&p, pos2(x, y - l.v(10.0, 8.0)), Align2::LEFT_CENTER, label, l.v(20.0, 16.0), sc.white(0.95));
            text(&p, pos2(x, y + l.v(15.0, 11.0)), Align2::LEFT_CENTER, sub, l.v(15.0, 13.0), sc.white(0.6));
            let (tw, th) = (l.v(70.0, 58.0), l.v(34.0, 30.0));
            let t = Rect::from_min_size(pos2(xr - tw, y - th * 0.5), vec2(tw, th));
            w::toggle(ui, sc, t, Id::unique(id), v);
            y += l.v(84.0, 58.0);
        }

        if !l.compact && self.backlight.is_none() {
            let screen_px = ui.ctx().content_rect().size() * ui.ctx().pixels_per_point();
            let rows = [
                ("Resolution", format!("{:.0} × {:.0}", screen_px.x, screen_px.y)),
                ("Scale", format!("{:.2}×", ui.ctx().pixels_per_point())),
            ];
            for (k, v) in rows {
                text(&p, pos2(x, y), Align2::LEFT_CENTER, k, 18.0 * l.k, sc.white(0.75));
                text(&p, pos2(xr, y), Align2::RIGHT_CENTER, &v, 18.0 * l.k, sc.white(0.95));
                y += 40.0 * l.k;
            }
        }

        let bgap = l.v(16.0, 12.0);
        let bw = (xr - x - bgap) * 0.5;
        let bh = l.v(52.0, 42.0);
        let reset = Rect::from_min_size(pos2(x, right.bottom() - pad - bh), vec2(bw, bh));
        let restart = reset.translate(vec2(bw + bgap, 0.0));
        if w::button(ui, sc, reset, Id::unique("reset"), "Reset", None).clicked() {
            self.settings = Settings { theme: self.settings.theme, brightness: self.settings.brightness, ..Settings::default() };
        }
        if w::button(ui, sc, restart, Id::unique("restart"), "Restart app", Some(ACCENT)).clicked()
            && let Some(f) = &self.on_restart
        {
            f();
        }
    }
}

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

fn tile_card(p: &egui::Painter, l: &Lay, sc: Sc, r: Rect, icon: Icon, title: &str, value: &str, sub: &str) {
    sc.glass(p, r, Glass::panel(l.v(30.0, 22.0)));
    let ic = l.v(34.0, 26.0);
    w::icon(p, icon, pos2(r.left() + ic, r.top() + ic), l.v(22.0, 17.0), sc.white(0.9));
    text(p, pos2(r.left() + l.v(54.0, 42.0), r.top() + ic), Align2::LEFT_CENTER, title, l.v(17.0, 14.0), sc.white(0.8));
    let pad = l.v(22.0, 16.0);
    text(p, pos2(r.left() + pad, r.bottom() - l.v(56.0, 40.0)), Align2::LEFT_CENTER, value, l.v(34.0, 25.0), sc.white(1.0));
    text(p, pos2(r.left() + pad, r.bottom() - l.v(24.0, 17.0)), Align2::LEFT_CENTER, sub, l.v(15.0, 13.0), sc.white(0.65));
}

impl Default for KioskApp {
    fn default() -> Self {
        Self::new()
    }
}

fn ring(p: &egui::Painter, c: Pos2, r: f32, width: f32, frac: f32, col: Color32, bg: Color32) {
    use std::f32::consts::TAU;
    p.circle_stroke(c, r, Stroke::new(width, bg));
    let n = 48;
    let pts: Vec<Pos2> = (0..=((n as f32 * frac) as usize).max(1))
        .map(|i| {
            let a = -TAU / 4.0 + TAU * (i as f32 / n as f32).min(frac);
            pos2(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect();
    p.add(egui::Shape::line(pts, Stroke::new(width, col)));
}

fn bars(p: &egui::Painter, base: Pos2, u: f32, q: f32, sc: Sc) {
    for i in 0..4 {
        let h = (5.0 + 4.0 * i as f32) * u;
        let r = Rect::from_min_size(pos2(base.x - (3 - i) as f32 * 6.0 * u, base.y - h), vec2(4.0 * u, h));
        let on = q > i as f32 * 0.25;
        p.rect_filled(r, CornerRadius::same(1), sc.white(if on { 0.9 } else { 0.25 }));
    }
}

fn load_photo(glass: &GlassSystem, path: &std::path::Path) -> Option<()> {
    let img = match image::open(path) {
        Ok(i) => i,
        Err(e) => {
            log::warn!("wallpaper {}: {e}", path.display());
            return None;
        }
    };
    // the Pi 3 GPU handles at most 2048x2048 textures
    let img = if img.width() > 1920 || img.height() > 1920 { img.resize(1920, 1920, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    glass.set_photo(rgba.width(), rgba.height(), rgba.into_raw());
    log::info!("wallpaper photo: {}", path.display());
    Some(())
}
