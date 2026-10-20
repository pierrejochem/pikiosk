//! Fullscreen backend for the kiosk: DRM/KMS modesetting, GBM buffers,
//! EGL + GLES 2 rendering, page-flipped on vsync, and libinput for touch,
//! mouse and keyboard. No X11, no Wayland, no compositor.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use drm::control::{self, Device as _, ModeTypeFlags, PageFlipFlags, connector, crtc, framebuffer};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, TouchDeviceId, TouchId, TouchPhase, pos2};
use gbm::{AsRaw, BufferObject, BufferObjectFlags, Format};
use input::event::keyboard::{KeyState, KeyboardEventTrait};
use input::event::pointer::{Axis, ButtonState, PointerScrollEvent};
use input::event::touch::{TouchEventPosition, TouchEventSlot};
use input::event::{DeviceEvent, EventTrait, KeyboardEvent, PointerEvent, TouchEvent};
use input::{AsRaw as _, DeviceCapability, Libinput, LibinputInterface};
use khronos_egl as egl;

use crate::App;
use crate::calibrate::{IDENTITY, TouchRequest};
use crate::gfx::{Gfx, pixels_per_point};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl drm::Device for Card {}
impl control::Device for Card {}

struct Output {
    card: Card,
    path: String,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: control::Mode,
}

fn pick_mode(modes: &[control::Mode]) -> control::Mode {
    if let Ok(want) = std::env::var("KIOSK_MODE")
        && let Some((w, h)) = want.split_once('x')
        && let (Ok(w), Ok(h)) = (w.parse::<u16>(), h.parse::<u16>())
        && let Some(m) = modes.iter().find(|m| m.size() == (w, h))
    {
        return *m;
    }
    *modes.iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)).unwrap_or(&modes[0])
}

/// Connected display to use. A DSI panel (e.g. the Waveshare 5" DSI LCD)
/// wins over HDMI unless `KIOSK_OUTPUT=hdmi`; vc4 is card0 on a Pi 3.
fn open_output() -> anyhow::Result<Output> {
    let want_hdmi = std::env::var("KIOSK_OUTPUT").is_ok_and(|v| v.eq_ignore_ascii_case("hdmi"));
    let rank = |i: connector::Interface| match (i, want_hdmi) {
        (connector::Interface::DSI, false) | (connector::Interface::HDMIA, true) => 0,
        (connector::Interface::DSI, true) | (connector::Interface::HDMIA, false) => 1,
        _ => 2,
    };
    for n in 0..8 {
        let path = format!("/dev/dri/card{n}");
        let Ok(file) = OpenOptions::new().read(true).write(true).custom_flags(libc::O_CLOEXEC).open(&path) else { continue };
        let card = Card(file);
        let Ok(res) = card.resource_handles() else { continue };
        let mut found: Vec<(u8, connector::Handle, crtc::Handle, control::Mode)> = Vec::new();
        for &conn in res.connectors() {
            let Ok(info) = card.get_connector(conn, true) else { continue };
            if info.state() != connector::State::Connected || info.modes().is_empty() {
                continue;
            }
            let crtc = info
                .encoders()
                .iter()
                .filter_map(|e| card.get_encoder(*e).ok())
                .find_map(|e| e.crtc().or_else(|| res.filter_crtcs(e.possible_crtcs()).first().copied()));
            if let Some(crtc) = crtc {
                log::info!("output: {:?} connected", info.interface());
                found.push((rank(info.interface()), conn, crtc, pick_mode(info.modes())));
            }
        }
        if let Some(&(_, connector, crtc, mode)) = found.iter().min_by_key(|f| f.0) {
            return Ok(Output { card, path, connector, crtc, mode });
        }
    }
    bail!("no connected display found on /dev/dri/card*")
}

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let acc = flags & libc::O_ACCMODE;
        OpenOptions::new()
            .custom_flags(flags)
            .read(acc == libc::O_RDONLY || acc == libc::O_RDWR)
            .write(acc == libc::O_WRONLY || acc == libc::O_RDWR)
            .open(path)
            .map(Into::into)
            .map_err(|e| -e.raw_os_error().unwrap_or(libc::EIO))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(File::from(fd));
    }
}

