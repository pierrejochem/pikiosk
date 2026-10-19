//! Offscreen rendering to PNG through EGL "surfaceless" (Mesa), for
//! screenshots and testing without a display. Works on the Pi too.

use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use glow::HasContext as _;
use khronos_egl as egl;

use crate::app::KioskApp;
use crate::gfx::{Gfx, pixels_per_point};

/// An offscreen GL context with a bound framebuffer of the given size.
pub struct Offscreen {
    pub gfx: Gfx,
    pub size: [u32; 2],
    // keep the EGL library loaded for the lifetime of the context
    _egl: egl::DynamicInstance<egl::EGL1_5>,
}

impl Offscreen {
    pub fn new(size: [u32; 2]) -> anyhow::Result<Self> {
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }.map_err(|e| anyhow!("loading libEGL: {e}"))?;
        const PLATFORM_SURFACELESS_MESA: egl::Enum = 0x31DD;
        let display = unsafe { egl.get_platform_display(PLATFORM_SURFACELESS_MESA, std::ptr::null_mut(), &[egl::ATTRIB_NONE]) }?;
        egl.initialize(display)?;
        egl.bind_api(egl::OPENGL_ES_API)?;
        let config = egl
            .choose_first_config(display, &[egl::SURFACE_TYPE, egl::PBUFFER_BIT, egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT, egl::NONE])?
            .ok_or_else(|| anyhow!("no EGL config"))?;
        let context = egl.create_context(display, config, None, &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE])?;
        egl.make_current(display, None, None, Some(context))?;
        let gl = unsafe {
            glow::Context::from_loader_function(|name| egl.get_proc_address(name).map(|f| f as *const c_void).unwrap_or(std::ptr::null()))
        };
        let gl = Arc::new(gl);
        let [w, h] = size;
        unsafe {
            let tex = gl.create_texture().map_err(|e| anyhow!(e))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            let fbo = gl.create_framebuffer().map_err(|e| anyhow!(e))?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        Ok(Self { gfx: Gfx::new(gl)?, size, _egl: egl })
    }

    /// RGBA8 pixels, rows top-down, opaque.
    pub fn read(&self) -> Vec<u8> {
        let [w, h] = self.size;
        let mut px = vec![0u8; (w * h * 4) as usize];
        unsafe {
            self.gfx.gl.read_pixels(0, 0, w as i32, h as i32, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelPackData::Slice(Some(&mut px)));
        }
        let row = (w * 4) as usize;
        let mut img = Vec::with_capacity(px.len());
        for y in (0..h as usize).rev() {
            img.extend_from_slice(&px[y * row..(y + 1) * row]);
        }
        img.chunks_exact_mut(4).for_each(|p| p[3] = 255);
        img
    }
}

