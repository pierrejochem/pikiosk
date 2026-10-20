//! Touchscreen calibration: the math (an affine fit in libinput's normalised
//! coordinates) and the fullscreen "tap the cross" flow.

use egui::{Align2, Color32, Event, Id, Key, PointerButton, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::layout::Lay;
use crate::widgets::{self as w, ACCENT, GREEN, Sc, text};

/// Where the crosses appear, normalised: the corners inset by 12%, then the centre.
pub const TARGETS: [[f32; 2]; 5] = [[0.12, 0.12], [0.88, 0.12], [0.88, 0.88], [0.12, 0.88], [0.5, 0.5]];
/// seconds without a tap before an unfinished calibration is abandoned
const COLLECT_TIMEOUT: f64 = 30.0;
/// seconds to confirm a new calibration before it is reverted
pub const VERIFY_TIMEOUT: f64 = 15.0;
/// reject a fit whose taps miss their targets by more than this (normalised)
const MAX_RESIDUAL: f32 = 0.06;

/// What the input backend should apply to the touchscreen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchRequest {
    /// the device's default matrix (from the udev rule, or identity)
    Default,
    Matrix(Matrix),
}

pub enum Outcome {
    Running,
    /// confirmed; the matrix is already applied
    Keep(Matrix),
    /// cancelled or timed out: restore the previous calibration
    Revert,
}

enum Step {
    Collect,
    Verify(Matrix),
}

/// The fullscreen calibration flow. While it runs the backend applies the
/// identity matrix, so the taps it collects are raw touch positions.
pub struct Calibration {
    step: Step,
    taps: Vec<[f32; 2]>,
    /// positions of the touch in progress
    press: Option<Vec<Pos2>>,
    /// time of the last tap or step change, for the timeouts
    since: f64,
    request: Option<TouchRequest>,
    /// where touches landed on the verify screen
    dots: Vec<Pos2>,
    message: Option<&'static str>,
}

impl Calibration {
    pub fn new(time: f64) -> Self {
        Self {
            step: Step::Collect,
            taps: Vec::new(),
            press: None,
            since: time,
            request: Some(TouchRequest::Matrix(IDENTITY)),
            dots: Vec::new(),
            message: None,
        }
    }

    /// A matrix change the backend should apply, once.
    pub fn take_request(&mut self) -> Option<TouchRequest> {
        self.request.take()
    }

