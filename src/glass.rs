//! Liquid-glass rendering on top of egui.
//!
//! egui has no backdrop blur, so this module renders the wallpaper itself
//! into offscreen textures (sharp, lightly blurred, heavily blurred) and then
//! draws each glass element with a custom shader that samples those textures
//! with refraction, frost, a lit rim and a drop shadow. Everything is emitted
//! as egui paint callbacks, so it interleaves correctly with egui's own text
//! and shapes and works with any backend that uses `egui_glow` (KMS, headless,
//! eframe on the desktop).
//!
//! Glass refracts the wallpaper only, not other egui content underneath it.
//!
//! The shaders target GLSL ES 1.00 (VideoCore IV on the Pi 3 is GLES 2.0 only)
//! and are re-prefixed for desktop GL 1.20/1.40 when running on a Mac or PC.

use std::sync::{Arc, Mutex};

use egui::{Color32, Rect};
use egui_glow::ShaderVersion;
use glow::HasContext as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Theme {
    Aurora,
    Sunset,
    Ocean,
    Graphite,
    Photo,
}

impl Theme {
    pub const BUILTIN: [Theme; 4] = [Theme::Aurora, Theme::Sunset, Theme::Ocean, Theme::Graphite];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Aurora => "Aurora",
            Theme::Sunset => "Sunset",
            Theme::Ocean => "Ocean",
            Theme::Graphite => "Graphite",
            Theme::Photo => "Photo",
        }
    }

    pub fn from_name(s: &str) -> Option<Theme> {
        [Theme::Aurora, Theme::Sunset, Theme::Ocean, Theme::Graphite, Theme::Photo]
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(s))
    }

    /// base colour + four colour fields
    fn palette(self) -> [[f32; 3]; 5] {
        fn c(hex: u32) -> [f32; 3] {
            [
                ((hex >> 16) & 0xff) as f32 / 255.0,
                ((hex >> 8) & 0xff) as f32 / 255.0,
                (hex & 0xff) as f32 / 255.0,
            ]
        }
        match self {
            Theme::Aurora | Theme::Photo => {
                [c(0x0a0f2c), c(0x7c3aed), c(0x14b8a6), c(0xec4899), c(0x2563eb)]
            }
            Theme::Sunset => [c(0x1c0b1f), c(0xff6b4a), c(0xffb238), c(0xc026d3), c(0x4f46e5)],
            Theme::Ocean => [c(0x021526), c(0x0ea5e9), c(0x22d3ee), c(0x059669), c(0x1e40af)],
            Theme::Graphite => [c(0x0e0f12), c(0x52525b), c(0xa1a1aa), c(0x27272a), c(0x71717a)],
        }
    }
}

/// Appearance of one glass element. Lengths are in egui points.
#[derive(Clone, Copy, Debug)]
pub struct Glass {
    pub radius: f32,
    /// tint colour (alpha ignored) and how strongly it is mixed in (0..1)
    pub tint: Color32,
    pub tint_amount: f32,
    /// 0 = clear glass, 0.5 = light frost, 1 = heavy frost
    pub frost: f32,
    /// maximum displacement at the rim
    pub refraction: f32,
    /// width of the curved rim
    pub bevel: f32,
    /// brightness of rim light and inner glow
    pub highlight: f32,
    pub shadow: f32,
    pub shadow_blur: f32,
    pub shadow_offset: f32,
    pub opacity: f32,
}

impl Glass {
    /// Large container: frosted, soft shadow.
    pub fn panel(radius: f32) -> Self {
        Self {
            radius,
            tint: Color32::WHITE,
            tint_amount: 0.06,
            frost: 0.75,
            refraction: 14.0,
            bevel: 18.0,
            highlight: 1.0,
            shadow: 0.28,
            shadow_blur: 26.0,
            shadow_offset: 8.0,
            opacity: 1.0,
        }
    }

    /// Small interactive control: clearer, stronger lensing.
    pub fn control(radius: f32) -> Self {
        Self {
            radius,
            tint: Color32::WHITE,
            tint_amount: 0.10,
            frost: 0.45,
            refraction: 10.0,
            bevel: 12.0,
            highlight: 1.0,
            shadow: 0.20,
            shadow_blur: 12.0,
            shadow_offset: 4.0,
            opacity: 1.0,
        }
    }

