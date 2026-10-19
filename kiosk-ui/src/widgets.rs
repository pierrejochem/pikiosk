//! Liquid-glass widgets: text helpers, vector icons, button, toggle, slider,
//! segmented control and the floating tab bar.

use egui::{Align2, Color32, CornerRadius, FontId, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};

use crate::glass::{Glass, GlassSystem};

pub const ACCENT: Color32 = Color32::from_rgb(10, 132, 255);
pub const GREEN: Color32 = Color32::from_rgb(52, 199, 89);

/// Everything a widget needs to draw itself.
#[derive(Clone, Copy)]
pub struct Sc<'a> {
    pub gs: &'a GlassSystem,
    /// layout unit: 1.0 on a 720-point-tall screen
    pub u: f32,
    /// page fade (0..1)
    pub alpha: f32,
    /// false while a page is animating out: draw, but don't react
    pub enabled: bool,
}

impl Sc<'_> {
    pub fn white(&self, a: f32) -> Color32 {
        Color32::from_white_alpha((255.0 * a * self.alpha).clamp(0.0, 255.0) as u8)
    }

    pub fn fade(&self, c: Color32) -> Color32 {
        c.gamma_multiply(self.alpha)
    }

    pub fn glass(&self, painter: &Painter, rect: Rect, g: Glass) {
        self.gs.paint(painter, rect, g.opacity(g.opacity * self.alpha));
    }

    fn sense(&self, s: Sense) -> Sense {
        if self.enabled { s } else { Sense::hover() }
    }
}

/// Text with a soft shadow so it stays readable on any wallpaper.
pub fn text(p: &Painter, pos: Pos2, align: Align2, s: &str, size: f32, color: Color32) -> Rect {
    let font = FontId::proportional(size);
    let shadow = Color32::from_black_alpha((color.a() as f32 * 0.22) as u8);
    p.text(pos + vec2(0.0, (size * 0.012).clamp(1.0, 2.5)), align, s, font.clone(), shadow);
    p.text(pos, align, s, font, color)
}

pub fn text_width(p: &Painter, s: &str, size: f32) -> f32 {
    p.layout_no_wrap(s.to_owned(), FontId::proportional(size), Color32::WHITE).size().x
}

// ---------------------------------------------------------------------------
// Icons, drawn as strokes so they scale cleanly.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Home,
    Wifi,
    Ethernet,
    Sliders,
    Clock,
    Thermometer,
    Chip,
    Globe,
}