    /// Touches released this frame, each as the median of its positions
    /// (a fingertip on a resistive panel wanders while pressed).
    fn released(&mut self, ui: &Ui) -> Vec<Pos2> {
        let mut done = Vec::new();
        ui.input(|i| {
            for e in &i.events {
                match e {
                    Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, .. } => self.press = Some(vec![*pos]),
                    Event::PointerMoved(p) => {
                        if let Some(v) = &mut self.press {
                            v.push(*p);
                        }
                    }
                    Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, .. } => {
                        if let Some(mut v) = self.press.take() {
                            v.push(*pos);
                            done.push(median(&v));
                        }
                    }
                    _ => {}
                }
            }
        });
        done
    }

    pub fn ui(&mut self, ui: &mut Ui, l: &Lay, sc: Sc, screen: Rect, time: f64) -> Outcome {
        let released = self.released(ui);
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            return Outcome::Revert;
        }
        let norm = |q: Pos2| [(q.x - screen.left()) / screen.width(), (q.y - screen.top()) / screen.height()];
        let at = |t: [f32; 2]| pos2(screen.left() + t[0] * screen.width(), screen.top() + t[1] * screen.height());

        match self.step {
            Step::Collect => {
                if !released.is_empty() {
                    self.since = time;
                }
                self.taps.extend(released.iter().map(|q| norm(*q)));
                if self.taps.len() >= TARGETS.len() {
                    self.taps.truncate(TARGETS.len());
                    match fit(&self.taps, &TARGETS).filter(|m| residual(m, &self.taps) <= MAX_RESIDUAL) {
                        Some(m) => {
                            self.step = Step::Verify(m);
                            self.request = Some(TouchRequest::Matrix(m));
                            self.dots.clear();
                            self.message = None;
                        }
                        None => {
                            self.taps.clear();
                            self.message = Some("Those taps didn't line up. Let's try again.");
                        }
                    }
                } else if time - self.since > COLLECT_TIMEOUT {
                    return Outcome::Revert;
                }
            }
            Step::Verify(_) => {
                self.dots.extend(released);
                if time - self.since > VERIFY_TIMEOUT {
                    return Outcome::Revert;
                }
            }
        }

        let p = ui.painter().clone();
        p.rect_filled(screen, 0, Color32::from_black_alpha(225));
        let cx = screen.center().x;
        let ty = screen.top() + screen.height() * 0.32;
        match self.step {
            Step::Collect => {
                let c = at(TARGETS[self.taps.len()]);
                let pulse = ((time * 3.0).sin() * 0.5 + 0.5) as f32;
                let ring = Color32::from_rgba_unmultiplied(10, 132, 255, (220.0 * (1.0 - 0.6 * pulse)) as u8);
                p.circle_stroke(c, l.v(26.0, 22.0) * (1.0 + 0.35 * pulse), Stroke::new(2.0, ring));
                crosshair(&p, c, l.v(36.0, 30.0), Color32::WHITE);
                text(&p, pos2(cx, ty), Align2::CENTER_CENTER, "Touch calibration", l.v(30.0, 22.0), Color32::WHITE);
                let hint = self.message.unwrap_or("Tap the centre of the cross");
                text(&p, pos2(cx, ty + l.v(40.0, 30.0)), Align2::CENTER_CENTER, hint, l.v(19.0, 15.0), Color32::from_white_alpha(215));
                let step = format!("{} of {}  ·  Esc or wait to cancel", self.taps.len() + 1, TARGETS.len());
                text(&p, pos2(cx, ty + l.v(72.0, 54.0)), Align2::CENTER_CENTER, &step, l.v(15.0, 13.0), Color32::from_white_alpha(140));
            }
            Step::Verify(m) => {
                for t in TARGETS {
                    crosshair(&p, at(t), l.v(22.0, 18.0), Color32::from_white_alpha(110));
                }
                for d in &self.dots {
                    p.circle_filled(*d, l.v(6.0, 5.0), GREEN);
                }
                text(&p, pos2(cx, ty), Align2::CENTER_CENTER, "Check the calibration", l.v(30.0, 22.0), Color32::WHITE);
                let hint = "Tap the crosses: the dots show where touches land";
                text(&p, pos2(cx, ty + l.v(40.0, 30.0)), Align2::CENTER_CENTER, hint, l.v(19.0, 15.0), Color32::from_white_alpha(215));
                let left = (VERIFY_TIMEOUT - (time - self.since)).ceil().max(0.0);
                let undo = format!("Reverting in {left:.0} s unless you keep it");
                text(&p, pos2(cx, ty + l.v(72.0, 54.0)), Align2::CENTER_CENTER, &undo, l.v(15.0, 13.0), Color32::from_white_alpha(140));

                let (bw, bh, gap) = (l.v(200.0, 160.0), l.v(52.0, 42.0), l.v(16.0, 16.0));
                let y = screen.bottom() - l.v(80.0, 60.0);
                let retry = Rect::from_center_size(pos2(cx - (gap + bw) * 0.5, y), vec2(bw, bh));
                let keep = retry.translate(vec2(bw + gap, 0.0));
                if w::button(ui, sc, retry, Id::unique("calibrate-retry"), "Retry", None).clicked() {
                    *self = Self::new(time);
                }
                if w::button(ui, sc, keep, Id::unique("calibrate-keep"), "Keep", Some(ACCENT)).clicked() {
                    return Outcome::Keep(m);
                }
            }
        }
        ui.ctx().request_repaint();
        Outcome::Running
    }
}

