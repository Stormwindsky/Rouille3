//! Scratch 3 block interpreter (cooperative threads, one step per tick).
#![allow(dead_code)]
use crate::assets::CostumeMeta;
use crate::audio::Audio;
use crate::project::*;
use crate::settings::Settings;
use crate::tts;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering as AO};
use std::sync::Arc;

// ------------------------------------------------------------------ values

#[derive(Clone, Debug)]
pub enum Val {
    Num(f64),
    Str(String),
    Bool(bool),
}

fn parse_num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => Some(n),
        _ => None,
    }
}

fn fmt_num(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if n == n.trunc() && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

impl Val {
    pub fn to_num(&self) -> f64 {
        match self {
            Val::Num(n) => if n.is_nan() { 0.0 } else { *n },
            Val::Bool(b) => if *b { 1.0 } else { 0.0 },
            Val::Str(s) => parse_num(s).unwrap_or(0.0),
        }
    }
    pub fn to_str(&self) -> String {
        match self {
            Val::Str(s) => s.clone(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => fmt_num(*n),
        }
    }
    pub fn to_bool(&self) -> bool {
        match self {
            Val::Bool(b) => *b,
            Val::Num(n) => *n != 0.0 && !n.is_nan(),
            Val::Str(s) => !(s.is_empty() || s == "0" || s.eq_ignore_ascii_case("false")),
        }
    }
    fn as_num_opt(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            Val::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Val::Str(s) => parse_num(s),
        }
    }
}

fn compare(a: &Val, b: &Val) -> Ordering {
    match (a.as_num_opt(), b.as_num_opt()) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        _ => a.to_str().to_lowercase().cmp(&b.to_str().to_lowercase()),
    }
}