    /// Clear magnifying lens (slider thumbs, selection pills).
    pub fn lens(radius: f32) -> Self {
        Self {
            radius,
            tint: Color32::WHITE,
            tint_amount: 0.14,
            frost: 0.0,
            refraction: 16.0,
            bevel: 14.0,
            highlight: 1.3,
            shadow: 0.22,
            shadow_blur: 12.0,
            shadow_offset: 4.0,
            opacity: 1.0,
        }
    }

    pub fn tinted(mut self, color: Color32, amount: f32) -> Self {
        self.tint = color;
        self.tint_amount = amount;
        self
    }

    pub fn frost(mut self, frost: f32) -> Self {
        self.frost = frost;
        self
    }

    pub fn highlight(mut self, h: f32) -> Self {
        self.highlight = h;
        self
    }

    pub fn opacity(mut self, o: f32) -> Self {
        self.opacity = o;
        self
    }

    pub fn no_shadow(mut self) -> Self {
        self.shadow = 0.0;
        self
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BackdropParams {
    pub time: f32,
    pub theme: Theme,
    pub animate: bool,
}

/// Global optics settings applied on top of every element.
#[derive(Clone, Copy, Debug)]
pub struct Optics {
    /// multiplies `Glass::frost` (0..1.5)
    pub frost: f32,
    /// multiplies `Glass::refraction` (0..2)
    pub refraction: f32,
}

/// Cheap to clone handle shared between the UI code and paint callbacks.
#[derive(Clone)]
pub struct GlassSystem {
    renderer: Arc<Mutex<Renderer>>,
    pub optics: Optics,
}

impl Default for GlassSystem {
    fn default() -> Self {
        Self {
            renderer: Arc::new(Mutex::new(Renderer::default())),
            optics: Optics { frost: 1.0, refraction: 1.0 },
        }
    }
}

impl GlassSystem {
    /// Wallpaper image for `Theme::Photo` (RGBA8, rows top-down).
    pub fn set_photo(&self, width: u32, height: u32, rgba: Vec<u8>) {
        let mut r = self.renderer.lock().unwrap();
        r.pending_photo = Some((width, height, rgba));
    }

    /// Paints the wallpaper over the whole screen. Must be the first shape of the frame.
    pub fn backdrop(&self, painter: &egui::Painter, params: BackdropParams) {
        let renderer = self.renderer.clone();
        let rect = painter.ctx().content_rect();
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, p| {
                let mut r = renderer.lock().unwrap();
                r.backdrop(p.gl(), &info, params);
            })),
        });
    }

    /// Paints one glass element covering `rect`.
    pub fn paint(&self, painter: &egui::Painter, rect: Rect, glass: Glass) {
        if glass.opacity <= 0.001 || rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        let mut g = glass;
        g.frost = (g.frost * self.optics.frost).clamp(0.0, 1.0);
        g.refraction *= self.optics.refraction;
        let margin = g.shadow_blur + g.shadow_offset.abs() + 2.0;
        let renderer = self.renderer.clone();
        painter.add(egui::PaintCallback {
            rect: rect.expand(margin),
            callback: Arc::new(egui_glow::CallbackFn::new(move |info, p| {
                let mut r = renderer.lock().unwrap();
                r.glass(p.gl(), &info, rect, g);
            })),
        });
    }
}

// ---------------------------------------------------------------------------

struct Target {
    fbo: glow::Framebuffer,
    tex: glow::Texture,
    w: u32,
    h: u32,
}

struct Targets {
    /// wallpaper at 1/2 resolution
    sharp: Target,
    /// 1/4 resolution, blurred (ping-pong pair)
    light: Target,
    light_tmp: Target,
    /// 1/8 resolution, blurred (ping-pong pair)
    heavy: Target,
    heavy_tmp: Target,
    screen: [u32; 2],
}

struct Programs {
    wallpaper: glow::Program,
    image: glow::Program,
    copy: glow::Program,
    down: glow::Program,
    blur: glow::Program,
    glass: glow::Program,
}

struct GlState {
    quad: glow::Buffer,
    vao: Option<glow::VertexArray>,
    progs: Programs,
    targets: Option<Targets>,
    photo: Option<(glow::Texture, u32, u32)>,
    /// what the offscreen textures currently contain
    rendered: Option<(Theme, [u32; 2])>,
}

#[derive(Default)]
struct Renderer {
    gl: Option<GlState>,
    failed: bool,
    pending_photo: Option<(u32, u32, Vec<u8>)>,
}