/// Translates libinput events into egui events.
struct InputState {
    size_px: [u32; 2],
    ppp: f32,
    pointer: Pos2,
    mouse_visible: bool,
    primary_touch: Option<u32>,
    touch_pos: std::collections::HashMap<u32, Pos2>,
    modifiers: Modifiers,
}

impl InputState {
    fn points(&self) -> egui::Vec2 {
        egui::vec2(self.size_px[0] as f32, self.size_px[1] as f32) / self.ppp
    }

    fn handle(&mut self, ev: input::Event, out: &mut Vec<Event>) {
        let [w, h] = self.size_px;
        match ev {
            input::Event::Pointer(p) => {
                match p {
                    PointerEvent::Motion(m) => {
                        let max = self.points();
                        self.pointer.x = (self.pointer.x + m.dx() as f32 / self.ppp).clamp(0.0, max.x - 1.0);
                        self.pointer.y = (self.pointer.y + m.dy() as f32 / self.ppp).clamp(0.0, max.y - 1.0);
                        out.push(Event::PointerMoved(self.pointer));
                    }
                    PointerEvent::MotionAbsolute(m) => {
                        self.pointer = pos2(m.absolute_x_transformed(w) as f32, m.absolute_y_transformed(h) as f32) / self.ppp;
                        out.push(Event::PointerMoved(self.pointer));
                    }
                    PointerEvent::Button(b) => {
                        let button = match b.button() {
                            0x110 => PointerButton::Primary,
                            0x111 => PointerButton::Secondary,
                            0x112 => PointerButton::Middle,
                            _ => return,
                        };
                        out.push(Event::PointerButton {
                            pos: self.pointer,
                            button,
                            pressed: b.button_state() == ButtonState::Pressed,
                            modifiers: self.modifiers,
                        });
                    }
                    PointerEvent::ScrollWheel(s) => {
                        let dy = if s.has_axis(Axis::Vertical) { -s.scroll_value_v120(Axis::Vertical) as f32 / 120.0 } else { 0.0 };
                        let dx = if s.has_axis(Axis::Horizontal) { -s.scroll_value_v120(Axis::Horizontal) as f32 / 120.0 } else { 0.0 };
                        out.push(Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Line,
                            delta: egui::vec2(dx, dy),
                            phase: TouchPhase::Move,
                            source: egui::MouseWheelSource::Wheel,
                            modifiers: self.modifiers,
                        });
                    }
                    _ => return,
                }
                self.mouse_visible = true;
            }
            input::Event::Touch(t) => {
                self.mouse_visible = false;
                let (slot, phase, pos) = match &t {
                    TouchEvent::Down(e) => {
                        (e.seat_slot(), TouchPhase::Start, pos2(e.x_transformed(w) as f32, e.y_transformed(h) as f32) / self.ppp)
                    }
                    TouchEvent::Motion(e) => {
                        (e.seat_slot(), TouchPhase::Move, pos2(e.x_transformed(w) as f32, e.y_transformed(h) as f32) / self.ppp)
                    }
                    TouchEvent::Up(e) => (e.seat_slot(), TouchPhase::End, self.touch_pos.get(&e.seat_slot()).copied().unwrap_or(self.pointer)),
                    TouchEvent::Cancel(e) => {
                        (e.seat_slot(), TouchPhase::Cancel, self.touch_pos.get(&e.seat_slot()).copied().unwrap_or(self.pointer))
                    }
                    _ => return,
                };
                out.push(Event::Touch { device_id: TouchDeviceId(0), id: TouchId(slot as u64), phase, pos, force: None });
                match phase {
                    TouchPhase::Start | TouchPhase::Move => {
                        self.touch_pos.insert(slot, pos);
                    }
                    _ => {
                        self.touch_pos.remove(&slot);
                    }
                }
                // the first finger also drives the egui pointer, like a mouse
                if phase == TouchPhase::Start && self.primary_touch.is_none() {
                    self.primary_touch = Some(slot);
                    self.pointer = pos;
                    out.push(Event::PointerMoved(pos));
                    out.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: self.modifiers });
                } else if self.primary_touch == Some(slot) {
                    self.pointer = pos;
                    match phase {
                        TouchPhase::Move => out.push(Event::PointerMoved(pos)),
                        _ => {
                            out.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: self.modifiers });
                            out.push(Event::PointerGone);
                            self.primary_touch = None;
                        }
                    }
                }
            }
            input::Event::Keyboard(KeyboardEvent::Key(k)) => {
                let pressed = k.key_state() == KeyState::Pressed;
                // evdev key codes (linux/input-event-codes.h)
                match k.key() {
                    42 | 54 => self.modifiers.shift = pressed,
                    29 | 97 => {
                        self.modifiers.ctrl = pressed;
                        self.modifiers.command = pressed;
                    }
                    56 | 100 => self.modifiers.alt = pressed,
                    _ => {}
                }
                let key = match k.key() {
                    1 => Key::Escape,
                    15 => Key::Tab,
                    28 | 96 => Key::Enter,
                    57 => Key::Space,
                    103 => Key::ArrowUp,
                    105 => Key::ArrowLeft,
                    106 => Key::ArrowRight,
                    108 => Key::ArrowDown,
                    _ => return,
                };
                out.push(Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: self.modifiers });
            }
            _ => {}
        }
    }
}

