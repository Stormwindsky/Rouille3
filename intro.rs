//! Intro video. Decoded by the external `ffmpeg` / `ffprobe` programs (separate
//! processes, so no codec or license is linked into Rouille3).
use crate::audio::Audio;
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::cell::RefCell;
use std::io::Read;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::time::Instant;

pub struct Intro {
    rx: Receiver<Vec<u8>>,
    pub w: usize,
    pub h: usize,
    fps: f64,
    start: Instant,
    shown: i64,
    pub tex: Option<TextureHandle>,
    audio: Rc<RefCell<Audio>>,
}

fn probe(path: &str) -> Option<(usize, usize, f64)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries",
               "stream=width,height,r_frame_rate", "-of", "default=noprint_wrappers=1", path])
        .output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (mut w, mut h, mut fps) = (0usize, 0usize, 30.0);
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            match k {
                "width" => w = v.trim().parse().ok()?,
                "height" => h = v.trim().parse().ok()?,
                "r_frame_rate" => {
                    let (a, b) = v.trim().split_once('/').unwrap_or((v.trim(), "1"));
                    let (a, b): (f64, f64) = (a.parse().ok()?, b.parse().ok()?);
                    if b > 0.0 && a > 0.0 { fps = a / b }
                }
                _ => {}
            }
        }
    }
    if w == 0 || h == 0 { None } else { Some((w, h, fps)) }
}

impl Intro {
    pub fn start(path: &str, audio: Rc<RefCell<Audio>>) -> Option<Intro> {
        if !std::path::Path::new(path).exists() {
            eprintln!("[intro] file not found: {path}");
            return None;
        }
        let Some((mut w, mut h, fps)) = probe(path) else {
            eprintln!("[intro] ffprobe failed (is ffmpeg installed?)");
            return None;
        };
        if w > 1280 {
            h = h * 1280 / w;
            w = 1280;
        }
        w &= !1;
        h &= !1;

        // Audio: extract to a temporary wav, then play it with rodio.
        let tmp = std::env::temp_dir().join("rouille3_intro.wav");
        let ok = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-i", path, "-vn", "-ac", "2", "-ar", "44100"])
            .arg(&tmp).stdout(Stdio::null()).stderr(Stdio::null()).status()
            .map_or(false, |s| s.success());
        if ok {
            if let Ok(bytes) = std::fs::read(&tmp) {
                audio.borrow_mut().play(&bytes, 1.0, 1.0);
            }
        }

        // Video: raw RGBA frames through a pipe.
        let mut child = Command::new("ffmpeg")
            .args(["-v", "error", "-i", path, "-an", "-vf", &format!("scale={w}:{h}"),
                   "-pix_fmt", "rgba", "-f", "rawvideo", "-"])
            .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut stdout = child.stdout.take()?;
        let (tx, rx) = sync_channel::<Vec<u8>>(6);
        std::thread::spawn(move || {
            loop {
                let mut buf = vec![0u8; w * h * 4];
                if stdout.read_exact(&mut buf).is_err() || tx.send(buf).is_err() {
                    break;
                }
            }
            let _ = child.wait();
        });
        Some(Intro { rx, w, h, fps, start: Instant::now(), shown: 0, tex: None, audio })
    }

    /// Advances playback. Returns true when the intro is over.
    pub fn update(&mut self, ctx: &egui::Context, skippable: bool) -> bool {
        if skippable && ctx.input(|i| i.pointer.any_pressed() || i.key_pressed(egui::Key::Space)
            || i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Escape)) {
            self.audio.borrow_mut().stop_all();
            return true;
        }
        let target = (self.start.elapsed().as_secs_f64() * self.fps) as i64 + 1;
        let mut latest = None;
        let mut ended = false;
        while self.shown < target {
            match self.rx.try_recv() {
                Ok(f) => { latest = Some(f); self.shown += 1; }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => { ended = true; break; }
            }
        }
        if let Some(f) = latest {
            let img = ColorImage::from_rgba_unmultiplied([self.w, self.h], &f);
            match &mut self.tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.tex = Some(ctx.load_texture("intro", img, TextureOptions::LINEAR)),
            }
        }
        if ended {
            self.audio.borrow_mut().stop_all();
        }
        ended
    }
}
