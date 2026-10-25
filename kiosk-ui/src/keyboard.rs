//! An on-screen keyboard for the touchscreen: letters, two symbol pages, shift and
//! backspace, and a row of action buttons next to the space bar. It only edits a
//! `String`; the dialog that owns the string decides what the actions do.

use egui::{Align2, Color32, Id, Rect, Sense, Ui, pos2, vec2};

use kiosk_kit::Lay;
use kiosk_kit::glass::Glass;
use kiosk_kit::widgets::{self as w, ACCENT, Icon, Sc, text};

const LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
/// every printable ASCII character is on one of the three pages
const SYMBOLS: [[&str; 3]; 2] = [["1234567890", "-/:;()$&@", ".,?!'\"#"], ["[]{}#%^*+=", "_\\|~<>`'\"", ".,?!-:;"]];

/// A button in the bottom row, right of the space bar.
pub struct Action<'a> {
    pub label: &'a str,
    pub accent: bool,
    pub enabled: bool,
}

#[derive(Default)]
pub struct Keyboard {
    shift: bool,
    /// 0 letters, 1 and 2 symbols
    page: usize,
}

enum Face<'a> {
    Text(&'a str),
    Icon(Icon),
}

/// Draws one key and says whether it was tapped.
#[allow(clippy::too_many_arguments)]
fn key(ui: &mut Ui, sc: Sc, r: Rect, id: Id, face: Face, size: f32, accent: Option<Color32>, quiet: bool) -> bool {
    let resp = ui.interact(r, id, if sc.enabled { Sense::click() } else { Sense::hover() });
    let press = ui.ctx().animate_bool_with_time(id.with("press"), resp.is_pointer_button_down_on(), 0.08);
    let p = ui.painter();
    let radius = (r.height() * 0.3).min(r.width() * 0.5);
    let mut g = Glass::control(radius).highlight(1.0 + 0.6 * press).no_shadow();
    g = match accent {
        Some(c) => g.tinted(c, 0.6),
        None if quiet => g.tinted(Color32::WHITE, 0.0).opacity(0.55),
        None => g,
    };
    sc.glass(p, r, g);
    match face {
        Face::Text(t) => {
            text(p, r.center(), Align2::CENTER_CENTER, t, size, sc.white(1.0));
        }
        Face::Icon(i) => w::icon(p, i, r.center(), size * 1.2, sc.white(1.0)),
    }
    resp.clicked()
}

impl Keyboard {
    /// Draws the keyboard in `area` (four rows fill its height) and edits `value`, at most `max` bytes.
    /// Returns the index of the action that was tapped, if any.
    #[allow(clippy::too_many_arguments)]
    pub fn ui(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, area: Rect, value: &mut String, max: usize, actions: &[Action]) -> Option<usize> {
        let gap = l.v(8.0, 6.0);
        let cell = (area.width() + gap) / 10.0;
        let key_h = (area.height() - 3.0 * gap) / 4.0;
        let size = (key_h * 0.42).min(l.v(30.0, 24.0));
        let row_y = |row: usize| area.top() + row as f32 * (key_h + gap);
        let slot = |row: usize, start: f32, units: f32| {
            Rect::from_min_size(pos2(area.left() + start * cell, row_y(row)), vec2(units * cell - gap, key_h))
        };
        let id = Id::new("kbd");
        let mut typed: Option<char> = None;

        let rows: [&str; 3] = if self.page == 0 { LETTERS } else { SYMBOLS[self.page - 1] };
        for (row, keys) in rows.iter().enumerate() {
            let n = keys.chars().count() as f32;
            // the second row is centred, the third sits between shift and backspace
            let start = match row {
                2 => 1.5,
                _ => (10.0 - n) * 0.5,
            };
            for (i, c) in keys.chars().enumerate() {
                let r = slot(row, start + i as f32, 1.0);
                let shown = if self.page == 0 && self.shift { c.to_ascii_uppercase() } else { c };
                if key(ui, sc, r, id.with((row, i, self.page)), Face::Text(&shown.to_string()), size, None, false) {
                    typed = Some(shown);
                }
            }
        }

        // third row: shift (or the symbol page switch) and backspace
        let left = slot(2, 0.0, 1.5);
        let shift_label = match self.page {
            0 => "Shift",
            1 => "#+=",
            _ => "123",
        };
        if key(ui, sc, left, id.with("shift"), Face::Text(shift_label), size * 0.7, self.shift.then_some(ACCENT).filter(|_| self.page == 0), true) {
            match self.page {
                0 => self.shift = !self.shift,
                1 => self.page = 2,
                _ => self.page = 1,
            }
        }
        if key(ui, sc, slot(2, 8.5, 1.5), id.with("back"), Face::Icon(Icon::Backspace), size, None, true) {
            value.pop();
        }

        // bottom row: page switch, space, actions
        let aw = 2.0;
        let space = 10.0 - 1.5 - aw * actions.len() as f32;
        let page_label = if self.page == 0 { "?123" } else { "ABC" };
        if key(ui, sc, slot(3, 0.0, 1.5), id.with("page"), Face::Text(page_label), size * 0.7, None, true) {
            self.page = if self.page == 0 { 1 } else { 0 };
            self.shift = false;
        }
        if key(ui, sc, slot(3, 1.5, space), id.with("space"), Face::Text(""), size, None, false) {
            typed = Some(' ');
        }
        let mut done = None;
        for (i, a) in actions.iter().enumerate() {
            let r = slot(3, 1.5 + space + aw * i as f32, aw);
            let sc_a = Sc { enabled: sc.enabled && a.enabled, alpha: sc.alpha * if a.enabled { 1.0 } else { 0.4 }, ..sc };
            if key(ui, sc_a, r, id.with(("action", i)), Face::Text(a.label), size * 0.75, a.accent.then_some(ACCENT), !a.accent) {
                done = Some(i);
            }
        }

        if let Some(c) = typed
            && value.len() + c.len_utf8() <= max
        {
            value.push(c);
            self.shift = false;
        }
        done
    }
}