/// Sets a touchscreen's libinput calibration matrix.
fn apply_touch_calibration(dev: &mut input::Device, req: TouchRequest) {
    let m = match req {
        TouchRequest::Matrix(m) => m,
        TouchRequest::Default => dev.config_calibration_default_matrix().unwrap_or(IDENTITY),
    };
    match dev.config_calibration_set_matrix(m) {
        Ok(()) => log::info!("touch calibration for {}: {m:?}", dev.name()),
        Err(e) => log::warn!("touch calibration for {}: {e:?}", dev.name()),
    }
}

pub fn run(mut app: impl App) -> anyhow::Result<()> {
    let out = open_output()?;
    let (w, h) = out.mode.size();
    let (w, h) = (w as u32, h as u32);
    log::info!("display: {} {}x{}@{}", out.path, w, h, out.mode.vrefresh());

    let gbm = gbm::Device::new(out.card).context("gbm device")?;
    let surface = gbm
        .create_surface::<framebuffer::Handle>(w, h, Format::Xrgb8888, BufferObjectFlags::SCANOUT | BufferObjectFlags::RENDERING)
        .context("gbm surface")?;

    // ---- EGL on GBM
    let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }.map_err(|e| anyhow!("loading libEGL: {e}"))?;
    const PLATFORM_GBM_KHR: egl::Enum = 0x31D7;
    let display = unsafe { egl.get_platform_display(PLATFORM_GBM_KHR, gbm.as_raw() as *mut c_void, &[egl::ATTRIB_NONE]) }?;
    egl.initialize(display)?;
    egl.bind_api(egl::OPENGL_ES_API)?;
    let attrs = [
        egl::SURFACE_TYPE, egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE, egl::OPENGL_ES2_BIT,
        egl::RED_SIZE, 8, egl::GREEN_SIZE, 8, egl::BLUE_SIZE, 8,
        egl::NONE,
    ];
    let mut configs = Vec::with_capacity(64);
    egl.choose_config(display, &attrs, &mut configs)?;
    let xrgb = Format::Xrgb8888 as i32;
    let config = configs
        .iter()
        .copied()
        .find(|c| egl.get_config_attrib(display, *c, egl::NATIVE_VISUAL_ID).ok() == Some(xrgb))
        .ok_or_else(|| anyhow!("no EGL config matching XRGB8888"))?;
    let context = egl.create_context(display, config, None, &[egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE])?;
    let egl_surface = unsafe { egl.create_platform_window_surface(display, config, surface.as_raw() as *mut c_void, &[egl::ATTRIB_NONE]) }?;
    egl.make_current(display, Some(egl_surface), Some(egl_surface), Some(context))?;

    let gl = unsafe {
        glow::Context::from_loader_function(|name| {
            egl.get_proc_address(name).map(|f| f as *const c_void).unwrap_or(std::ptr::null())
        })
    };
    let mut gfx = Gfx::new(Arc::new(gl))?;

    // ---- input
    let mut input = Libinput::new_with_udev(Interface);
    if input.udev_assign_seat("seat0").is_err() {
        log::warn!("libinput: could not assign seat0, input disabled");
    }
    let ppp = pixels_per_point(h);
    let mut ist = InputState {
        size_px: [w, h],
        ppp,
        pointer: pos2(w as f32, h as f32) / ppp * 0.5,
        mouse_visible: false,
        primary_touch: None,
        touch_pos: Default::default(),
        modifiers: Modifiers::default(),
    };

    // touchscreens with a calibration matrix, and the calibration they should use
    let mut touch_devices: Vec<input::Device> = Vec::new();
    let mut touch_cal = app.touch_calibration();

    let fps: f64 = std::env::var("KIOSK_FPS").ok().and_then(|s| s.parse().ok()).filter(|f: &f64| *f >= 1.0).unwrap_or(30.0);
    let frame_interval = Duration::from_secs_f64(1.0 / fps);
    let start = Instant::now();
    // the buffer on screen, and the one queued to replace it at the next vblank
    let mut current: Option<BufferObject<framebuffer::Handle>> = None;
    let mut pending: Option<BufferObject<framebuffer::Handle>> = None;
    let mut deadline = Instant::now();
    let mut last_frame = Instant::now() - frame_interval;
    let mut events = Vec::new();

    loop {
        // Sleep until input arrives, the queued page flip completes, or the next
        // frame is due. Waiting for vsync here rather than blocking on it keeps
        // input flowing to libinput, which warns when events wait over 20 ms.
        let timeout = if pending.is_some() {
            Duration::from_secs(1)
        } else {
            deadline.saturating_duration_since(Instant::now()).min(Duration::from_secs(1))
        };
        let mut pfds = vec![
            libc::pollfd { fd: input.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: gbm.as_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
        ];
        if let Some(fd) = app.wake_fd() {
            pfds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
        }
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout.as_millis().min(i32::MAX as u128) as i32) };

        if pfds.len() > 2 && pfds[2].revents & libc::POLLIN != 0 {
            // the app has news: clear the eventfd and draw a frame soon
            let mut n = 0u64;
            unsafe { libc::read(pfds[2].fd, &mut n as *mut u64 as *mut c_void, 8) };
            deadline = deadline.min(last_frame + frame_interval);
        }

        if pfds[1].revents & libc::POLLIN != 0 {
            for e in gbm.receive_events().context("drm events")? {
                if let control::Event::PageFlip(_) = e {
                    // the queued buffer is on screen; dropping the old one hands it back to GBM
                    current = pending.take();
                }
            }
        }

        if input.dispatch().is_ok() {
            let n = events.len();
            for ev in &mut input {
                if let input::Event::Device(d) = &ev {
                    let mut dev = ev.device();
                    match d {
                        DeviceEvent::Added(_) if dev.has_capability(DeviceCapability::Touch) && dev.config_calibration_has_matrix() => {
                            apply_touch_calibration(&mut dev, touch_cal);
                            touch_devices.push(dev);
                        }
                        DeviceEvent::Removed(_) => touch_devices.retain(|t| t.as_raw() != dev.as_raw()),
                        _ => {}
                    }
                }
                ist.handle(ev, &mut events);
            }
            if events.len() > n {
                deadline = deadline.min(last_frame + frame_interval);
            }
        }
        let now = Instant::now();
        if pending.is_some() || now < deadline {
            continue;
        }

        app.set_cursor(ist.mouse_visible.then_some(ist.pointer));
        last_frame = now;
        let delay = gfx.frame(&mut app, [w, h], ppp, std::mem::take(&mut events), start.elapsed().as_secs_f64());
        if let Some(req) = app.take_touch_request() {
            touch_cal = req;
            for dev in &mut touch_devices {
                apply_touch_calibration(dev, req);
            }
        }
        deadline = if delay == Duration::MAX { now + Duration::from_secs(3600) } else { now + delay.max(frame_interval) };

        egl.swap_buffers(display, egl_surface)?;
        let mut bo = unsafe { surface.lock_front_buffer() }.map_err(|_| anyhow!("gbm: no front buffer"))?;
        let fb = match bo.userdata() {
            Some(fb) => *fb,
            None => {
                let fb = gbm.add_framebuffer(&bo, 24, 32).context("add framebuffer")?;
                bo.set_userdata(fb);
                fb
            }
        };
        if current.is_none() {
            gbm.set_crtc(out.crtc, Some(fb), (0, 0), &[out.connector], Some(out.mode)).context("set mode")?;
            current = Some(bo);
        } else {
            // completes at the next vblank; the loop above picks up the event
            gbm.page_flip(out.crtc, fb, PageFlipFlags::EVENT, None).context("page flip")?;
            pending = Some(bo);
        }
    }
}
