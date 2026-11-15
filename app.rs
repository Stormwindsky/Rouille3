//! Window, rendering, input and on-screen controls.
use crate::assets::{self, Gfx};
use crate::audio::Audio;
use crate::intro::Intro;
use crate::project;
use crate::runtime::{corners, PenItem, Visual, World};
use crate::settings::Settings;
use eframe::egui::{
    self, Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, TextureHandle, Vec2,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

enum Phase {
    Intro(Intro),
    Run,
}

#[derive(Clone, Copy)]
struct Xf {
    origin: Pos2,
    sx: f32,
    sy: f32,
}

impl Xf {
    fn pt(&self, x: f64, y: f64) -> Pos2 {
        Pos2::new(self.origin.x + (240.0 + x as f32) * self.sx, self.origin.y + (180.0 - y as f32) * self.sy)
    }
}

pub struct App {
    settings: Settings,
    audio: Rc<RefCell<Audio>>,
    world: Option<World>,
    gfx: Gfx,
    phase: Phase,
    status: String,
    acc: f64,
    last: Instant,
    paused: bool,
    muted: bool,
    current_path: Option<PathBuf>,
    icons: HashMap<String, TextureHandle>,
    bubble_family: Option<FontFamily>,
    monitor_family: Option<FontFamily>,
    started: bool,
}

fn find_sb3(settings: &Settings) -> Option<PathBuf> {
    if let Some(arg) = std::env::args().nth(1) {
        return Some(PathBuf::from(arg));
    }
    if let Some(p) = &settings.game.sb3 {
        return Some(PathBuf::from(p));
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(&settings.game.project_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map_or(false, |x| x.eq_ignore_ascii_case("sb3")))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Scratch name of a key: "a".."z", "0".."9", "space", "up arrow", "enter", ...
pub fn key_name(k: egui::Key) -> String {
    match k {
        egui::Key::ArrowUp => "up arrow".into(),
        egui::Key::ArrowDown => "down arrow".into(),
        egui::Key::ArrowLeft => "left arrow".into(),
        egui::Key::ArrowRight => "right arrow".into(),
        egui::Key::Space => "space".into(),
        egui::Key::Enter => "enter".into(),
        other => other.name().to_lowercase(),
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        let audio = Rc::new(RefCell::new(Audio::new()));
        let mut app = App {
            settings: settings.clone(),
            audio: audio.clone(),
            world: None,
            gfx: Gfx::default(),
            phase: Phase::Run,
            status: "Drop a .sb3 file on this window".into(),
            acc: 0.0,
            last: Instant::now(),
            paused: false,
            muted: false,
            current_path: None,
            icons: HashMap::new(),
            bubble_family: None,
            monitor_family: None,
            started: false,
        };
        app.setup_fonts(&cc.egui_ctx);
        if let Some(path) = find_sb3(&settings) {
            app.load_project(&cc.egui_ctx, &path);
        }
        if settings.intro.enabled {
            if let Some(intro) = Intro::start(&settings.intro.file, audio) {
                app.phase = Phase::Intro(intro);
            }
        }
        if matches!(app.phase, Phase::Run) {
            app.begin_run();
        }
        app
    }

    fn setup_fonts(&mut self, ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        let mut changed = false;
        for f in &self.settings.fonts {
            match std::fs::read(&f.file) {
                Ok(bytes) => {
                    fonts.font_data.insert(f.name.clone(), egui::FontData::from_owned(bytes));
                    fonts.families.insert(FontFamily::Name(f.name.as_str().into()), vec![f.name.clone()]);
                    changed = true;
                }
                Err(e) => eprintln!("[fonts] {}: {e}", f.file),
            }
        }
        if let Some(path) = &self.settings.speech_bubbles.font {
            match std::fs::read(path) {
                Ok(bytes) => {
                    fonts.font_data.insert("bubble".into(), egui::FontData::from_owned(bytes));
                    fonts.families.insert(FontFamily::Name("bubble".into()), vec!["bubble".into()]);
                    self.bubble_family = Some(FontFamily::Name("bubble".into()));
                    changed = true;
                }
                Err(e) => eprintln!("[fonts] speech bubble font {path}: {e}"),
            }
        }
        if self.bubble_family.is_none() && self.settings.speech_bubbles.use_builtin_font {
            self.bubble_family = Some(FontFamily::Proportional);
        }
        if let Some(path) = &self.settings.monitors.font {
            match std::fs::read(path) {
                Ok(bytes) => {
                    fonts.font_data.insert("monitor".into(), egui::FontData::from_owned(bytes));
                    fonts.families.insert(FontFamily::Name("monitor".into()), vec!["monitor".into()]);
                    self.monitor_family = Some(FontFamily::Name("monitor".into()));
                    changed = true;
                }
                Err(e) => eprintln!("[fonts] monitor font {path}: {e}"),
            }
        }
        if self.monitor_family.is_none() {
            self.monitor_family = self.bubble_family.clone();
        }
        if self.monitor_family.is_none() && self.settings.monitors.use_builtin_font {
            self.monitor_family = Some(FontFamily::Proportional);
        }
        if changed {
            ctx.set_fonts(fonts);
        }
    }

    fn load_project(&mut self, ctx: &egui::Context, path: &Path) {
        self.status = format!("Loading {}", path.display());
        match project::load_sb3(path) {
            Ok(mut lp) => {
                self.fetch_missing(&mut lp);
                let (gfx, meta) = assets::build(ctx, &lp.project, &lp.assets);
                self.gfx = gfx;
                self.audio.borrow_mut().stop_all();
                self.world = Some(World::new(lp.project, lp.assets, meta, self.settings.clone(), self.audio.clone()));
                self.started = false;
                self.paused = false;
                self.current_path = Some(path.to_path_buf());
                self.status = format!("Loaded {}", path.display());
            }
            Err(e) => {
                self.status = format!("Cannot load {}: {e}", path.display());
                eprintln!("{}", self.status);
            }
        }
    }

    /// Fills assets missing from the .sb3 using the local cache or `downloads.assets` URLs.
    fn fetch_missing(&self, lp: &mut project::LoadedProject) {
        let missing = lp.missing_assets();
        if missing.is_empty() {
            return;
        }
        let cache = Path::new(&self.settings.game.project_dir).join("other").join("downloads");
        for name in missing {
            let cached = cache.join(&name);
            if let Ok(bytes) = std::fs::read(&cached) {
                lp.assets.insert(name, bytes);
                continue;
            }
            if !self.settings.downloads.enabled {
                eprintln!("[assets] missing: {name} (downloads are disabled)");
                continue;
            }
            let Some(url) = self.settings.downloads.assets.get(&name) else {
                eprintln!("[assets] missing: {name} (no URL in settings.json5)");
                continue;
            };
            eprintln!("[assets] downloading {name}");
            let result = ureq::get(url).call().map_err(|e| e.to_string()).and_then(|r| {
                use std::io::Read;
                let mut buf = Vec::new();
                r.into_reader().take(64 * 1024 * 1024).read_to_end(&mut buf).map_err(|e| e.to_string())?;
                Ok(buf)
            });
            match result {
                Ok(bytes) => {
                    let _ = std::fs::create_dir_all(&cache);
                    let _ = std::fs::write(&cached, &bytes);
                    lp.assets.insert(name, bytes);
                }
                Err(e) => eprintln!("[assets] download failed for {name}: {e}"),
            }
        }
    }

    fn begin_run(&mut self) {
        self.last = Instant::now();
        if !self.started && self.settings.controls.auto_start {
            if let Some(w) = &mut self.world {
                w.green_flag();
                self.started = true;
            }
        }
    }

    fn icon(&mut self, ctx: &egui::Context, path: &str) -> Option<TextureHandle> {
        if let Some(t) = self.icons.get(path) {
            return Some(t.clone());
        }
        let bytes = std::fs::read(path).ok()?;
        let fmt = if path.to_lowercase().ends_with(".svg") { "svg" } else { "png" };
        let (img, _, _) = assets::decode_image(&bytes, fmt).ok()?;
        let t = ctx.load_texture(
            format!("icon:{path}"), img,
            if fmt == "svg" { egui::TextureOptions::LINEAR } else { egui::TextureOptions::NEAREST }, // crisp pixel art
        );
        self.icons.insert(path.to_string(), t.clone());
        Some(t)
    }

    fn stage_rect(&self, avail: Rect) -> Rect {
        if self.settings.window.scaling == "stretch" {
            return avail;
        }
        let scale = (avail.width() / 480.0).min(avail.height() / 360.0);
        Rect::from_center_size(avail.center(), Vec2::new(480.0 * scale, 360.0 * scale))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Btn { Flag, Stop, Volume }

/// One on-screen control, resolved for the current state.
#[derive(Clone)]
struct Ctl {
    kind: Btn,
    visible: bool,
    position: String,
    size: f32,
    margin: f32,
    icon: Option<String>,
    /// Flag: "shows the pause icon" / Volume: "is muted".
    state: bool,
}

fn align_for(pos: &str) -> Align2 {
    match pos {
        "top-center" => Align2::CENTER_TOP,
        "top-right" => Align2::RIGHT_TOP,
        "bottom-left" => Align2::LEFT_BOTTOM,
        "bottom-center" => Align2::CENTER_BOTTOM,
        "bottom-right" => Align2::RIGHT_BOTTOM,
        _ => Align2::LEFT_TOP,
    }
}

fn paint_button(painter: &Painter, rect: Rect, kind: Btn, hovered: bool, state: bool, icon: Option<&TextureHandle>) {
    painter.rect_filled(rect, 6.0, Color32::from_black_alpha(if hovered { 90 } else { 35 }));
    if let Some(t) = icon {
        // keep the icon's own aspect ratio inside the square button
        let sz = t.size_vec2();
        let inner = rect.shrink(rect.width() * 0.12);
        let k = (inner.width() / sz.x).min(inner.height() / sz.y);
        painter.image(
            t.id(), Rect::from_center_size(rect.center(), sz * k),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        return;
    }
    // Fallback drawings when an icon file is missing
    let c = rect.center();
    let u = rect.width() / 32.0;
    match kind {
        Btn::Flag => {
            if state {
                for dx in [-6.0f32, 2.0] {
                    painter.rect_filled(Rect::from_min_size(c + Vec2::new(dx, -9.0) * u, Vec2::new(5.0, 18.0) * u), 1.0, Color32::from_rgb(230, 200, 60));
                }
            } else {
                painter.add(Shape::convex_polygon(
                    vec![c + Vec2::new(-6.0, -10.0) * u, c + Vec2::new(10.0, 0.0) * u, c + Vec2::new(-6.0, 10.0) * u],
                    Color32::from_rgb(70, 200, 60), Stroke::NONE));
            }
        }
        Btn::Stop => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(16.0 * u)), 2.0, Color32::from_rgb(255, 80, 80));
        }
        Btn::Volume => {
            let col = Color32::from_rgb(170, 140, 255);
            painter.rect_filled(Rect::from_min_max(c + Vec2::new(-9.0, -4.0) * u, c + Vec2::new(-3.0, 4.0) * u), 0.0, col);
            painter.add(Shape::convex_polygon(
                vec![c + Vec2::new(-3.0, -4.0) * u, c + Vec2::new(4.0, -9.0) * u, c + Vec2::new(4.0, 9.0) * u, c + Vec2::new(-3.0, 4.0) * u],
                col, Stroke::NONE));
            if state {
                painter.line_segment([c + Vec2::new(-9.0, -9.0) * u, c + Vec2::new(9.0, 9.0) * u], Stroke::new(3.0 * u, Color32::from_rgb(255, 80, 80)));
            }
        }
    }
}

fn draw_visual(
    painter: &Painter,
    ctx: &egui::Context,
    gfx: &mut Gfx,
    world: &World,
    v: &Visual,
    xf: &Xf,
) {
    let Some(md5) = world.costume_md5(v.target, v.costume) else { return };
    let Some(tex) = gfx.texture_for(ctx, &md5, v.effects[0], v.effects[5]) else { return };
    let m = world.costume_meta(v.target, v.costume);
    let c = corners(&m, v);
    let alpha = ((1.0 - v.effects[6] / 100.0).clamp(0.0, 1.0) * 255.0) as u8;
    let color = Color32::from_white_alpha(alpha);
    let uvs = [Pos2::new(0.0, 0.0), Pos2::new(1.0, 0.0), Pos2::new(1.0, 1.0), Pos2::new(0.0, 1.0)];
    let mut mesh = egui::Mesh::with_texture(tex);
    for i in 0..4 {
        mesh.vertices.push(egui::epaint::Vertex { pos: xf.pt(c[i].0, c[i].1), uv: uvs[i], color });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(Shape::mesh(mesh));
}

fn hash01(i: u32, salt: u32) -> f32 {
    let mut x = i.wrapping_mul(0x9E3779B1) ^ salt.wrapping_mul(0x85EBCA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B3C6D);
    x ^= x >> 12;
    (x & 0xFFFF) as f32 / 65535.0
}

/// Rouille3 screen effects driven by the custom blocks (CPU overlays for now).
fn draw_screen_effects(painter: &Painter, rect: Rect, world: &World) {
    if !world.shader_on {
        return;
    }
    let t = world.time as f32;
    let k = rect.width() / 480.0;
    let has = |n: &str| world.effects_on.contains(n);
    if has("dark") {
        painter.rect_filled(rect, 0.0, Color32::from_black_alpha(165));
    }
    if has("night vision") {
        painter.rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 255, 70, 70));
        let mut y = rect.top();
        while y < rect.bottom() {
            painter.line_segment([Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)], Stroke::new(1.0f32, Color32::from_black_alpha(35)));
            y += 4.0 * k.max(0.5);
        }
    }
    for (name, color, tint) in [
        ("rain", Color32::from_rgba_unmultiplied(190, 210, 255, 170), None),
        ("acid rain", Color32::from_rgba_unmultiplied(150, 255, 90, 190), Some(Color32::from_rgba_unmultiplied(80, 200, 40, 28))),
    ] {
        if has(name) {
            if let Some(tc) = tint { painter.rect_filled(rect, 0.0, tc); }
            let salt = if name == "rain" { 1 } else { 7 };
            for i in 0..140u32 {
                let x = rect.left() + hash01(i, salt) * rect.width();
                let speed = 260.0 + hash01(i, salt + 2) * 220.0;
                let y = rect.top() + ((hash01(i, salt + 3) * rect.height()) + t * speed * k) % rect.height();
                let len = 14.0 * k;
                painter.line_segment([Pos2::new(x, y), Pos2::new(x - len * 0.25, y + len)], Stroke::new(1.2 * k.max(0.8), color));
            }
        }
    }
}

/// Variable / list monitors, drawn like Scratch's (orange value boxes).
fn draw_monitors(
    ui: &egui::Ui,
    painter: &Painter,
    world: &World,
    xf: &Xf,
    family: &Option<FontFamily>,
    slider_set: &mut Option<(usize, f64)>,
) {
    let Some(fam) = family else { return };
    let k = xf.sx;
    let font = FontId::new(11.0 * k, fam.clone());
    let orange = Color32::from_rgb(255, 140, 26);
    let panel = Color32::from_rgb(230, 240, 255);
    let border = Stroke::new(1.0f32, Color32::from_gray(190));
    let text = Color32::from_rgb(87, 94, 117);
    for v in world.monitor_views() {
        let pos = Pos2::new(xf.origin.x + v.x as f32 * xf.sx, xf.origin.y + v.y as f32 * xf.sy);
        if v.is_list {
            let (w, h) = (if v.w > 0.0 { v.w as f32 } else { 100.0 } * k, if v.h > 0.0 { v.h as f32 } else { 200.0 } * k);
            let rect = Rect::from_min_size(pos, Vec2::new(w, h));
            painter.rect(rect, 4.0 * k, panel, border);
            let head = painter.layout_no_wrap(v.label.clone(), font.clone(), text);
            painter.galley(Pos2::new(rect.center().x - head.size().x / 2.0, rect.top() + 3.0 * k), head, text);
            let foot = painter.layout_no_wrap(format!("length {}", v.items.len()), font.clone(), text);
            painter.galley(Pos2::new(rect.center().x - foot.size().x / 2.0, rect.bottom() - foot.size().y - 3.0 * k), foot, text);
            let area = Rect::from_min_max(
                Pos2::new(rect.left() + 3.0 * k, rect.top() + 20.0 * k),
                Pos2::new(rect.right() - 3.0 * k, rect.bottom() - 20.0 * k));
            let clip = painter.with_clip_rect(area.intersect(painter.clip_rect()));
            let row = 20.0 * k;
            for (i, item) in v.items.iter().enumerate() {
                let y = area.top() + i as f32 * row;
                if y > area.bottom() { break }
                let idx = clip.layout_no_wrap((i + 1).to_string(), font.clone(), text);
                clip.galley(Pos2::new(area.left(), y + 3.0 * k), idx, text);
                let boxr = Rect::from_min_max(Pos2::new(area.left() + 20.0 * k, y), Pos2::new(area.right(), y + row - 2.0 * k));
                clip.rect_filled(boxr, 3.0 * k, orange);
                let g = clip.layout_no_wrap(item.clone(), font.clone(), Color32::WHITE);
                clip.galley(boxr.left_top() + Vec2::new(4.0 * k, 3.0 * k), g, Color32::WHITE);
            }
            continue;
        }
        if v.mode == "large" {
            let g = painter.layout_no_wrap(v.value.clone(), FontId::new(16.0 * k, fam.clone()), Color32::WHITE);
            let size = Vec2::new((g.size().x + 14.0 * k).max(44.0 * k), g.size().y + 8.0 * k);
            let r = Rect::from_min_size(pos, size);
            painter.rect_filled(r, 4.0 * k, orange);
            painter.galley(Pos2::new(r.center().x - g.size().x / 2.0, r.top() + 4.0 * k), g, Color32::WHITE);
            continue;
        }
        let label = painter.layout_no_wrap(v.label.clone(), font.clone(), text);
        let val = painter.layout_no_wrap(v.value.clone(), font.clone(), Color32::WHITE);
        let vbox_w = (val.size().x + 10.0 * k).max(40.0 * k);
        let row_h = label.size().y.max(val.size().y) + 6.0 * k;
        let mut width = label.size().x + vbox_w + 14.0 * k;
        let slider = v.mode == "slider";
        if slider { width = width.max(100.0 * k); }
        let height = row_h + 4.0 * k + if slider { 16.0 * k } else { 0.0 };
        let rect = Rect::from_min_size(pos, Vec2::new(width, height));
        painter.rect(rect, 4.0 * k, panel, border);
        painter.galley(Pos2::new(rect.left() + 5.0 * k, rect.top() + 5.0 * k), label.clone(), text);
        let vb = Rect::from_min_size(
            Pos2::new(rect.right() - vbox_w - 4.0 * k, rect.top() + 2.0 * k), Vec2::new(vbox_w, row_h));
        painter.rect_filled(vb, 3.0 * k, orange);
        painter.galley(Pos2::new(vb.center().x - val.size().x / 2.0, vb.top() + 3.0 * k), val, Color32::WHITE);
        if slider {
            let track = Rect::from_min_max(
                Pos2::new(rect.left() + 8.0 * k, rect.bottom() - 11.0 * k),
                Pos2::new(rect.right() - 8.0 * k, rect.bottom() - 7.0 * k));
            painter.rect_filled(track, 2.0 * k, Color32::from_gray(200));
            let span = (v.slider_max - v.slider_min).max(f64::EPSILON);
            let frac = ((v.value_num - v.slider_min) / span).clamp(0.0, 1.0) as f32;
            let hx = track.left() + frac * track.width();
            painter.circle_filled(Pos2::new(hx, track.center().y), 5.0 * k, orange);
            let hit = track.expand2(Vec2::new(6.0 * k, 8.0 * k));
            let resp = ui.interact(hit, egui::Id::new(("rouille3_slider", v.index)), egui::Sense::click_and_drag());
            if resp.dragged() || resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let f = ((p.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64;
                    let mut val = v.slider_min + f * span;
                    if v.discrete { val = val.round(); }
                    *slider_set = Some((v.index, val));
                }
            }
        }
    }
}

fn draw_bubble(
    painter: &Painter,
    stage: Rect,
    world: &World,
    s: usize,
    xf: &Xf,
    family: &Option<FontFamily>,
) {
    let sp = &world.sprites[s];
    let Some(b) = &sp.bubble else { return };
    let (_, _, maxx, maxy) = world.bounds(s);
    let anchor = xf.pt(maxx, maxy);
    let k = xf.sx;
    let pad = 8.0 * k;
    let galley = family.as_ref().map(|f| {
        painter.layout(b.text.clone(), FontId::new(14.0 * k, f.clone()), Color32::from_gray(60), 170.0 * k)
    });
    let size = galley.as_ref().map_or(Vec2::new(60.0 * k, 24.0 * k), |g| g.size());
    let bsize = size + Vec2::splat(pad * 2.0);
    let mut min = Pos2::new(anchor.x + 4.0 * k, anchor.y - bsize.y - 10.0 * k);
    let mut left = false;
    if min.x + bsize.x > stage.right() {
        min.x = anchor.x - (maxx - world.bounds(s).0) as f32 * k - bsize.x - 4.0 * k;
        left = true;
    }
    min.x = min.x.clamp(stage.left(), (stage.right() - bsize.x).max(stage.left()));
    min.y = min.y.clamp(stage.top(), (stage.bottom() - bsize.y).max(stage.top()));
    let rect = Rect::from_min_size(min, bsize);
    let stroke = Stroke::new(2.0 * k.max(0.7), Color32::from_rgb(160, 160, 170));
    painter.rect(rect, 12.0 * k, Color32::WHITE, stroke);
    let tx = if left { rect.right() - 20.0 * k } else { rect.left() + 20.0 * k };
    let tail = vec![
        Pos2::new(tx - 6.0 * k, rect.bottom() - 1.0),
        Pos2::new(tx + 6.0 * k, rect.bottom() - 1.0),
        Pos2::new(if left { tx + 14.0 * k } else { tx - 14.0 * k }, rect.bottom() + 12.0 * k),
    ];
    painter.add(Shape::convex_polygon(tail, Color32::WHITE, Stroke::NONE));
    if let Some(g) = galley {
        painter.galley(rect.min + Vec2::splat(pad), g, Color32::from_gray(60));
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- drag & drop a .sb3
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(p) = dropped.iter().find_map(|f| f.path.clone()) {
            self.load_project(ctx, &p);
            if matches!(self.phase, Phase::Run) {
                self.begin_run();
            }
        }

        let now = Instant::now();
        let dt = (now - self.last).as_secs_f64().min(0.25);
        self.last = now;
        let screen = ctx.screen_rect();

        // ---- intro
        if let Phase::Intro(intro) = &mut self.phase {
            let finished = intro.update(ctx, self.settings.intro.skippable);
            egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::BLACK)).show(ctx, |ui| {
                if let Some(t) = &intro.tex {
                    let scale = (screen.width() / intro.w as f32).min(screen.height() / intro.h as f32);
                    let r = Rect::from_center_size(screen.center(), Vec2::new(intro.w as f32 * scale, intro.h as f32 * scale));
                    ui.painter().image(t.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                }
            });
            ctx.request_repaint();
            if finished {
                self.phase = Phase::Run;
                self.begin_run();
            }
            return;
        }

        let stage = self.stage_rect(screen);
        let xf = Xf { origin: stage.min, sx: stage.width() / 480.0, sy: stage.height() / 360.0 };

        // ---- input -> world
        let mut over_ui = false;
        let mut clicked: Option<Btn> = None;
        let c = self.settings.controls.clone();
        let flag_pause = self.started && !self.paused;
        let ctls = vec![
            Ctl {
                kind: Btn::Flag, visible: c.green_flag.visible, position: c.green_flag.position.clone(),
                size: c.green_flag.size, margin: c.green_flag.margin, state: flag_pause,
                icon: if flag_pause { c.green_flag.icon_pause.clone() } else { c.green_flag.icon_play.clone() },
            },
            Ctl {
                kind: Btn::Stop, visible: c.stop.visible, position: c.stop.position.clone(),
                size: c.stop.size, margin: c.stop.margin, state: false, icon: c.stop.icon.clone(),
            },
            Ctl {
                kind: Btn::Volume, visible: c.volume.visible, position: c.volume.position.clone(),
                size: c.volume.size, margin: c.volume.margin, state: self.muted,
                icon: if self.muted { c.volume.icon_off.clone() } else { c.volume.icon_on.clone() },
            },
        ];

        // controls overlay
        let positions = ["top-left", "top-center", "top-right", "bottom-left", "bottom-center", "bottom-right"];
        for pos in positions {
            let group: Vec<&Ctl> = ctls.iter().filter(|b| b.visible && b.position == pos).collect();
            if group.is_empty() || self.world.is_none() {
                continue;
            }
            let align = align_for(pos);
            let margin = group[0].margin;
            let off = Vec2::new(
                if pos.ends_with("right") { -margin } else if pos.ends_with("left") { margin } else { 0.0 },
                if pos.starts_with("bottom") { -margin } else { margin },
            );
            let mut icons: Vec<Option<TextureHandle>> = Vec::new();
            for b in &group {
                icons.push(b.icon.as_ref().and_then(|p| self.icon(ctx, p)));
            }
            egui::Area::new(egui::Id::new(("rouille3_controls", pos)))
                .anchor(align, off)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing = Vec2::splat(4.0);
                        for (i, b) in group.iter().enumerate() {
                            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(b.size), egui::Sense::click());
                            paint_button(ui.painter(), rect, b.kind, resp.hovered(), b.state, icons[i].as_ref());
                            if resp.hovered() || resp.is_pointer_button_down_on() { over_ui = true; }
                            if resp.clicked() { clicked = Some(b.kind); }
                        }
                    });
                });
        }

        // button actions
        match clicked {
            Some(Btn::Flag) => {
                if !self.started {
                    if let Some(w) = &mut self.world { w.green_flag(); }
                    self.started = true;
                    self.paused = false;
                } else {
                    self.paused = !self.paused; // after the first press the flag is a pause / resume button
                }
                self.audio.borrow_mut().set_paused(self.paused);
            }
            Some(Btn::Stop) => {
                self.paused = false;
                self.audio.borrow_mut().set_paused(false);
                if c.stop.mode == "stop" {
                    if let Some(w) = &mut self.world { w.stop_all(); }
                    self.started = false;
                } else if let Some(p) = self.current_path.clone() {
                    // restart from the very beginning: fresh state, then green flag
                    self.load_project(ctx, &p);
                    if let Some(w) = &mut self.world { w.green_flag(); }
                    self.started = true;
                }
            }
            Some(Btn::Volume) => {
                self.muted = !self.muted;
                self.audio.borrow_mut().set_muted(self.muted);
            }
            None => {}
        }

        if let Some(world) = &mut self.world {
            // mouse
            let (hover, pressed, released, down) = ctx.input(|i| (
                i.pointer.hover_pos(), i.pointer.primary_pressed(), i.pointer.primary_released(), i.pointer.primary_down()));
            if let Some(p) = hover {
                let x = ((p.x - xf.origin.x) / xf.sx) as f64 - 240.0;
                let y = 180.0 - ((p.y - xf.origin.y) / xf.sy) as f64;
                world.mouse = (x.clamp(-240.0, 240.0), y.clamp(-180.0, 180.0));
            }
            world.mouse_down = down && !over_ui;
            if pressed && !over_ui { world.mouse_press(); }
            if released { world.mouse_release(); }
            // keyboard: every key of the keyboard (letters, digits, arrows, space, enter...)
            let (events, down_keys) = ctx.input(|i| (i.events.clone(), i.keys_down.clone()));
            let mut pressed_now: Vec<String> = Vec::new();
            for e in &events {
                match e {
                    egui::Event::Key { key, pressed: true, repeat: false, .. } => {
                        let n = key_name(*key);
                        if !pressed_now.contains(&n) { pressed_now.push(n); }
                    }
                    // Fallback: typed characters (some layouts / input methods only send text)
                    egui::Event::Text(t) => {
                        for ch in t.chars().filter(|c| c.is_ascii_alphanumeric()) {
                            let n = ch.to_ascii_lowercase().to_string();
                            if !pressed_now.contains(&n) { pressed_now.push(n); }
                        }
                    }
                    _ => {}
                }
            }
            let mut held: std::collections::HashSet<String> = down_keys.iter().map(|k| key_name(*k)).collect();
            held.extend(pressed_now.iter().cloned());
            world.keys = held;
            if std::env::var_os("ROUILLE3_DEBUG_KEYS").is_some() && !pressed_now.is_empty() {
                eprintln!("[keys] pressed: {:?}", pressed_now);
            }
            if !self.paused {
                for n in &pressed_now { world.key_press(n); }
            }
            // fixed-step simulation
            if !self.paused {
                self.acc += dt;
                let step = 1.0 / self.settings.window.fps.max(1) as f64;
                let mut n = 0;
                while self.acc >= step && n < 8 {
                    world.tick(step);
                    self.acc -= step;
                    n += 1;
                }
                if n == 8 { self.acc = 0.0; }
            }
        }

        // ---- draw
        let status = self.status.clone();
        let family = self.bubble_family.clone();
        let mfam = if self.settings.monitors.enabled { self.monitor_family.clone() } else { None };
        let mut slider_set: Option<(usize, f64)> = None;
        let (world, gfx) = (&self.world, &mut self.gfx);
        egui::CentralPanel::default().frame(egui::Frame::none().fill(Color32::BLACK)).show(ctx, |ui| {
            let Some(world) = world else {
                ui.painter().text(screen.center(), Align2::CENTER_CENTER, status, FontId::proportional(16.0), Color32::GRAY);
                return;
            };
            // earthquake shakes the whole stage
            let mut xf = xf;
            if world.shader_on && world.effects_on.contains("earthquake") {
                let t = world.time as f32;
                xf.origin += Vec2::new((t * 83.0).sin() * 5.0, (t * 67.0).cos() * 5.0) * xf.sx;
            }
            let painter = ui.painter_at(stage);
            painter.rect_filled(stage, 0.0, Color32::WHITE);
            for &s in &world.order {
                let sp = &world.sprites[s];
                if sp.deleted || !sp.visible { continue; }
                // pen layer sits above the backdrop, below sprites
                if sp.is_stage {
                    draw_visual(&painter, ctx, gfx, world, &world.visual(s), &xf);
                    for item in &world.pen_items {
                        match item {
                            PenItem::Line { x0, y0, x1, y1, rgba, size } => {
                                let col = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                                let w = (*size as f32 * xf.sx).max(1.0);
                                let (a, b) = (xf.pt(*x0, *y0), xf.pt(*x1, *y1));
                                painter.line_segment([a, b], Stroke::new(w, col));
                                if w >= 3.0 || a == b {
                                    painter.circle_filled(a, w / 2.0, col);
                                    painter.circle_filled(b, w / 2.0, col);
                                }
                            }
                            PenItem::Stamp(v) => draw_visual(&painter, ctx, gfx, world, v, &xf),
                        }
                    }
                } else {
                    draw_visual(&painter, ctx, gfx, world, &world.visual(s), &xf);
                }
            }
            for &s in &world.order {
                let sp = &world.sprites[s];
                if !sp.deleted && sp.visible && !sp.is_stage {
                    draw_bubble(&painter, stage, world, s, &xf, &family);
                }
            }
            draw_monitors(ui, &painter, world, &xf, &mfam, &mut slider_set);
            draw_screen_effects(&painter, stage, world);
        });
        if let (Some((i, v)), Some(w)) = (slider_set, &mut self.world) {
            w.monitor_set(i, v);
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn key_names_match_scratch() {
        assert_eq!(key_name(egui::Key::Q), "q");
        assert_eq!(key_name(egui::Key::A), "a");
        assert_eq!(key_name(egui::Key::Z), "z");
        assert_eq!(key_name(egui::Key::Num0), "0");
        assert_eq!(key_name(egui::Key::Num7), "7");
        assert_eq!(key_name(egui::Key::Space), "space");
        assert_eq!(key_name(egui::Key::ArrowLeft), "left arrow");
        assert_eq!(key_name(egui::Key::ArrowUp), "up arrow");
        assert_eq!(key_name(egui::Key::Enter), "enter");
    }
}