fn shader_prefixes(version: ShaderVersion) -> (&'static str, &'static str) {
    match version {
        ShaderVersion::Es100 | ShaderVersion::Es300 => (
            "#version 100\n#define ATTR attribute\n#define VARY varying\n",
            "#version 100\n#ifdef GL_FRAGMENT_PRECISION_HIGH\nprecision highp float;\n#else\nprecision mediump float;\n#endif\n#define VARY varying\n#define TEX texture2D\n#define FRAG_COLOR gl_FragColor\n",
        ),
        ShaderVersion::Gl140 => (
            "#version 140\n#define ATTR in\n#define VARY out\n",
            "#version 140\n#define VARY in\n#define TEX texture\nout vec4 frag_color_out;\n#define FRAG_COLOR frag_color_out\n",
        ),
        ShaderVersion::Gl120 => (
            "#version 120\n#define ATTR attribute\n#define VARY varying\n",
            "#version 120\n#define VARY varying\n#define TEX texture2D\n#define FRAG_COLOR gl_FragColor\n",
        ),
    }
}

unsafe fn compile(gl: &glow::Context, version: ShaderVersion, frag: &str) -> Result<glow::Program, String> {
    let (vp, fp) = shader_prefixes(version);
    unsafe {
        let program = gl.create_program()?;
        let sources = [
            (glow::VERTEX_SHADER, format!("{vp}{}", include_str!("shaders/quad.vert"))),
            (glow::FRAGMENT_SHADER, format!("{fp}{frag}")),
        ];
        let mut shaders = Vec::new();
        for (kind, src) in sources {
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, &src);
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(gl.get_shader_info_log(s));
            }
            gl.attach_shader(program, s);
            shaders.push(s);
        }
        gl.bind_attrib_location(program, 0, "a_pos");
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        Ok(program)
    }
}

unsafe fn make_target(gl: &glow::Context, w: u32, h: u32) -> Result<Target, String> {
    unsafe {
        let tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        for (p, v) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
        }
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );
        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(tex), 0);
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        if status != glow::FRAMEBUFFER_COMPLETE {
            return Err(format!("framebuffer incomplete: 0x{status:x}"));
        }
        Ok(Target { fbo, tex, w, h })
    }
}

unsafe fn delete_target(gl: &glow::Context, t: &Target) {
    unsafe {
        gl.delete_framebuffer(t.fbo);
        gl.delete_texture(t.tex);
    }
}

impl GlState {
    unsafe fn new(gl: &glow::Context) -> Result<Self, String> {
        let version = ShaderVersion::get(gl);
        log::info!("glass: shader version {version:?}");
        unsafe {
            let progs = Programs {
                wallpaper: compile(gl, version, include_str!("shaders/wallpaper.frag"))?,
                image: compile(gl, version, include_str!("shaders/image.frag"))?,
                copy: compile(gl, version, include_str!("shaders/copy.frag"))?,
                down: compile(gl, version, include_str!("shaders/down.frag"))?,
                blur: compile(gl, version, include_str!("shaders/blur.frag"))?,
                glass: compile(gl, version, include_str!("shaders/glass.frag"))?,
            };
            let quad = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
            let verts: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            let bytes: Vec<u8> = verts.iter().flat_map(|f| f.to_ne_bytes()).collect();
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            // GLES 2.0 (the Pi 3) has no vertex array objects; core profiles require one.
            let v = gl.version();
            let vao = if v.is_embedded && v.major < 3 { None } else { Some(gl.create_vertex_array()?) };
            Ok(Self { quad, vao, progs, targets: None, photo: None, rendered: None })
        }
    }

    unsafe fn ensure_targets(&mut self, gl: &glow::Context, screen: [u32; 2]) -> Result<(), String> {
        if self.targets.as_ref().is_some_and(|t| t.screen == screen) {
            return Ok(());
        }
        unsafe {
            if let Some(t) = self.targets.take() {
                for x in [&t.sharp, &t.light, &t.light_tmp, &t.heavy, &t.heavy_tmp] {
                    delete_target(gl, x);
                }
            }
            let d = |n: u32| -> (u32, u32) { (screen[0].div_ceil(n).max(1), screen[1].div_ceil(n).max(1)) };
            let (sw, sh) = d(2);
            let (lw, lh) = d(4);
            let (hw, hh) = d(8);
            self.targets = Some(Targets {
                sharp: make_target(gl, sw, sh)?,
                light: make_target(gl, lw, lh)?,
                light_tmp: make_target(gl, lw, lh)?,
                heavy: make_target(gl, hw, hh)?,
                heavy_tmp: make_target(gl, hw, hh)?,
                screen,
            });
        }
        self.rendered = None;
        Ok(())
    }