pub fn screenshot(mut app: KioskApp, out: &Path, size: [u32; 2], pages: &[usize]) -> anyhow::Result<()> {
    let mut off = Offscreen::new(size)?;
    let [w, h] = size;
    let ppp = pixels_per_point(h);
    let names = ["home", "network", "controls", "settings"];
    let mut t = 12.0;
    for &page in pages {
        app.page = page;
        // let page transitions and animations settle
        for _ in 0..40 {
            off.gfx.frame(&mut app, size, ppp, vec![], t);
            t += 1.0 / 30.0;
        }
        let img = off.read();
        let path = if pages.len() > 1 {
            let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("screenshot");
            out.with_file_name(format!("{stem}-{}.png", names[page]))
        } else {
            out.to_path_buf()
        };
        image::save_buffer(&path, &img, w, h, image::ColorType::Rgba8).with_context(|| format!("writing {}", path.display()))?;
        println!("{}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, pos2};

    fn click(off: &mut Offscreen, app: &mut KioskApp, t: &mut f64, from: egui::Pos2, to: egui::Pos2) {
        let size = off.size;
        let ev = |e: Vec<Event>| e;
        let mut step = |off: &mut Offscreen, app: &mut KioskApp, events: Vec<Event>| {
            off.gfx.frame(app, size, 1.0, events, *t);
            *t += 1.0 / 30.0;
        };
        step(off, app, ev(vec![Event::PointerMoved(from)]));
        step(off, app, ev(vec![Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]));
        for i in 1..=8 {
            let p = from + (to - from) * (i as f32 / 8.0);
            step(off, app, ev(vec![Event::PointerMoved(p)]));
        }
        step(off, app, ev(vec![Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]));
        for _ in 0..15 {
            step(off, app, vec![]);
        }
    }

    /// Drives the real UI with pointer input at 1280x720 (scale 1).
    #[test]
    fn widgets_respond_to_input() {
        unsafe { std::env::remove_var("KIOSK_DATA") };
        let mut off = Offscreen::new([1280, 720]).expect("EGL surfaceless context (needs Mesa)");
        let mut app = KioskApp::new();
        let mut t = 1.0;
        for _ in 0..3 {
            off.gfx.frame(&mut app, off.size, 1.0, vec![], t);
            t += 0.03;
        }

        // tab bar: 480 wide, four items; the fourth is "Settings"
        let tab = |i: f32| pos2(640.0 - 240.0 + 6.0 + (480.0 - 12.0) / 4.0 * (i + 0.5), 720.0 - 24.0 - 35.0);
        click(&mut off, &mut app, &mut t, tab(3.0), tab(3.0));
        assert_eq!(app.page, 3, "tab bar should switch to Settings");

        // "Show frame rate" toggle, first row of the System card
        assert!(!app.settings.show_fps);
        let toggle = pos2(1075.0, 84.0 + 112.0 + 8.0);
        click(&mut off, &mut app, &mut t, toggle, toggle);
        assert!(app.settings.show_fps, "toggle should switch on");

        click(&mut off, &mut app, &mut t, tab(2.0), tab(2.0));
        assert_eq!(app.page, 2, "tab bar should switch to Controls");

        // drag the Frost slider all the way to the right
        let thumb = pos2(170.0 - 6.0 + 19.0 + (604.0 - 164.0 - 38.0) * 0.625, 84.0 + 112.0 + 38.0);
        click(&mut off, &mut app, &mut t, thumb, pos2(700.0, thumb.y));
        assert!(app.settings.frost > 0.99, "frost slider should reach 100%, got {}", app.settings.frost);

        // segmented control: "Ocean"
        let ocean = pos2(164.0 + (440.0 / 4.0) * 2.5, 84.0 + 112.0 + 2.0 * 98.0 + 47.0);
        click(&mut off, &mut app, &mut t, ocean, ocean);
        assert_eq!(app.settings.theme, crate::glass::Theme::Ocean);

        // swipe right goes back to Network
        click(&mut off, &mut app, &mut t, pos2(640.0, 600.0), pos2(900.0, 600.0));
        assert_eq!(app.page, 1, "swipe right should go to the previous page");

        // the frame actually contains glass: tab bar centre differs from a flat colour
        let img = off.read();
        let px = |x: usize, y: usize| &img[(y * 1280 + x) * 4..(y * 1280 + x) * 4 + 3];
        assert_ne!(px(640, 661), px(5, 5));
    }

    /// The compact 800x480 layout (Waveshare 5" DSI) with a fake backlight.
    #[test]
    fn compact_layout_and_backlight() {
        let dir = std::env::temp_dir().join(format!("kiosk-bl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("max_brightness"), "255\n").unwrap();
        std::fs::write(dir.join("brightness"), "255\n").unwrap();
        unsafe {
            std::env::remove_var("KIOSK_DATA");
            std::env::set_var("KIOSK_BACKLIGHT", &dir);
        }
        let mut off = Offscreen::new([800, 480]).expect("EGL surfaceless context (needs Mesa)");
        let mut app = KioskApp::new();
        unsafe { std::env::remove_var("KIOSK_BACKLIGHT") };
        let mut t = 1.0;
        for _ in 0..3 {
            off.gfx.frame(&mut app, off.size, 1.0, vec![], t);
            t += 0.03;
        }

        // compact tab bar: 420 wide, 10 from the bottom; third item is Controls
        let tab = pos2(190.0 + 5.16 + 102.42 * 2.5, 440.0);
        click(&mut off, &mut app, &mut t, tab, tab);
        assert_eq!(app.page, 2);

        // drag the brightness thumb from 100% to about 20%
        click(&mut off, &mut app, &mut t, pos2(746.0, 152.0), pos2(500.0, 152.0));
        let b = app.settings.brightness;
        assert!((0.15..0.25).contains(&b), "brightness {b}");
        let written: u32 = std::fs::read_to_string(dir.join("brightness")).unwrap().trim().parse().unwrap();
        assert_eq!(written, ((0.05 + 0.95 * b) * 255.0).round() as u32);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Settings tab -> touch calibration on the 800x480 layout: tap the five
    /// targets on a "screen" that reads too small and shifted, check the
    /// fitted matrix, keep it, then let a second run time out and revert.
    #[test]
    fn touch_calibration_flow() {
        use crate::calibrate::{self, TARGETS, TouchRequest};
        unsafe { std::env::remove_var("KIOSK_DATA") };
        let mut off = Offscreen::new([800, 480]).expect("EGL surfaceless context (needs Mesa)");
        let mut app = KioskApp::new();
        let mut t = 1.0;
        for _ in 0..3 {
            off.gfx.frame(&mut app, off.size, 1.0, vec![], t);
            t += 0.03;
        }
        let tap = |off: &mut Offscreen, app: &mut KioskApp, t: &mut f64, p: egui::Pos2| click(off, app, t, p, p);

        // Settings: fourth tab of the 420 wide compact bar
        tap(&mut off, &mut app, &mut t, pos2(190.0 + 5.16 + 102.42 * 3.5, 440.0));
        assert_eq!(app.page, 3, "tab bar should switch to Settings");

        // "Calibrate" button, bottom left of the Touch screen card
        let calibrate_btn = pos2(122.0, 355.0);
        tap(&mut off, &mut app, &mut t, calibrate_btn);
        assert!(app.calibrating(), "Calibrate should open the calibration screen");
        assert_eq!(app.touch_request.take(), Some(TouchRequest::Matrix(calibrate::IDENTITY)), "raw touches while calibrating");

        // a panel that reports touches 10% too small and shifted by (20, 12) px
        let raw = |x: f32, y: f32| pos2(x * 0.9 + 20.0, y * 0.9 + 12.0);
        for tg in TARGETS {
            tap(&mut off, &mut app, &mut t, raw(tg[0] * 800.0, tg[1] * 480.0));
        }
        let m = match app.touch_request.take() {
            Some(TouchRequest::Matrix(m)) => m,
            r => panic!("expected the fitted matrix to be applied for testing, got {r:?}"),
        };
        let r = raw(400.0, 240.0);
        let back = calibrate::apply(&m, [r.x / 800.0, r.y / 480.0]);
        assert!((back[0] - 0.5).abs() < 0.002 && (back[1] - 0.5).abs() < 0.002, "centre maps to {back:?}");
        assert_eq!(app.settings.touch_matrix, None, "nothing is saved before Keep");

        // Keep (right of the two buttons at the bottom)
        let keep = pos2(488.0, 420.0);
        tap(&mut off, &mut app, &mut t, keep);
        assert!(!app.calibrating());
        assert_eq!(app.settings.touch_matrix, Some(m));
        assert_eq!(app.touch_request.take(), None, "the kept matrix is already applied");

        // second run: tap the targets, then don't confirm; it reverts after 15 s
        tap(&mut off, &mut app, &mut t, calibrate_btn);
        assert!(app.calibrating());
        app.touch_request.take();
        for tg in TARGETS {
            tap(&mut off, &mut app, &mut t, pos2(tg[0] * 800.0, tg[1] * 480.0));
        }
        assert!(matches!(app.touch_request.take(), Some(TouchRequest::Matrix(_))));
        for _ in 0..16 * 30 {
            off.gfx.frame(&mut app, off.size, 1.0, vec![], t);
            t += 1.0 / 30.0;
        }
        assert!(!app.calibrating(), "unconfirmed calibration should time out");
        assert_eq!(app.touch_request.take(), Some(TouchRequest::Matrix(m)), "restores the kept calibration");
        assert_eq!(app.settings.touch_matrix, Some(m));

        // "Reset" next to Calibrate: back to the device default
        tap(&mut off, &mut app, &mut t, pos2(294.0, 355.0));
        assert_eq!(app.settings.touch_matrix, None);
        assert_eq!(app.touch_request.take(), Some(TouchRequest::Default));
    }
}