fn crosshair(p: &egui::Painter, c: Pos2, size: f32, color: Color32) {
    let h = size * 0.5;
    let st = Stroke::new(2.0, color);
    p.line_segment([pos2(c.x - h, c.y), pos2(c.x + h, c.y)], st);
    p.line_segment([pos2(c.x, c.y - h), pos2(c.x, c.y + h)], st);
    p.circle_stroke(c, size * 0.22, st);
}

fn median(pts: &[Pos2]) -> Pos2 {
    let mid = |mut v: Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    pos2(mid(pts.iter().map(|p| p.x).collect()), mid(pts.iter().map(|p| p.y).collect()))
}

/// Largest distance between a fitted tap and its target (normalised).
fn residual(m: &Matrix, taps: &[[f32; 2]]) -> f32 {
    taps.iter()
        .zip(TARGETS)
        .map(|(p, t)| {
            let q = apply(m, *p);
            ((q[0] - t[0]).powi(2) + (q[1] - t[1]).powi(2)).sqrt()
        })
        .fold(0.0, f32::max)
}

/// libinput calibration matrix: the first two rows of a 3x3 affine matrix,
/// applied to touch coordinates normalised to 0..1.
pub type Matrix = [f32; 6];

pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

/// Maps a normalised point through `m`.
pub fn apply(m: &Matrix, p: [f32; 2]) -> [f32; 2] {
    [m[0] * p[0] + m[1] * p[1] + m[2], m[3] * p[0] + m[4] * p[1] + m[5]]
}

/// Least-squares affine matrix that maps the `measured` points onto
/// `target`. None if the points can't determine one (fewer than 3, or all
/// on a line).
pub fn fit(measured: &[[f32; 2]], target: &[[f32; 2]]) -> Option<Matrix> {
    if measured.len() < 3 || measured.len() != target.len() {
        return None;
    }
    // normal equations: (AᵀA) m = Aᵀb, with rows of A = [x, y, 1]
    let mut ata = [[0.0f64; 3]; 3];
    let mut atb = [[0.0f64; 3]; 2];
    for (p, t) in measured.iter().zip(target) {
        let row = [p[0] as f64, p[1] as f64, 1.0];
        for i in 0..3 {
            for j in 0..3 {
                ata[i][j] += row[i] * row[j];
            }
            atb[0][i] += row[i] * t[0] as f64;
            atb[1][i] += row[i] * t[1] as f64;
        }
    }
    let det3 = |m: &[[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let det = det3(&ata);
    // collinear points make AᵀA singular
    if det.abs() < 1e-9 {
        return None;
    }
    // Cramer's rule, once per output axis
    let mut out = [0.0f32; 6];
    for (axis, b) in atb.iter().enumerate() {
        for k in 0..3 {
            let mut m = ata;
            for (i, row) in m.iter_mut().enumerate() {
                row[k] = b[i];
            }
            out[axis * 3 + k] = (det3(&m) / det) as f32;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Matrix, b: &Matrix) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn fit_recovers_a_known_matrix() {
        let m: Matrix = [1.06, -0.007, -0.038, -0.02, 1.11, -0.08];
        let measured = [[0.1, 0.12], [0.9, 0.1], [0.88, 0.9], [0.12, 0.88], [0.5, 0.52]];
        let target: Vec<[f32; 2]> = measured.iter().map(|p| apply(&m, *p)).collect();
        let got = fit(&measured, &target).expect("fit");
        assert!(close(&got, &m), "got {got:?}");
    }

    #[test]
    fn fit_of_matching_points_is_identity() {
        let pts = [[0.12, 0.12], [0.88, 0.12], [0.88, 0.88], [0.12, 0.88], [0.5, 0.5]];
        assert!(close(&fit(&pts, &pts).expect("fit"), &IDENTITY));
    }

    #[test]
    fn fit_rejects_points_on_a_line() {
        let line = [[0.1, 0.1], [0.5, 0.5], [0.9, 0.9]];
        assert_eq!(fit(&line, &line), None);
        assert_eq!(fit(&line[..2], &line[..2]), None);
    }
}