    unsafe fn bind_quad(&self, gl: &glow::Context) {
        unsafe {
            if let Some(vao) = self.vao {
                gl.bind_vertex_array(Some(vao));
            }
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.quad));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        }
    }

    unsafe fn unbind_quad(&self, gl: &glow::Context) {
        unsafe {
            gl.disable_vertex_attrib_array(0);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            if self.vao.is_some() {
                gl.bind_vertex_array(None);
            }
        }
    }

    unsafe fn draw(gl: &glow::Context) {
        unsafe { gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4) };
    }

    /// Render `src` through `prog` into `dst`.
    unsafe fn pass(&self, gl: &glow::Context, prog: glow::Program, src: Option<glow::Texture>, dst: &Target) {
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dst.fbo));
            gl.viewport(0, 0, dst.w as i32, dst.h as i32);
            gl.use_program(Some(prog));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, src);
            if let Some(l) = gl.get_uniform_location(prog, "u_tex") {
                gl.uniform_1_i32(Some(&l), 0);
            }
            Self::draw(gl);
        }
    }

    unsafe fn blur(&self, gl: &glow::Context, a: &Target, tmp: &Target) {
        let p = self.progs.blur;
        unsafe {
            gl.use_program(Some(p));
            let dir = gl.get_uniform_location(p, "u_dir");
            gl.uniform_2_f32(dir.as_ref(), 1.0 / a.w as f32, 0.0);
            self.pass(gl, p, Some(a.tex), tmp);
            gl.use_program(Some(p));
            gl.uniform_2_f32(dir.as_ref(), 0.0, 1.0 / a.h as f32);
            self.pass(gl, p, Some(tmp.tex), a);
        }
    }

    unsafe fn down(&self, gl: &glow::Context, src: &Target, dst: &Target) {
        let p = self.progs.down;
        unsafe {
            gl.use_program(Some(p));
            let h = gl.get_uniform_location(p, "u_half");
            gl.uniform_2_f32(h.as_ref(), 0.5 / src.w as f32, 0.5 / src.h as f32);
            self.pass(gl, p, Some(src.tex), dst);
        }
    }

    unsafe fn render_wallpaper(&mut self, gl: &glow::Context, params: BackdropParams) {
        let t = self.targets.as_ref().unwrap();
        let aspect = t.screen[0] as f32 / t.screen[1].max(1) as f32;
        unsafe {
            match (params.theme, self.photo) {
                (Theme::Photo, Some((tex, iw, ih))) => {
                    let p = self.progs.image;
                    gl.use_program(Some(p));
                    // cover: scale the smaller relative dimension to fit
                    let img_aspect = iw as f32 / ih.max(1) as f32;
                    let (sx, sy) = if img_aspect > aspect { (aspect / img_aspect, 1.0) } else { (1.0, img_aspect / aspect) };
                    gl.uniform_2_f32(gl.get_uniform_location(p, "u_scale").as_ref(), sx, sy);
                    self.pass(gl, p, Some(tex), &t.sharp);
                }
                _ => {
                    let p = self.progs.wallpaper;
                    gl.use_program(Some(p));
                    let pal = params.theme.palette();
                    let u = |n: &str| gl.get_uniform_location(p, n);
                    gl.uniform_1_f32(u("u_time").as_ref(), params.time);
                    gl.uniform_1_f32(u("u_aspect").as_ref(), aspect);
                    for (name, c) in ["u_base", "u_c0", "u_c1", "u_c2", "u_c3"].iter().zip(pal) {
                        gl.uniform_3_f32(u(name).as_ref(), c[0], c[1], c[2]);
                    }
                    self.pass(gl, p, None, &t.sharp);
                }
            }
            self.down(gl, &t.sharp, &t.light);
            self.blur(gl, &t.light, &t.light_tmp);
            self.down(gl, &t.light, &t.heavy);
            self.blur(gl, &t.heavy, &t.heavy_tmp);
            self.blur(gl, &t.heavy, &t.heavy_tmp);
        }
        self.rendered = Some((params.theme, t.screen));
    }

    unsafe fn upload_photo(&mut self, gl: &glow::Context, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
        unsafe {
            if let Some((tex, _, _)) = self.photo.take() {
                gl.delete_texture(tex);
            }
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            for (p, v) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, p, v as i32);
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(rgba)),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            self.photo = Some((tex, w, h));
        }
        self.rendered = None;
        Ok(())
    }
}