pub fn icon(p: &Painter, icon: Icon, c: Pos2, size: f32, color: Color32) {
    let s = size / 24.0; // icons are designed on a 24x24 grid
    let w = (1.9 * s).max(1.0);
    let st = Stroke::new(w, color);
    let at = |x: f32, y: f32| pos2(c.x + (x - 12.0) * s, c.y + (y - 12.0) * s);
    let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| -> Vec<Pos2> {
        (0..=24)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / 24.0;
                at(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    };
    use std::f32::consts::PI;
    match icon {
        Icon::Home => {
            p.add(Shape::line(vec![at(3.0, 11.5), at(12.0, 3.5), at(21.0, 11.5)], st));
            p.add(Shape::line(vec![at(5.5, 9.5), at(5.5, 20.5), at(18.5, 20.5), at(18.5, 9.5)], st));
            p.add(Shape::line(vec![at(10.0, 20.5), at(10.0, 14.5), at(14.0, 14.5), at(14.0, 20.5)], st));
        }
        Icon::Wifi => {
            for r in [5.0, 10.0, 15.0] {
                p.add(Shape::line(arc(12.0, 20.0, r, -PI * 0.78, -PI * 0.22), st));
            }
            p.circle_filled(at(12.0, 19.5), 1.6 * s, color);
        }
        Icon::Ethernet => {
            p.rect_stroke(Rect::from_min_max(at(4.0, 8.0), at(20.0, 20.0)), CornerRadius::same((2.0 * s) as u8), st, egui::StrokeKind::Middle);
            p.add(Shape::line(vec![at(8.0, 8.0), at(8.0, 4.0), at(16.0, 4.0), at(16.0, 8.0)], st));
            for x in [8.5, 12.0, 15.5] {
                p.line_segment([at(x, 12.5), at(x, 16.0)], st);
            }
        }
        Icon::Sliders => {
            for (y, k) in [(6.0, 15.0), (12.0, 8.0), (18.0, 13.0)] {
                p.line_segment([at(3.5, y), at(20.5, y)], st);
                p.circle_filled(at(k, y), 2.8 * s, color);
            }
        }
        Icon::Clock => {
            p.circle_stroke(at(12.0, 12.0), 8.5 * s, st);
            p.add(Shape::line(vec![at(12.0, 7.0), at(12.0, 12.0), at(15.5, 14.0)], st));
        }
        Icon::Thermometer => {
            p.add(Shape::line(vec![at(10.0, 14.5), at(10.0, 4.5), at(14.0, 4.5), at(14.0, 14.5)], st));
            p.circle_stroke(at(12.0, 17.5), 3.8 * s, st);
            p.line_segment([at(12.0, 9.0), at(12.0, 17.0)], st);
        }
        Icon::Chip => {
            p.rect_stroke(Rect::from_min_max(at(6.0, 6.0), at(18.0, 18.0)), CornerRadius::same((2.0 * s) as u8), st, egui::StrokeKind::Middle);
            for v in [9.5, 14.5] {
                p.line_segment([at(v, 2.5), at(v, 6.0)], st);
                p.line_segment([at(v, 18.0), at(v, 21.5)], st);
                p.line_segment([at(2.5, v), at(6.0, v)], st);
                p.line_segment([at(18.0, v), at(21.5, v)], st);
            }
        }
        Icon::Globe => {
            p.circle_stroke(at(12.0, 12.0), 8.5 * s, st);
            p.line_segment([at(3.5, 12.0), at(20.5, 12.0)], st);
            p.add(Shape::line(arc(19.0, 12.0, 9.5, PI * 0.72, PI * 1.28), st));
            p.add(Shape::line(arc(5.0, 12.0, 9.5, -PI * 0.28, PI * 0.28), st));
        }
    }
}

// ---------------------------------------------------------------------------

fn press_anim(ui: &Ui, id: Id, r: &Response) -> f32 {
    ui.ctx().animate_bool_with_time(id.with("press"), r.is_pointer_button_down_on(), 0.12)
}

/// Capsule button. `accent` tints the glass.
pub fn button(ui: &mut Ui, sc: Sc, rect: Rect, id: Id, label: &str, accent: Option<Color32>) -> Response {
    let r = ui.interact(rect, id, sc.sense(Sense::click()));
    let press = press_anim(ui, id, &r);
    let hover = ui.ctx().animate_bool_with_time(id.with("hover"), r.hovered(), 0.15);
    let rr = Rect::from_center_size(rect.center(), rect.size() * (1.0 - 0.04 * press) + Vec2::splat(sc.u * 4.0 * press));
    let p = ui.painter();
    let mut g = Glass::control(rr.height() * 0.5).highlight(1.0 + 0.4 * hover + 0.6 * press);
    if let Some(c) = accent {
        g = g.tinted(c, 0.55 + 0.1 * press);
    } else {
        g = g.tinted(Color32::WHITE, 0.10 + 0.08 * press);
    }
    sc.glass(p, rr, g);
    text(p, rr.center(), Align2::CENTER_CENTER, label, 19.0 * sc.u, sc.white(1.0));
    r
}

/// iOS-style switch; the knob turns into a clear lens while pressed.
pub fn toggle(ui: &mut Ui, sc: Sc, rect: Rect, id: Id, on: &mut bool) -> Response {
    let mut r = ui.interact(rect, id, sc.sense(Sense::click()));
    if r.clicked() {
        *on = !*on;
        r.mark_changed();
    }
    let t = ui.ctx().animate_bool_with_time(id.with("on"), *on, 0.22);
    let press = press_anim(ui, id, &r);
    let p = ui.painter();
    let track = rect;
    let tint = Color32::from_white_alpha(255).lerp_to_gamma(GREEN, t);
    sc.glass(p, track, Glass::control(track.height() * 0.5).tinted(tint, 0.12 + 0.62 * t).frost(0.6).no_shadow());

    let kh = track.height() - 6.0 * sc.u;
    let kw = kh * 1.45;
    let x0 = track.left() + 3.0 * sc.u + kw * 0.5;
    let x1 = track.right() - 3.0 * sc.u - kw * 0.5;
    let c = pos2(x0 + (x1 - x0) * t, track.center().y);
    let grow = 1.0 + 0.28 * press;
    let knob = Rect::from_center_size(c, vec2(kw * grow, kh * grow));
    let solid = 0.85 * (1.0 - press);
    sc.glass(p, knob, Glass::lens(knob.height() * 0.5).tinted(Color32::WHITE, 0.08 + solid));
    r
}

/// Horizontal slider for a value in 0..1; the thumb becomes a lens while dragged.
pub fn slider(ui: &mut Ui, sc: Sc, rect: Rect, id: Id, value: &mut f32) -> Response {
    let mut r = ui.interact(rect, id, sc.sense(Sense::click_and_drag()));
    let thumb_w = 38.0 * sc.u;
    let left = rect.left() + thumb_w * 0.5;
    let right = rect.right() - thumb_w * 0.5;
    if (r.dragged() || r.clicked())
        && let Some(pos) = r.interact_pointer_pos()
    {
        let v = ((pos.x - left) / (right - left)).clamp(0.0, 1.0);
        if v != *value {
            *value = v;
            r.mark_changed();
        }
    }
    let active = ui.ctx().animate_bool_with_time(id.with("drag"), r.is_pointer_button_down_on(), 0.14);
    let p = ui.painter();
    let cy = rect.center().y;
    let th = 7.0 * sc.u;
    let track = Rect::from_min_max(pos2(left, cy - th * 0.5), pos2(right, cy + th * 0.5));
    let cr = CornerRadius::same((th * 0.5) as u8);
    p.rect_filled(track, cr, sc.white(0.22));
    let x = left + (right - left) * *value;
    p.rect_filled(Rect::from_min_max(track.min, pos2(x, track.max.y)), cr, sc.fade(Color32::from_white_alpha(235)));

    let kh = 26.0 * sc.u * (1.0 + 0.35 * active);
    let kw = thumb_w * (1.0 + 0.35 * active);
    let knob = Rect::from_center_size(pos2(x, cy), vec2(kw, kh));
    sc.glass(p, knob, Glass::lens(kh * 0.5).tinted(Color32::WHITE, 0.08 + 0.85 * (1.0 - active)));
    r
}

/// Segmented control with a sliding lens. Returns true when the selection changed.
pub fn segmented(ui: &mut Ui, sc: Sc, rect: Rect, id: Id, labels: &[&str], selected: &mut usize) -> bool {
    let n = labels.len().max(1);
    let seg_w = rect.width() / n as f32;
    let mut changed = false;
    for i in 0..n {
        let seg = Rect::from_min_size(pos2(rect.left() + seg_w * i as f32, rect.top()), vec2(seg_w, rect.height()));
        if ui.interact(seg, id.with(i), sc.sense(Sense::click())).clicked() && *selected != i {
            *selected = i;
            changed = true;
        }
    }
    let pos = ui.ctx().animate_value_with_time(id.with("sel"), *selected as f32, 0.28);
    let p = ui.painter();
    sc.glass(p, rect, Glass::control(rect.height() * 0.5).frost(0.7));
    let inset = 4.0 * sc.u;
    let pill = Rect::from_min_size(pos2(rect.left() + seg_w * pos + inset, rect.top() + inset), vec2(seg_w - 2.0 * inset, rect.height() - 2.0 * inset));
    sc.glass(p, pill, Glass::lens(pill.height() * 0.5).tinted(Color32::WHITE, 0.22).no_shadow());
    for (i, l) in labels.iter().enumerate() {
        let c = pos2(rect.left() + seg_w * (i as f32 + 0.5), rect.center().y);
        let near = 1.0 - (pos - i as f32).abs().min(1.0);
        text(p, c, Align2::CENTER_CENTER, l, 17.0 * sc.u, sc.white(0.72 + 0.28 * near));
    }
    changed
}

/// Floating tab bar with icon + label per item and a sliding glass selection.
pub fn tab_bar(ui: &mut Ui, sc: Sc, rect: Rect, id: Id, items: &[(Icon, &str)], selected: &mut usize) {
    let n = items.len().max(1);
    let inset = 6.0 * sc.u;
    let item_w = (rect.width() - 2.0 * inset) / n as f32;
    for i in 0..n {
        let r = Rect::from_min_size(pos2(rect.left() + inset + item_w * i as f32, rect.top()), vec2(item_w, rect.height()));
        if ui.interact(r, id.with(i), sc.sense(Sense::click())).clicked() {
            *selected = i;
        }
    }
    let pos = ui.ctx().animate_value_with_time(id.with("sel"), *selected as f32, 0.32);
    let p = ui.painter();
    sc.glass(p, rect, Glass::panel(rect.height() * 0.5).frost(0.55));
    let pill = Rect::from_min_size(
        pos2(rect.left() + inset + item_w * pos, rect.top() + inset),
        vec2(item_w, rect.height() - 2.0 * inset),
    );
    sc.glass(p, pill, Glass::lens(pill.height() * 0.5).tinted(Color32::WHITE, 0.20).no_shadow());
    for (i, (ic, label)) in items.iter().enumerate() {
        let cx = rect.left() + inset + item_w * (i as f32 + 0.5);
        let near = 1.0 - (pos - i as f32).abs().min(1.0);
        let col = sc.white(0.70 + 0.30 * near);
        icon(p, *ic, pos2(cx, rect.center().y - 9.0 * sc.u), 24.0 * sc.u, col);
        text(p, pos2(cx, rect.center().y + 15.0 * sc.u), Align2::CENTER_CENTER, label, 14.0 * sc.u, col);
    }
}

/// Small rounded "status pill".
pub fn pill(ui: &Ui, sc: Sc, right_center: Pos2, label: &str, color: Color32) -> Rect {
    let p = ui.painter();
    let size = 15.0 * sc.u;
    let w = text_width(p, label, size) + 26.0 * sc.u;
    let rect = Rect::from_min_size(pos2(right_center.x - w, right_center.y - 14.0 * sc.u), vec2(w, 28.0 * sc.u));
    sc.glass(p, rect, Glass::control(rect.height() * 0.5).tinted(color, 0.55).frost(0.5).no_shadow());
    text(p, rect.center(), Align2::CENTER_CENTER, label, size, sc.white(1.0));
    rect
}