fn json_to_val(v: Option<&Value>) -> Val {
    match v {
        Some(Value::String(s)) => Val::Str(s.clone()),
        Some(Value::Number(n)) => Val::Num(n.as_f64().unwrap_or(0.0)),
        Some(Value::Bool(b)) => Val::Bool(*b),
        _ => Val::Str(String::new()),
    }
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn field_str(b: &Block, name: &str) -> String {
    b.fields.get(name).and_then(|f| f.get(0)).map(value_str).unwrap_or_default()
}

fn field_pair(b: &Block, name: &str) -> (String, String) {
    let f = b.fields.get(name);
    let n = f.and_then(|f| f.get(0)).map(value_str).unwrap_or_default();
    let i = f.and_then(|f| f.get(1)).map(value_str).unwrap_or_default();
    (n, i)
}

fn is_menu(op: &str) -> bool {
    op.contains("_menu") || op.ends_with("menu") || matches!(
        op,
        "looks_costume" | "looks_backdrops" | "sensing_keyoptions" | "pen_menu_colorParam"
            | "sound_sounds_menu" | "motion_goto_menu" | "motion_glideto_menu"
    )
}

fn wrap_dir(d: f64) -> f64 {
    let mut d = d % 360.0;
    if d > 180.0 { d -= 360.0 }
    if d <= -180.0 { d += 360.0 }
    d
}

// ----------------------------------------------------------------- sprites

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RotStyle { All, LeftRight, None }

#[derive(Clone, Debug)]
pub struct Bubble { pub text: String, pub think: bool, pub id: u64 }

#[derive(Clone, Debug)]
pub struct Pen { pub down: bool, pub h: f64, pub s: f64, pub v: f64, pub a: f64, pub size: f64 }

impl Pen {
    pub fn rgba(&self) -> [u8; 4] {
        let (r, g, b) = hsv_to_rgb(self.h * 3.6, self.s / 100.0, self.v / 100.0);
        [r, g, b, (255.0 * (1.0 - self.a / 100.0)).clamp(0.0, 255.0) as u8]
    }
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 60.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p), 1 => (q, v, p), 2 => (p, v, t),
        3 => (p, q, v), 4 => (t, p, v), _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn rgb_to_hsv(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    let h = if d == 0.0 { 0.0 }
        else if mx == r { 60.0 * (((g - b) / d).rem_euclid(6.0)) }
        else if mx == g { 60.0 * ((b - r) / d + 2.0) }
        else { 60.0 * ((r - g) / d + 4.0) };
    (h, if mx == 0.0 { 0.0 } else { d / mx }, mx)
}

#[derive(Clone, Debug)]
pub struct Sprite {
    pub name: String,
    pub target: usize,
    pub is_stage: bool,
    pub is_clone: bool,
    pub deleted: bool,
    pub x: f64,
    pub y: f64,
    pub dir: f64,
    pub size: f64,
    pub visible: bool,
    pub costume: usize,
    pub rot: RotStyle,
    pub draggable: bool,
    /// color, fisheye, whirl, pixelate, mosaic, brightness, ghost
    pub effects: [f64; 7],
    pub volume: f64,
    pub pitch: f64,
    pub voice: String,
    pub vars: HashMap<String, (String, Val)>,
    pub lists: HashMap<String, (String, Vec<Val>)>,
    pub bubble: Option<Bubble>,
    pub pen: Pen,
}

fn effect_index(name: &str) -> Option<usize> {
    match name.to_uppercase().as_str() {
        "COLOR" => Some(0), "FISHEYE" => Some(1), "WHIRL" => Some(2), "PIXELATE" => Some(3),
        "MOSAIC" => Some(4), "BRIGHTNESS" => Some(5), "GHOST" => Some(6), _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Visual {
    pub target: usize,
    pub costume: usize,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub dir: f64,
    pub rot: RotStyle,
    pub effects: [f64; 7],
}

/// Corners (TL, TR, BR, BL) of a costume in stage coordinates (y up).
pub fn corners(m: &CostumeMeta, v: &Visual) -> [(f64, f64); 4] {
    let sc = v.size / 100.0;
    let (mut flip, mut theta) = (1.0, 0.0);
    match v.rot {
        RotStyle::All => theta = (v.dir - 90.0).to_radians(),
        RotStyle::LeftRight => if v.dir < 0.0 { flip = -1.0 },
        RotStyle::None => {}
    }
    let (s, c) = theta.sin_cos();
    let pts = [(0.0, 0.0), (m.w, 0.0), (m.w, m.h), (0.0, m.h)];
    let mut out = [(0.0, 0.0); 4];
    for (i, (cx, cy)) in pts.iter().enumerate() {
        let dx = (*cx as f64 - m.rcx as f64) * sc * flip;
        let dy = -(*cy as f64 - m.rcy as f64) * sc;
        out[i] = (v.x + dx * c + dy * s, v.y - dx * s + dy * c);
    }
    out
}

pub enum PenItem {
    Line { x0: f64, y0: f64, x1: f64, y1: f64, rgba: [u8; 4], size: f64 },
    Stamp(Visual),
}

// ----------------------------------------------------------------- threads

enum Frame {
    Seq(Option<String>),
    Repeat { body: Option<String>, left: i64, fresh: bool },
    Forever { body: Option<String>, fresh: bool },
    Until { owner: String, body: Option<String>, while_mode: bool, fresh: bool },
    Proc { warp: bool },
}

enum Wait {
    None,
    Until(f64),
    Cond(String),
    Glide { t0: f64, dur: f64, sx: f64, sy: f64, ex: f64, ey: f64 },
    Sound(u64),
    Speech(Arc<AtomicBool>),
    Threads(Vec<u64>),
    SayFor { until: f64, bubble: u64 },
}

struct Thread {
    id: u64,
    sprite: usize,
    hat: String,
    stack: Vec<Frame>,
    wait: Wait,
    args: Vec<HashMap<String, Val>>,
    dead: bool,
}

impl Thread {
    fn placeholder() -> Self {
        Thread { id: 0, sprite: 0, hat: String::new(), stack: vec![], wait: Wait::None, args: vec![], dead: true }
    }
}

struct ProcDef {
    next: Option<String>,
    names: Vec<String>,
    warp: bool,
}

enum Act {
    Pop,
    PopProc,
    Exec(String),
    PushBody(Option<String>, bool),
}

// ------------------------------------------------------------------- world

pub struct World {
    pub project: Rc<Project>,
    pub sprites: Vec<Sprite>,
    /// Draw order, bottom to top. The stage is always first.
    pub order: Vec<usize>,
    pub meta: Vec<Vec<CostumeMeta>>,
    assets: HashMap<String, Vec<u8>>,
    pub settings: Settings,
    pub audio: Rc<RefCell<Audio>>,
    threads: Vec<Thread>,
    next_id: u64,
    pub time: f64,
    timer_zero: f64,
    pub keys: HashSet<String>,
    pub mouse: (f64, f64),
    pub mouse_down: bool,
    rng: Cell<u64>,
    gvars: HashMap<String, (String, Val)>,
    glists: HashMap<String, (String, Vec<Val>)>,
    pub stage_idx: usize,
    procs: Vec<HashMap<String, ProcDef>>,
    pub pen_items: Vec<PenItem>,
    // Rouille3 custom blocks
    pub shader_on: bool,
    pub effects_on: HashSet<String>,
    drag: Option<(usize, f64, f64)>,
    stop_all_flag: bool,
    unknown: HashSet<String>,
}

impl World {
    pub fn new(
        project: Project,
        assets: HashMap<String, Vec<u8>>,
        meta: Vec<Vec<CostumeMeta>>,
        settings: Settings,
        audio: Rc<RefCell<Audio>>,
    ) -> World {
        let project = Rc::new(project);
        let mut sprites = Vec::new();
        let mut gvars = HashMap::new();
        let mut glists = HashMap::new();
        let mut stage_idx = 0;
        for (i, t) in project.targets.iter().enumerate() {
            let mut vars = HashMap::new();
            for (id, v) in &t.variables {
                let name = v.get(0).map(value_str).unwrap_or_default();
                vars.insert(id.clone(), (name, json_to_val(v.get(1))));
            }
            let mut lists = HashMap::new();
            for (id, v) in &t.lists {
                let name = v.get(0).map(value_str).unwrap_or_default();
                let items = v.get(1).and_then(|a| a.as_array())
                    .map(|a| a.iter().map(|x| json_to_val(Some(x))).collect()).unwrap_or_default();
                lists.insert(id.clone(), (name, items));
            }
            if t.is_stage {
                stage_idx = i;
                gvars = vars.clone();
                glists = lists.clone();
                vars.clear();
                lists.clear();
            }
            let rot = match t.rotation_style.as_deref() {
                Some("left-right") => RotStyle::LeftRight,
                Some("don't rotate") => RotStyle::None,
                _ => RotStyle::All,
            };
            sprites.push(Sprite {
                name: t.name.clone(), target: i, is_stage: t.is_stage, is_clone: false, deleted: false,
                x: t.x.unwrap_or(0.0), y: t.y.unwrap_or(0.0), dir: t.direction.unwrap_or(90.0),
                size: t.size.unwrap_or(100.0), visible: t.visible.unwrap_or(true) || t.is_stage,
                costume: t.current_costume.unwrap_or(0), rot, draggable: t.draggable.unwrap_or(false),
                effects: [0.0; 7], volume: t.volume.unwrap_or(100.0), pitch: 0.0, voice: "alto".into(),
                vars, lists, bubble: None,
                pen: Pen { down: false, h: 66.66, s: 100.0, v: 100.0, a: 0.0, size: 1.0 },
            });
        }
        let mut order: Vec<usize> = (0..sprites.len()).collect();
        order.sort_by_key(|&i| project.targets[i].layer_order.unwrap_or(i as i64));
        if let Some(p) = order.iter().position(|&i| i == stage_idx) {
            order.remove(p);
            order.insert(0, stage_idx);
        }
        let procs = project.targets.iter().map(build_procs).collect();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(88172645463325252) | 1;
        World {
            project, sprites, order, meta, assets, settings, audio, threads: vec![], next_id: 1,
            time: 0.0, timer_zero: 0.0, keys: HashSet::new(), mouse: (0.0, 0.0), mouse_down: false,
            rng: Cell::new(seed), gvars, glists, stage_idx, procs, pen_items: vec![],
            shader_on: false, effects_on: HashSet::new(), drag: None, stop_all_flag: false,
            unknown: HashSet::new(),
        }
    }

    // ---------------------------------------------------------- public API

    pub fn green_flag(&mut self) {
        self.stop_all();
        self.timer_zero = self.time;
        self.start_hats(|b| b.opcode == "event_whenflagclicked", None);
    }

    pub fn stop_all(&mut self) {
        self.threads.clear();
        self.audio.borrow_mut().stop_all();
        for i in 0..self.sprites.len() {
            if self.sprites[i].is_clone && !self.sprites[i].deleted {
                self.delete_sprite(i);
            }
            self.sprites[i].bubble = None;
            self.sprites[i].effects = [0.0; 7];
        }
        self.shader_on = false;
        self.effects_on.clear();
        self.drag = None;
    }

    pub fn key_press(&mut self, key: &str) {
        let k = key.to_string();
        self.start_hats(
            move |b| b.opcode == "event_whenkeypressed" && {
                let o = field_str(b, "KEY_OPTION");
                o == "any" || o.eq_ignore_ascii_case(&k)
            },
            None,
        );
    }

    pub fn mouse_press(&mut self) {
        self.mouse_down = true;
        if let Some(s) = self.sprite_at(self.mouse) {
            if self.sprites[s].draggable {
                self.drag = Some((s, self.sprites[s].x - self.mouse.0, self.sprites[s].y - self.mouse.1));
            }
            self.start_hats(|b| b.opcode == "event_whenthisspriteclicked", Some(s));
        } else {
            let st = self.stage_idx;
            self.start_hats(|b| b.opcode == "event_whenstageclicked", Some(st));
        }
    }

    pub fn mouse_release(&mut self) {
        self.mouse_down = false;
        self.drag = None;
    }

    pub fn visual(&self, s: usize) -> Visual {
        let sp = &self.sprites[s];
        Visual {
            target: sp.target, costume: sp.costume, x: sp.x, y: sp.y, size: sp.size,
            dir: sp.dir, rot: sp.rot, effects: sp.effects,
        }
    }

    pub fn costume_meta(&self, target: usize, costume: usize) -> CostumeMeta {
        self.meta.get(target).and_then(|m| m.get(costume)).copied().unwrap_or_default()
    }

    pub fn costume_md5(&self, target: usize, costume: usize) -> Option<String> {
        self.project.targets[target].costumes.get(costume).map(|c| c.md5ext.clone())
    }

    pub fn bounds(&self, s: usize) -> (f64, f64, f64, f64) {
        let sp = &self.sprites[s];
        let m = self.costume_meta(sp.target, sp.costume);
        let c = corners(&m, &self.visual(s));
        let xs = c.iter().map(|p| p.0);
        let ys = c.iter().map(|p| p.1);
        (
            xs.clone().fold(f64::MAX, f64::min), ys.clone().fold(f64::MAX, f64::min),
            xs.fold(f64::MIN, f64::max), ys.fold(f64::MIN, f64::max),
        )
    }

    fn sprite_at(&self, p: (f64, f64)) -> Option<usize> {
        for &s in self.order.iter().rev() {
            let sp = &self.sprites[s];
            if sp.deleted || sp.is_stage || !sp.visible {
                continue;
            }
            let (x0, y0, x1, y1) = self.bounds(s);
            if p.0 >= x0 && p.0 <= x1 && p.1 >= y0 && p.1 <= y1 {
                return Some(s);
            }
        }
        None
    }

    pub fn tick(&mut self, dt: f64) {
        self.time += dt;
        if let Some((s, ox, oy)) = self.drag {
            let (mx, my) = self.mouse;
            self.move_to(s, mx + ox, my + oy);
        }
        let mut i = 0;
        while i < self.threads.len() {
            if self.threads[i].dead {
                i += 1;
                continue;
            }
            let mut th = std::mem::replace(&mut self.threads[i], Thread::placeholder());
            self.step(&mut th);
            self.threads[i] = th;
            i += 1;
        }
        if self.stop_all_flag {
            self.stop_all_flag = false;
            self.stop_all();
        }
        self.threads.retain(|t| !t.dead);
        self.audio.borrow_mut().gc();
    }

    // ----------------------------------------------------------- scheduling

    fn start_hats<F: Fn(&Block) -> bool>(&mut self, pred: F, only: Option<usize>) -> Vec<u64> {
        let proj = self.project.clone();
        let mut started = vec![];
        let order: Vec<usize> = self.order.iter().rev().copied().collect();
        for s in order {
            if self.sprites[s].deleted {
                continue;
            }
            if let Some(o) = only {
                if o != s {
                    continue;
                }
            }
            let t = &proj.targets[self.sprites[s].target];
            let mut hats: Vec<(&String, &Block)> = t.blocks.iter().filter_map(|(id, e)| match e {
                BlockEntry::Block(b) if b.top_level && pred(b) => Some((id, b)),
                _ => None,
            }).collect();
            hats.sort_by(|a, b| {
                let ka = (a.1.y.unwrap_or(0.0), a.1.x.unwrap_or(0.0));
                let kb = (b.1.y.unwrap_or(0.0), b.1.x.unwrap_or(0.0));
                ka.partial_cmp(&kb).unwrap_or(Ordering::Equal)
            });
            for (id, b) in hats {
                started.push(self.spawn(s, id.clone(), b.next.clone()));
            }
        }
        started
    }

    fn spawn(&mut self, sprite: usize, hat: String, first: Option<String>) -> u64 {
        for t in self.threads.iter_mut() {
            if t.sprite == sprite && t.hat == hat {
                t.dead = true;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.threads.push(Thread {
            id, sprite, hat, stack: vec![Frame::Seq(first)], wait: Wait::None, args: vec![], dead: false,
        });
        id
    }

    fn broadcast(&mut self, name: &str) -> Vec<u64> {
        let n = name.to_lowercase();
        self.start_hats(
            move |b| b.opcode == "event_whenbroadcastreceived"
                && field_str(b, "BROADCAST_OPTION").to_lowercase() == n,
            None,
        )
    }

    fn kill_sprite_threads(&mut self, s: usize) {
        for t in self.threads.iter_mut() {
            if t.sprite == s {
                t.dead = true;
            }
        }
    }

    fn delete_sprite(&mut self, s: usize) {
        self.sprites[s].deleted = true;
        self.sprites[s].bubble = None;
        self.order.retain(|&i| i != s);
        self.kill_sprite_threads(s);
    }

    fn create_clone(&mut self, src: usize) {
        if self.sprites[src].deleted || self.sprites.iter().filter(|s| s.is_clone && !s.deleted).count() >= 300 {
            return;
        }
        let mut c = self.sprites[src].clone();
        c.is_clone = true;
        c.deleted = false;
        c.bubble = None;
        let idx = match self.sprites.iter().position(|s| s.deleted) {
            Some(i) => { self.sprites[i] = c; i }
            None => { self.sprites.push(c); self.sprites.len() - 1 }
        };
        let pos = self.order.iter().position(|&i| i == src).unwrap_or(1);
        self.order.insert(pos, idx);
        self.start_hats(|b| b.opcode == "control_start_as_clone", Some(idx));
    }

    // ---------------------------------------------------------- stepping

    fn step(&mut self, th: &mut Thread) {
        let proj = self.project.clone();
        let tgt = self.sprites[th.sprite].target;
        let blocks = &proj.targets[tgt].blocks;
        let mut guard = 0u32;
        loop {
            guard += 1;
            if guard > 200_000 {
                return;
            }
            if !self.check_wait(th, blocks) {
                return;
            }
            if th.dead {
                return;
            }
            if th.stack.is_empty() {
                th.dead = true;
                return;
            }
            let in_warp = th.stack.iter().any(|f| matches!(f, Frame::Proc { warp: true }));
            let top = th.stack.len() - 1;
            let act = match &mut th.stack[top] {
                Frame::Seq(next) => match next.take() {
                    None => Act::Pop,
                    Some(id) => {
                        if let Some(BlockEntry::Block(b)) = blocks.get(&id) {
                            *next = b.next.clone();
                        }
                        Act::Exec(id)
                    }
                },
                Frame::Repeat { body, left, fresh } => {
                    if *left <= 0 {
                        Act::Pop
                    } else {
                        *left -= 1;
                        let y = !*fresh;
                        *fresh = false;
                        Act::PushBody(body.clone(), y)
                    }
                }
                Frame::Forever { body, fresh } => {
                    let y = !*fresh;
                    *fresh = false;
                    Act::PushBody(body.clone(), y)
                }
                Frame::Until { owner, body, while_mode, fresh } => {
                    let c = self.cond_of(th.sprite, th.args.last(), blocks, owner);
                    let stop = if *while_mode { !c } else { c };
                    if stop {
                        Act::Pop
                    } else {
                        let y = !*fresh;
                        *fresh = false;
                        Act::PushBody(body.clone(), y)
                    }
                }
                Frame::Proc { .. } => Act::PopProc,
            };
            match act {
                Act::Pop => { th.stack.pop(); }
                Act::PopProc => { th.stack.pop(); th.args.pop(); }
                Act::Exec(id) => {
                    if let Some(BlockEntry::Block(b)) = blocks.get(&id) {
                        self.exec(th, &id, b);
                    }
                }
                Act::PushBody(body, y) => {
                    th.stack.push(Frame::Seq(body));
                    if y && !in_warp {
                        return;
                    }
                }
            }
        }
    }

    fn cond_of(
        &self,
        s: usize,
        args: Option<&HashMap<String, Val>>,
        blocks: &HashMap<String, BlockEntry>,
        owner: &str,
    ) -> bool {
        match blocks.get(owner) {
            Some(BlockEntry::Block(b)) => self.eval_input(s, args, b, "CONDITION").to_bool(),
            _ => false,
        }
    }

    /// Returns true when the thread may continue.
    fn check_wait(&mut self, th: &mut Thread, blocks: &HashMap<String, BlockEntry>) -> bool {
        let w = std::mem::replace(&mut th.wait, Wait::None);
        match w {
            Wait::None => true,
            Wait::Until(t) => {
                if self.time >= t { true } else { th.wait = Wait::Until(t); false }
            }
            Wait::Cond(id) => {
                if self.cond_of(th.sprite, th.args.last(), blocks, &id) { true } else { th.wait = Wait::Cond(id); false }
            }
            Wait::Glide { t0, dur, sx, sy, ex, ey } => {
                let t = if dur <= 0.0 { 1.0 } else { (self.time - t0) / dur };
                if t >= 1.0 {
                    self.move_to(th.sprite, ex, ey);
                    true
                } else {
                    self.move_to(th.sprite, sx + (ex - sx) * t, sy + (ey - sy) * t);
                    th.wait = Wait::Glide { t0, dur, sx, sy, ex, ey };
                    false
                }
            }
            Wait::Sound(id) => {
                if self.audio.borrow().is_playing(id) { th.wait = Wait::Sound(id); false } else { true }
            }
            Wait::Speech(f) => {
                if f.load(AO::SeqCst) { true } else { th.wait = Wait::Speech(f); false }
            }
            Wait::Threads(ids) => {
                if self.threads.iter().any(|t| ids.contains(&t.id) && !t.dead) {
                    th.wait = Wait::Threads(ids);
                    false
                } else { true }
            }
            Wait::SayFor { until, bubble } => {
                if self.time >= until {
                    let sp = &mut self.sprites[th.sprite];
                    if sp.bubble.as_ref().map_or(false, |b| b.id == bubble) {
                        sp.bubble = None;
                    }
                    true
                } else {
                    th.wait = Wait::SayFor { until, bubble };
                    false
                }
            }
        }
    }

    // --------------------------------------------------------- evaluation

    fn blocks_of(&self, s: usize) -> &HashMap<String, BlockEntry> {
        &self.project.targets[self.sprites[s].target].blocks
    }

    fn eval_input(&self, s: usize, args: Option<&HashMap<String, Val>>, b: &Block, name: &str) -> Val {
        let Some(inp) = b.inputs.get(name) else { return Val::Str(String::new()) };
        match inp.get(1) {
            Some(Value::String(id)) => self.eval_id(s, args, id),
            Some(Value::Array(a)) => self.primitive(s, a),
            _ => Val::Str(String::new()),
        }
    }

    fn primitive(&self, s: usize, a: &[Value]) -> Val {
        match a.get(0).and_then(|x| x.as_i64()) {
            Some(12) => {
                let name = a.get(1).map(value_str).unwrap_or_default();
                let id = a.get(2).map(value_str).unwrap_or_default();
                self.var_get(s, &id, &name)
            }
            Some(13) => {
                let name = a.get(1).map(value_str).unwrap_or_default();
                let id = a.get(2).map(value_str).unwrap_or_default();
                let items = self.list_ref(s, &id, &name).map(|l| l.iter().map(|v| v.to_str()).collect::<Vec<_>>().join(" "));
                Val::Str(items.unwrap_or_default())
            }
            _ => json_to_val(a.get(1)),
        }
    }

    fn eval_id(&self, s: usize, args: Option<&HashMap<String, Val>>, id: &str) -> Val {
        match self.blocks_of(s).get(id) {
            Some(BlockEntry::Block(b)) => self.eval_block(s, args, b),
            Some(BlockEntry::Primitive(a)) => self.primitive(s, a),
            None => Val::Str(String::new()),
        }
    }

    fn rand01(&self) -> f64 {
        let mut x = self.rng.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    fn eval_block(&self, s: usize, args: Option<&HashMap<String, Val>>, b: &Block) -> Val {
        let op = b.opcode.as_str();
        if is_menu(op) {
            return Val::Str(b.fields.values().next().and_then(|f| f.get(0)).map(value_str).unwrap_or_default());
        }
        let inp = |n: &str| self.eval_input(s, args, b, n);
        let num = |n: &str| inp(n).to_num();
        let sp = &self.sprites[s];
        match op {
            // ---- operators
            "operator_add" => Val::Num(num("NUM1") + num("NUM2")),
            "operator_subtract" => Val::Num(num("NUM1") - num("NUM2")),
            "operator_multiply" => Val::Num(num("NUM1") * num("NUM2")),
            "operator_divide" => Val::Num(num("NUM1") / num("NUM2")),
            "operator_mod" => {
                let (a, d) = (num("NUM1"), num("NUM2"));
                let mut m = a % d;
                if m * d < 0.0 { m += d }
                Val::Num(m)
            }
            "operator_round" => Val::Num((num("NUM") + 0.5).floor()),
            "operator_lt" => Val::Bool(compare(&inp("OPERAND1"), &inp("OPERAND2")) == Ordering::Less),
            "operator_gt" => Val::Bool(compare(&inp("OPERAND1"), &inp("OPERAND2")) == Ordering::Greater),
            "operator_equals" => Val::Bool(compare(&inp("OPERAND1"), &inp("OPERAND2")) == Ordering::Equal),
            "operator_and" => Val::Bool(inp("OPERAND1").to_bool() && inp("OPERAND2").to_bool()),
            "operator_or" => Val::Bool(inp("OPERAND1").to_bool() || inp("OPERAND2").to_bool()),
            "operator_not" => Val::Bool(!inp("OPERAND").to_bool()),
            "operator_join" => Val::Str(inp("STRING1").to_str() + &inp("STRING2").to_str()),
            "operator_letter_of" => {
                let i = num("LETTER");
                let st = inp("STRING").to_str();
                Val::Str(if i >= 1.0 { st.chars().nth(i as usize - 1).map(|c| c.to_string()).unwrap_or_default() } else { String::new() })
            }
            "operator_length" => Val::Num(inp("STRING").to_str().chars().count() as f64),
            "operator_contains" => Val::Bool(inp("STRING1").to_str().to_lowercase().contains(&inp("STRING2").to_str().to_lowercase())),
            "operator_random" => {
                let (a, c) = (inp("FROM"), inp("TO"));
                let (lo, hi) = (a.to_num().min(c.to_num()), a.to_num().max(c.to_num()));
                let ints = !a.to_str().contains('.') && !c.to_str().contains('.');
                if ints {
                    Val::Num(lo + (self.rand01() * (hi - lo + 1.0)).floor())
                } else {
                    Val::Num(lo + self.rand01() * (hi - lo))
                }
            }
            "operator_mathop" => {
                let n = num("NUM");
                Val::Num(match field_str(b, "OPERATOR").as_str() {
                    "abs" => n.abs(), "floor" => n.floor(), "ceiling" => n.ceil(), "sqrt" => n.sqrt(),
                    "sin" => n.to_radians().sin(), "cos" => n.to_radians().cos(), "tan" => n.to_radians().tan(),
                    "asin" => n.asin().to_degrees(), "acos" => n.acos().to_degrees(), "atan" => n.atan().to_degrees(),
                    "ln" => n.ln(), "log" => n.log10(), "e ^" => n.exp(), "10 ^" => 10f64.powf(n),
                    _ => 0.0,
                })
            }
            // ---- motion / looks / sound reporters
            "motion_xposition" => Val::Num(sp.x),
            "motion_yposition" => Val::Num(sp.y),
            "motion_direction" => Val::Num(sp.dir),
            "looks_size" => Val::Num(sp.size.round()),
            "looks_costumenumbername" => {
                if field_str(b, "NUMBER_NAME") == "name" {
                    Val::Str(self.project.targets[sp.target].costumes.get(sp.costume).map(|c| c.name.clone()).unwrap_or_default())
                } else { Val::Num(sp.costume as f64 + 1.0) }
            }
            "looks_backdropnumbername" => {
                let st = &self.sprites[self.stage_idx];
                if field_str(b, "NUMBER_NAME") == "name" {
                    Val::Str(self.project.targets[st.target].costumes.get(st.costume).map(|c| c.name.clone()).unwrap_or_default())
                } else { Val::Num(st.costume as f64 + 1.0) }
            }
            "sound_volume" => Val::Num(sp.volume),
            // ---- sensing
            "sensing_timer" => Val::Num(self.time - self.timer_zero),
            "sensing_mousex" => Val::Num(self.mouse.0.round()),
            "sensing_mousey" => Val::Num(self.mouse.1.round()),
            "sensing_mousedown" => Val::Bool(self.mouse_down),
            "sensing_keypressed" => {
                let k = inp("KEY_OPTION").to_str().to_lowercase();
                Val::Bool(if k == "any" { !self.keys.is_empty() } else { self.keys.contains(&k) })
            }
            "sensing_touchingobject" => Val::Bool(self.touching(s, &inp("TOUCHINGOBJECTMENU").to_str())),
            "sensing_touchingcolor" | "sensing_coloristouchingcolor" => Val::Bool(false),
            "sensing_distanceto" => {
                let (tx, ty) = self.resolve_pos(s, &inp("DISTANCETOMENU").to_str());
                Val::Num(((tx - sp.x).powi(2) + (ty - sp.y).powi(2)).sqrt())
            }
            "sensing_answer" | "sensing_username" => Val::Str(String::new()),
            "sensing_loudness" => Val::Num(0.0),
            "sensing_online" => Val::Bool(true),
            "sensing_dayssince2000" => {
                let now = chrono::Local::now().timestamp() as f64 / 86400.0;
                Val::Num(now - 10957.0)
            }
            "sensing_current" => {
                use chrono::{Datelike, Timelike};
                let n = chrono::Local::now();
                Val::Num(match field_str(b, "CURRENTMENU").to_uppercase().as_str() {
                    "YEAR" => n.year() as f64, "MONTH" => n.month() as f64, "DATE" => n.day() as f64,
                    "DAYOFWEEK" => n.weekday().num_days_from_sunday() as f64 + 1.0,
                    "HOUR" => n.hour() as f64, "MINUTE" => n.minute() as f64, _ => n.second() as f64,
                })
            }
            "sensing_of" => {
                let obj = inp("OBJECT").to_str();
                let prop = field_str(b, "PROPERTY");
                let ti = if obj == "_stage_" { Some(self.stage_idx) } else { self.find_sprite(&obj) };
                let Some(ti) = ti else { return Val::Num(0.0) };
                let t = &self.sprites[ti];
                match prop.as_str() {
                    "x position" => Val::Num(t.x), "y position" => Val::Num(t.y), "direction" => Val::Num(t.dir),
                    "costume #" | "backdrop #" => Val::Num(t.costume as f64 + 1.0),
                    "costume name" | "backdrop name" => Val::Str(self.project.targets[t.target].costumes.get(t.costume).map(|c| c.name.clone()).unwrap_or_default()),
                    "size" => Val::Num(t.size), "volume" => Val::Num(t.volume),
                    name => self.var_get(ti, "", name),
                }
            }
            // ---- data
            "data_itemoflist" => {
                let (n, id) = field_pair(b, "LIST");
                let idx = inp("INDEX");
                match self.list_ref(s, &id, &n) {
                    Some(l) => self.list_index(&idx, l.len()).map(|i| l[i].clone()).unwrap_or(Val::Str(String::new())),
                    None => Val::Str(String::new()),
                }
            }
            "data_itemnumoflist" => {
                let (n, id) = field_pair(b, "LIST");
                let item = inp("ITEM");
                let pos = self.list_ref(s, &id, &n).and_then(|l| l.iter().position(|v| compare(v, &item) == Ordering::Equal));
                Val::Num(pos.map_or(0.0, |p| p as f64 + 1.0))
            }
            "data_lengthoflist" => {
                let (n, id) = field_pair(b, "LIST");
                Val::Num(self.list_ref(s, &id, &n).map_or(0, |l| l.len()) as f64)
            }
            "data_listcontainsitem" => {
                let (n, id) = field_pair(b, "LIST");
                let item = inp("ITEM");
                Val::Bool(self.list_ref(s, &id, &n).map_or(false, |l| l.iter().any(|v| compare(v, &item) == Ordering::Equal)))
            }
            // ---- custom block arguments
            "argument_reporter_string_number" => {
                let n = field_str(b, "VALUE");
                args.and_then(|a| a.get(&n)).cloned().unwrap_or(Val::Str(String::new()))
            }
            "argument_reporter_boolean" => {
                let n = field_str(b, "VALUE");
                args.and_then(|a| a.get(&n)).cloned().unwrap_or(Val::Bool(false))
            }
            _ => Val::Str(String::new()),
        }
    }

    fn list_index(&self, v: &Val, len: usize) -> Option<usize> {
        if len == 0 {
            return None;
        }
        let st = v.to_str();
        let i = match st.as_str() {
            "last" => len as f64,
            "random" | "any" => 1.0 + (self.rand01() * len as f64).floor(),
            _ => v.to_num().floor(),
        };
        if i >= 1.0 && i <= len as f64 { Some(i as usize - 1) } else { None }
    }

    // --------------------------------------------------- variables & lists

    fn var_get(&self, s: usize, id: &str, name: &str) -> Val {
        if let Some((_, v)) = self.sprites[s].vars.get(id) { return v.clone() }
        if let Some((_, v)) = self.gvars.get(id) { return v.clone() }
        if let Some((_, v)) = self.sprites[s].vars.values().find(|(n, _)| n == name) { return v.clone() }
        if let Some((_, v)) = self.gvars.values().find(|(n, _)| n == name) { return v.clone() }
        Val::Num(0.0)
    }

    fn var_set(&mut self, s: usize, id: &str, name: &str, val: Val) {
        if let Some(e) = self.sprites[s].vars.get_mut(id) { e.1 = val; return }
        if let Some(e) = self.gvars.get_mut(id) { e.1 = val; return }
        if let Some(k) = self.sprites[s].vars.iter().find(|(_, v)| v.0 == name).map(|(k, _)| k.clone()) {
            self.sprites[s].vars.get_mut(&k).unwrap().1 = val;
            return;
        }
        if let Some(k) = self.gvars.iter().find(|(_, v)| v.0 == name).map(|(k, _)| k.clone()) {
            self.gvars.get_mut(&k).unwrap().1 = val;
            return;
        }
        self.gvars.insert(id.to_string(), (name.to_string(), val));
    }

    fn list_ref(&self, s: usize, id: &str, name: &str) -> Option<&Vec<Val>> {
        if let Some((_, l)) = self.sprites[s].lists.get(id) { return Some(l) }
        if let Some((_, l)) = self.glists.get(id) { return Some(l) }
        if let Some((_, l)) = self.sprites[s].lists.values().find(|(n, _)| n == name) { return Some(l) }
        self.glists.values().find(|(n, _)| n == name).map(|(_, l)| l)
    }

    fn list_mut(&mut self, s: usize, id: &str, name: &str) -> &mut Vec<Val> {
        if self.sprites[s].lists.contains_key(id) {
            return &mut self.sprites[s].lists.get_mut(id).unwrap().1;
        }
        if self.glists.contains_key(id) {
            return &mut self.glists.get_mut(id).unwrap().1;
        }
        if let Some(k) = self.sprites[s].lists.iter().find(|(_, v)| v.0 == name).map(|(k, _)| k.clone()) {
            return &mut self.sprites[s].lists.get_mut(&k).unwrap().1;
        }
        if let Some(k) = self.glists.iter().find(|(_, v)| v.0 == name).map(|(k, _)| k.clone()) {
            return &mut self.glists.get_mut(&k).unwrap().1;
        }
        &mut self.glists.entry(id.to_string()).or_insert((name.to_string(), vec![])).1
    }

    // ------------------------------------------------------------ helpers

    fn find_sprite(&self, name: &str) -> Option<usize> {
        self.sprites.iter().position(|s| !s.deleted && !s.is_clone && !s.is_stage && s.name == name)
            .or_else(|| self.sprites.iter().position(|s| !s.deleted && !s.is_stage && s.name == name))
    }

    fn resolve_pos(&self, s: usize, to: &str) -> (f64, f64) {
        match to {
            "_random_" => (-240.0 + self.rand01() * 480.0, -180.0 + self.rand01() * 360.0),
            "_mouse_" => self.mouse,
            name => match self.find_sprite(name) {
                Some(i) => (self.sprites[i].x, self.sprites[i].y),
                None => (self.sprites[s].x, self.sprites[s].y),
            },
        }
    }

    fn touching(&self, s: usize, what: &str) -> bool {
        if !self.sprites[s].visible { return false }
        let (x0, y0, x1, y1) = self.bounds(s);
        match what {
            "_mouse_" => self.mouse.0 >= x0 && self.mouse.0 <= x1 && self.mouse.1 >= y0 && self.mouse.1 <= y1,
            "_edge_" => x0 < -240.0 || x1 > 240.0 || y0 < -180.0 || y1 > 180.0,
            name => (0..self.sprites.len()).any(|o| {
                let sp = &self.sprites[o];
                if o == s || sp.deleted || sp.is_stage || !sp.visible || sp.name != name { return false }
                let (a0, b0, a1, b1) = self.bounds(o);
                x0 <= a1 && x1 >= a0 && y0 <= b1 && y1 >= b0
            }),
        }
    }

    fn move_to(&mut self, s: usize, x: f64, y: f64) {
        let (ox, oy) = (self.sprites[s].x, self.sprites[s].y);
        if ox == x && oy == y { return }
        let pen = &self.sprites[s].pen;
        if pen.down {
            let item = PenItem::Line { x0: ox, y0: oy, x1: x, y1: y, rgba: pen.rgba(), size: pen.size };
            if self.pen_items.len() > 150_000 { self.pen_items.drain(0..20_000); }
            self.pen_items.push(item);
        }
        self.sprites[s].x = x;
        self.sprites[s].y = y;
    }

    fn switch_costume(&mut self, s: usize, v: &Val) {
        let t = self.sprites[s].target;
        let costumes = &self.project.targets[t].costumes;
        let n = costumes.len();
        if n == 0 { return }
        let cur = self.sprites[s].costume;
        let st = v.to_str();
        let idx = if let Some(i) = costumes.iter().position(|c| c.name == st) { i }
            else if st == "next costume" || st == "next backdrop" { (cur + 1) % n }
            else if st == "previous costume" || st == "previous backdrop" { (cur + n - 1) % n }
            else if st == "random backdrop" { (self.rand01() * n as f64) as usize % n }
            else if let Some(x) = v.as_num_opt() { ((x.round() as i64 - 1).rem_euclid(n as i64)) as usize }
            else { return };
        self.sprites[s].costume = idx;
    }

    fn play_sound(&self, s: usize, v: &Val) -> Option<u64> {
        let sp = &self.sprites[s];
        let sounds = &self.project.targets[sp.target].sounds;
        if sounds.is_empty() { return None }
        let st = v.to_str();
        let snd = sounds.iter().find(|x| x.name == st).or_else(|| {
            v.as_num_opt().map(|n| &sounds[((n.round() as i64 - 1).rem_euclid(sounds.len() as i64)) as usize])
        })?;
        let bytes = self.assets.get(&snd.md5ext)?;
        let speed = 2f64.powf(sp.pitch / 120.0) as f32;
        self.audio.borrow_mut().play(bytes, (sp.volume / 100.0) as f32, speed)
    }

    fn set_bubble(&mut self, s: usize, text: String, think: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.sprites[s].bubble = if text.is_empty() { None } else { Some(Bubble { text, think, id }) };
        id
    }

    fn apply_intrinsic(&mut self, i: Intrinsic) {
        match i {
            Intrinsic::Shader { enabled } => {
                if self.settings.custom_blocks.shader { self.shader_on = enabled }
            }
            Intrinsic::Effect { enabled, name } => {
                if !self.settings.custom_blocks.effects { return }
                let n = name.trim().to_lowercase();
                if n == "remove all" {
                    if enabled { self.effects_on.clear() }
                } else if enabled {
                    self.effects_on.insert(n);
                } else {
                    self.effects_on.remove(&n);
                }
            }
            Intrinsic::RemoveAllEffects { enabled } => {
                if enabled && self.settings.custom_blocks.effects { self.effects_on.clear() }
            }
        }
    }

    // ------------------------------------------------------------ execute

    fn exec(&mut self, th: &mut Thread, id: &str, b: &Block) {
        let s = th.sprite;
        macro_rules! val { ($n:expr) => { self.eval_input(s, th.args.last(), b, $n) }; }
        macro_rules! num { ($n:expr) => { val!($n).to_num() }; }
        macro_rules! txt { ($n:expr) => { val!($n).to_str() }; }
        let sub = |name: &str| -> Option<String> {
            b.inputs.get(name).and_then(|v| v.get(1)).and_then(|v| v.as_str()).map(str::to_owned)
        };
        match b.opcode.as_str() {
            // ---------------- events
            "event_broadcast" => { let n = txt!("BROADCAST_INPUT"); self.broadcast(&n); }
            "event_broadcastandwait" => {
                let n = txt!("BROADCAST_INPUT");
                let ids = self.broadcast(&n);
                th.wait = Wait::Threads(ids);
            }
            // ---------------- control
            "control_wait" => { th.wait = Wait::Until(self.time + num!("DURATION")); }
            "control_repeat" => {
                let n = num!("TIMES").round() as i64;
                th.stack.push(Frame::Repeat { body: sub("SUBSTACK"), left: n, fresh: true });
            }
            "control_forever" => th.stack.push(Frame::Forever { body: sub("SUBSTACK"), fresh: true }),
            "control_if" => { if val!("CONDITION").to_bool() { th.stack.push(Frame::Seq(sub("SUBSTACK"))); } }
            "control_if_else" => {
                let c = val!("CONDITION").to_bool();
                th.stack.push(Frame::Seq(sub(if c { "SUBSTACK" } else { "SUBSTACK2" })));
            }
            "control_wait_until" => { if !val!("CONDITION").to_bool() { th.wait = Wait::Cond(id.to_string()); } }
            "control_repeat_until" => th.stack.push(Frame::Until { owner: id.to_string(), body: sub("SUBSTACK"), while_mode: false, fresh: true }),
            "control_while" => th.stack.push(Frame::Until { owner: id.to_string(), body: sub("SUBSTACK"), while_mode: true, fresh: true }),
            "control_stop" => match field_str(b, "STOP_OPTION").as_str() {
                "all" => { self.stop_all_flag = true; th.dead = true; }
                "other scripts in sprite" | "other scripts in stage" => {
                    let me = th.id;
                    for t in self.threads.iter_mut() { if t.sprite == s && t.id != me { t.dead = true; } }
                }
                _ => th.dead = true,
            },
            "control_create_clone_of" => {
                let n = txt!("CLONE_OPTION");
                let src = if n == "_myself_" { Some(s) } else { self.find_sprite(&n) };
                if let Some(src) = src { self.create_clone(src); }
            }
            "control_delete_this_clone" => {
                if self.sprites[s].is_clone { self.delete_sprite(s); th.dead = true; }
            }
            // ---------------- motion
            "motion_movesteps" => {
                let (st, d) = (num!("STEPS"), self.sprites[s].dir.to_radians());
                let (x, y) = (self.sprites[s].x + st * d.sin(), self.sprites[s].y + st * d.cos());
                self.move_to(s, x, y);
            }
            "motion_turnright" => { let d = num!("DEGREES"); self.sprites[s].dir = wrap_dir(self.sprites[s].dir + d); }
            "motion_turnleft" => { let d = num!("DEGREES"); self.sprites[s].dir = wrap_dir(self.sprites[s].dir - d); }
            "motion_goto" => { let (x, y) = self.resolve_pos(s, &txt!("TO")); self.move_to(s, x, y); }
            "motion_gotoxy" => { let (x, y) = (num!("X"), num!("Y")); self.move_to(s, x, y); }
            "motion_glideto" => {
                let (ex, ey) = self.resolve_pos(s, &txt!("TO"));
                th.wait = Wait::Glide { t0: self.time, dur: num!("SECS"), sx: self.sprites[s].x, sy: self.sprites[s].y, ex, ey };
            }
            "motion_glidesecstoxy" => {
                th.wait = Wait::Glide { t0: self.time, dur: num!("SECS"), sx: self.sprites[s].x, sy: self.sprites[s].y, ex: num!("X"), ey: num!("Y") };
            }
            "motion_pointindirection" => { self.sprites[s].dir = wrap_dir(num!("DIRECTION")); }
            "motion_pointtowards" => {
                let to = txt!("TOWARDS");
                let (tx, ty) = if to == "_random_" {
                    self.sprites[s].dir = wrap_dir(-180.0 + self.rand01() * 360.0);
                    return;
                } else { self.resolve_pos(s, &to) };
                let (dx, dy) = (tx - self.sprites[s].x, ty - self.sprites[s].y);
                self.sprites[s].dir = wrap_dir(90.0 - dy.atan2(dx).to_degrees());
            }
            "motion_changexby" => { let (x, y) = (self.sprites[s].x + num!("DX"), self.sprites[s].y); self.move_to(s, x, y); }
            "motion_setx" => { let y = self.sprites[s].y; self.move_to(s, num!("X"), y); }
            "motion_changeyby" => { let (x, y) = (self.sprites[s].x, self.sprites[s].y + num!("DY")); self.move_to(s, x, y); }
            "motion_sety" => { let x = self.sprites[s].x; self.move_to(s, x, num!("Y")); }
            "motion_ifonedgebounce" => self.edge_bounce(s),
            "motion_setrotationstyle" => {
                self.sprites[s].rot = match field_str(b, "STYLE").as_str() {
                    "left-right" => RotStyle::LeftRight, "don't rotate" => RotStyle::None, _ => RotStyle::All,
                };
            }
            // ---------------- looks
            "looks_say" => { let t = txt!("MESSAGE"); self.set_bubble(s, t, false); }
            "looks_think" => { let t = txt!("MESSAGE"); self.set_bubble(s, t, true); }
            "looks_sayforsecs" | "looks_thinkforsecs" => {
                let t = txt!("MESSAGE");
                let bid = self.set_bubble(s, t, b.opcode == "looks_thinkforsecs");
                th.wait = Wait::SayFor { until: self.time + num!("SECS"), bubble: bid };
            }
            "looks_show" => self.sprites[s].visible = true,
            "looks_hide" => self.sprites[s].visible = false,
            "looks_switchcostumeto" => { let v = val!("COSTUME"); self.switch_costume(s, &v); }
            "looks_nextcostume" => self.switch_costume(s, &Val::Str("next costume".into())),
            "looks_switchbackdropto" | "looks_switchbackdroptoandwait" => {
                let v = val!("BACKDROP");
                let st = self.stage_idx;
                self.switch_costume(st, &v);
                let name = self.project.targets[self.sprites[st].target].costumes.get(self.sprites[st].costume).map(|c| c.name.clone()).unwrap_or_default();
                let ids = self.start_hats(move |h| h.opcode == "event_whenbackdropswitchesto" && field_str(h, "BACKDROP") == name, None);
                if b.opcode.ends_with("andwait") { th.wait = Wait::Threads(ids); }
            }
            "looks_nextbackdrop" => { let st = self.stage_idx; self.switch_costume(st, &Val::Str("next backdrop".into())); }
            "looks_changesizeby" => { let n = num!("CHANGE"); self.sprites[s].size = (self.sprites[s].size + n).max(0.0); }
            "looks_setsizeto" => { let n = num!("SIZE"); self.sprites[s].size = n.max(0.0); }
            "looks_changeeffectby" | "looks_seteffectto" => {
                if let Some(i) = effect_index(&field_str(b, "EFFECT")) {
                    let v = num!(if b.opcode == "looks_seteffectto" { "VALUE" } else { "CHANGE" });
                    let e = &mut self.sprites[s].effects[i];
                    *e = if b.opcode == "looks_seteffectto" { v } else { *e + v };
                    if i == 6 { *e = e.clamp(0.0, 100.0) }
                    if i == 5 { *e = e.clamp(-100.0, 100.0) }
                }
            }
            "looks_cleargraphiceffects" => self.sprites[s].effects = [0.0; 7],
            "looks_gotofrontback" => {
                self.order.retain(|&i| i != s);
                if field_str(b, "FRONT_BACK") == "front" { self.order.push(s) } else { self.order.insert(1.min(self.order.len()), s) }
            }
            "looks_goforwardbackwardlayers" => {
                let n = num!("NUM") as i64;
                let dir = if field_str(b, "FORWARD_BACKWARD") == "forward" { 1 } else { -1 };
                if let Some(p) = self.order.iter().position(|&i| i == s) {
                    self.order.remove(p);
                    let np = (p as i64 + dir * n).clamp(1, self.order.len() as i64) as usize;
                    self.order.insert(np, s);
                }
            }
            // ---------------- sound
            "sound_play" => { let v = val!("SOUND_MENU"); self.play_sound(s, &v); }
            "sound_playuntildone" => {
                let v = val!("SOUND_MENU");
                if let Some(sid) = self.play_sound(s, &v) { th.wait = Wait::Sound(sid); }
            }
            "sound_stopallsounds" => self.audio.borrow_mut().stop_all(),
            "sound_changevolumeby" => { let n = num!("VOLUME"); self.sprites[s].volume = (self.sprites[s].volume + n).clamp(0.0, 100.0); }
            "sound_setvolumeto" => { let n = num!("VOLUME"); self.sprites[s].volume = n.clamp(0.0, 100.0); }
            "sound_changeeffectby" | "sound_seteffectto" => {
                if field_str(b, "EFFECT") == "PITCH" {
                    let v = num!("VALUE");
                    self.sprites[s].pitch = if b.opcode == "sound_seteffectto" { v } else { self.sprites[s].pitch + v };
                }
            }
            "sound_cleareffects" => self.sprites[s].pitch = 0.0,
            // ---------------- text to speech
            "text2speech_setVoice" => { self.sprites[s].voice = txt!("VOICE").to_lowercase(); }
            "text2speech_speakAndWait" => {
                let words = txt!("WORDS");
                if !self.audio.borrow().is_muted() {
                    th.wait = Wait::Speech(tts::speak(&self.settings, &self.sprites[s].voice, &words));
                }
            }
            // ---------------- data
            "data_setvariableto" => { let (n, i) = field_pair(b, "VARIABLE"); let v = val!("VALUE"); self.var_set(s, &i, &n, v); }
            "data_changevariableby" => {
                let (n, i) = field_pair(b, "VARIABLE");
                let cur = self.var_get(s, &i, &n).to_num();
                self.var_set(s, &i, &n, Val::Num(cur + num!("VALUE")));
            }
            "data_addtolist" => { let (n, i) = field_pair(b, "LIST"); let v = val!("ITEM"); self.list_mut(s, &i, &n).push(v); }
            "data_deleteoflist" => {
                let (n, i) = field_pair(b, "LIST");
                let idx = val!("INDEX");
                if idx.to_str() == "all" { self.list_mut(s, &i, &n).clear(); }
                else {
                    let len = self.list_mut(s, &i, &n).len();
                    if let Some(k) = self.list_index(&idx, len) { self.list_mut(s, &i, &n).remove(k); }
                }
            }
            "data_deletealloflist" => { let (n, i) = field_pair(b, "LIST"); self.list_mut(s, &i, &n).clear(); }
            "data_insertatlist" => {
                let (n, i) = field_pair(b, "LIST");
                let (idx, v) = (val!("INDEX"), val!("ITEM"));
                let len = self.list_mut(s, &i, &n).len();
                let pos = if idx.to_str() == "last" { Some(len) } else { self.list_index(&idx, len + 1) };
                if let Some(k) = pos { self.list_mut(s, &i, &n).insert(k.min(len), v); }
            }
            "data_replaceitemoflist" => {
                let (n, i) = field_pair(b, "LIST");
                let (idx, v) = (val!("INDEX"), val!("ITEM"));
                let len = self.list_mut(s, &i, &n).len();
                if let Some(k) = self.list_index(&idx, len) { self.list_mut(s, &i, &n)[k] = v; }
            }
            "data_showvariable" | "data_hidevariable" | "data_showlist" | "data_hidelist" => {}
            // ---------------- sensing
            "sensing_resettimer" => self.timer_zero = self.time,
            "sensing_setdragmode" => { self.sprites[s].draggable = field_str(b, "DRAG_MODE") == "draggable"; }
            "sensing_askandwait" => {}
            // ---------------- pen
            "pen_clear" => self.pen_items.clear(),
            "pen_stamp" => { let v = self.visual(s); self.pen_items.push(PenItem::Stamp(v)); }
            "pen_penDown" => {
                self.sprites[s].pen.down = true;
                let (x, y) = (self.sprites[s].x, self.sprites[s].y);
                let p = &self.sprites[s].pen;
                self.pen_items.push(PenItem::Line { x0: x, y0: y, x1: x, y1: y, rgba: p.rgba(), size: p.size });
            }
            "pen_penUp" => self.sprites[s].pen.down = false,
            "pen_setPenColorToColor" => {
                let c = txt!("COLOR");
                let c = c.trim_start_matches('#');
                if c.len() >= 6 {
                    if let Ok(v) = u32::from_str_radix(&c[c.len() - 6..], 16) {
                        let (h, sat, val) = rgb_to_hsv(((v >> 16) & 255) as f64 / 255.0, ((v >> 8) & 255) as f64 / 255.0, (v & 255) as f64 / 255.0);
                        let p = &mut self.sprites[s].pen;
                        p.h = h / 3.6; p.s = sat * 100.0; p.v = val * 100.0;
                    }
                }
            }
            "pen_changePenColorParamBy" | "pen_setPenColorParamTo" => {
                let param = txt!("COLOR_PARAM");
                let v = num!("VALUE");
                let set = b.opcode == "pen_setPenColorParamTo";
                let p = &mut self.sprites[s].pen;
                let f = match param.as_str() {
                    "color" => &mut p.h, "saturation" => &mut p.s, "brightness" => &mut p.v, _ => &mut p.a,
                };
                *f = if set { v } else { *f + v };
                *f = if param == "color" { f.rem_euclid(100.0) } else { f.clamp(0.0, 100.0) };
            }
            "pen_changePenSizeBy" => { let n = num!("SIZE"); let p = &mut self.sprites[s].pen; p.size = (p.size + n).clamp(1.0, 1200.0); }
            "pen_setPenSizeTo" => { let n = num!("SIZE"); self.sprites[s].pen.size = n.clamp(1.0, 1200.0); }
            // ---------------- custom blocks (+ Rouille3 intrinsics)
            "procedures_call" => {
                if let Some(i) = b.intrinsic() { self.apply_intrinsic(i); return; }
                let Some(m) = b.mutation.as_ref() else { return };
                let tgt = self.sprites[s].target;
                let Some(def) = self.procs[tgt].get(&m.proccode) else { return };
                let (next, names, warp) = (def.next.clone(), def.names.clone(), def.warp);
                if th.args.len() >= 150 { return }
                let ids: Vec<String> = serde_json::from_str(&m.argumentids).unwrap_or_default();
                let mut map = HashMap::new();
                for (k, aid) in ids.iter().enumerate() {
                    let v = self.eval_input(s, th.args.last(), b, aid);
                    if let Some(n) = names.get(k) { map.insert(n.clone(), v); }
                }
                th.args.push(map);
                th.stack.push(Frame::Proc { warp });
                th.stack.push(Frame::Seq(next));
            }
            "procedures_definition" | "procedures_prototype" => {}
            op if op.starts_with("event_when") || op == "control_start_as_clone" => {}
            op => {
                if self.unknown.insert(op.to_string()) {
                    eprintln!("[runtime] unsupported block ignored: {op}");
                }
            }
        }
    }

    fn edge_bounce(&mut self, s: usize) {
        let (x0, y0, x1, y1) = self.bounds(s);
        let over = [(-240.0 - x0, 0), (x1 - 240.0, 1), (y1 - 180.0, 2), (-180.0 - y0, 3)];
        let (amt, edge) = over.iter().fold((0.0, 0), |a, &(v, e)| if v > a.0 { (v, e) } else { a });
        if amt <= 0.0 { return }
        let d = self.sprites[s].dir.to_radians();
        let (mut dx, mut dy) = (d.sin(), d.cos());
        let (mut px, mut py) = (0.0, 0.0);
        match edge {
            0 => { dx = dx.abs(); px = amt }
            1 => { dx = -dx.abs(); px = -amt }
            2 => { dy = -dy.abs(); py = -amt }
            _ => { dy = dy.abs(); py = amt }
        }
        self.sprites[s].dir = wrap_dir(dx.atan2(dy).to_degrees());
        let (x, y) = (self.sprites[s].x + px, self.sprites[s].y + py);
        self.move_to(s, x, y);
    }
}

fn build_procs(t: &Target) -> HashMap<String, ProcDef> {
    let mut map = HashMap::new();
    for e in t.blocks.values() {
        if let BlockEntry::Block(b) = e {
            if b.opcode != "procedures_definition" { continue }
            let proto = b.inputs.get("custom_block").and_then(|v| v.get(1)).and_then(|v| v.as_str());
            if let Some(BlockEntry::Block(p)) = proto.and_then(|id| t.blocks.get(id)) {
                if let Some(m) = &p.mutation {
                    let names: Vec<String> = serde_json::from_str(&m.argumentnames).unwrap_or_default();
                    map.insert(m.proccode.clone(), ProcDef { next: b.next.clone(), names, warp: m.warp == "true" });
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the real test project headlessly (no window, no GPU).
    #[test]
    fn runs_test_project() {
        let lp = load_sb3(std::path::Path::new("project/test.sb3")).unwrap();
        let meta = lp.project.targets.iter()
            .map(|t| t.costumes.iter().map(|_| CostumeMeta { w: 96.0, h: 100.0, rcx: 48.0, rcy: 50.0 }).collect())
            .collect();
        let audio = Rc::new(RefCell::new(Audio::new()));
        let mut w = World::new(lp.project, lp.assets, meta, Settings::default(), audio);
        w.green_flag();
        println!("threads started: {}", w.threads.len());
        for i in 0..600 {
            w.tick(1.0 / 30.0);
            if i % 100 == 0 {
                let z = w.sprites.iter().find(|s| s.name == "Znak").unwrap();
                println!("t={:.1}s znak=({:.1},{:.1}) dir={:.0} size={:.0} vis={} bubble={:?} shader={} fx={:?} threads={} gvars={:?}",
                    w.time, z.x, z.y, z.dir, z.size, z.visible, z.bubble.as_ref().map(|b| b.text.clone()),
                    w.shader_on, w.effects_on, w.threads.len(),
                    w.gvars.values().map(|(n, v)| (n.clone(), v.to_str())).collect::<Vec<_>>());
            }
        }
        println!("unsupported blocks: {:?}", w.unknown);
    }
}