unsafe fn current_framebuffer(gl: &glow::Context) -> Option<glow::Framebuffer> {
    let id = unsafe { gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) } as u32;
    std::num::NonZeroU32::new(id).map(glow::NativeFramebuffer)
}

impl Renderer {
    fn ensure_gl(&mut self, gl: &glow::Context) -> bool {
        if self.failed {
            return false;
        }
        if self.gl.is_none() {
            match unsafe { GlState::new(gl) } {
                Ok(s) => self.gl = Some(s),
                Err(e) => {
                    log::error!("glass: GL setup failed, falling back to flat colours: {e}");
                    self.failed = true;
                    return false;
                }
            }
        }
        true
    }

    fn backdrop(&mut self, gl: &glow::Context, info: &egui::PaintCallbackInfo, params: BackdropParams) {
        if !self.ensure_gl(gl) {
            unsafe {
                gl.clear_color(0.04, 0.06, 0.17, 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
            return;
        }
        let screen = info.screen_size_px;
        let photo = self.pending_photo.take();
        let s = self.gl.as_mut().unwrap();
        unsafe {
            let target_fb = current_framebuffer(gl);
            if let Some((w, h, data)) = photo
                && let Err(e) = s.upload_photo(gl, w, h, &data)
            {
                log::warn!("glass: could not upload wallpaper photo: {e}");
            }
            if let Err(e) = s.ensure_targets(gl, screen) {
                log::error!("glass: {e}");
                self.failed = true;
                gl.bind_framebuffer(glow::FRAMEBUFFER, target_fb);
                return;
            }
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            s.bind_quad(gl);

            let stale = s.rendered != Some((params.theme, screen));
            let animated = params.animate && !(params.theme == Theme::Photo && s.photo.is_some());
            if stale || animated {
                s.render_wallpaper(gl, params);
            }

            // present the sharp wallpaper to the screen
            gl.bind_framebuffer(glow::FRAMEBUFFER, target_fb);
            gl.viewport(0, 0, screen[0] as i32, screen[1] as i32);
            let p = s.progs.copy;
            gl.use_program(Some(p));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(s.targets.as_ref().unwrap().sharp.tex));
            gl.uniform_1_i32(gl.get_uniform_location(p, "u_tex").as_ref(), 0);
            GlState::draw(gl);

            s.unbind_quad(gl);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.use_program(None);
            gl.enable(glow::SCISSOR_TEST);
        }
    }

    fn glass(&mut self, gl: &glow::Context, info: &egui::PaintCallbackInfo, rect: Rect, g: Glass) {
        let Some(s) = self.gl.as_ref() else { return };
        let Some(t) = s.targets.as_ref() else { return };
        let ppp = info.pixels_per_point;
        let [sw, sh] = info.screen_size_px;
        let c = rect.center();
        let p = s.progs.glass;
        unsafe {
            gl.use_program(Some(p));
            let u = |n: &str| gl.get_uniform_location(p, n);
            for (unit, name, tex) in [(0u32, "u_sharp", t.sharp.tex), (1, "u_light", t.light.tex), (2, "u_heavy", t.heavy.tex)] {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.uniform_1_i32(u(name).as_ref(), unit as i32);
            }
            gl.uniform_2_f32(u("u_screen").as_ref(), sw as f32, sh as f32);
            gl.uniform_4_f32(
                u("u_rect").as_ref(),
                c.x * ppp,
                sh as f32 - c.y * ppp,
                rect.width() * 0.5 * ppp,
                rect.height() * 0.5 * ppp,
            );
            gl.uniform_1_f32(u("u_radius").as_ref(), g.radius * ppp);
            let [r, gg, b, _] = g.tint.to_normalized_gamma_f32();
            gl.uniform_4_f32(u("u_tint").as_ref(), r, gg, b, g.tint_amount);
            gl.uniform_4_f32(u("u_optics").as_ref(), g.frost, g.refraction * ppp, g.bevel * ppp, g.highlight);
            gl.uniform_4_f32(u("u_shadow").as_ref(), g.shadow, g.shadow_blur * ppp, g.shadow_offset * ppp, g.opacity);

            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            s.bind_quad(gl);
            GlState::draw(gl);
            s.unbind_quad(gl);

            for unit in [2u32, 1, 0] {
                gl.active_texture(glow::TEXTURE0 + unit);
                gl.bind_texture(glow::TEXTURE_2D, None);
            }
            gl.use_program(None);
        }
    }
}
