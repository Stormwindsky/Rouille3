//! Text to speech through the external `espeak-ng` executable.
//! Spawned as a separate process on purpose: espeak-ng is GPL-3.0, and this
//! keeps Rouille3 (MIT) free of any GPL linking.
use crate::settings::Settings;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Starts speaking in the background. The returned flag becomes `true` when done.
pub fn speak(settings: &Settings, voice_name: &str, text: &str) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    let voice = match settings.tts.voices.get(&voice_name.to_lowercase()) {
        Some(v) if settings.tts.enabled => v.clone(),
        _ => {
            done.store(true, Ordering::SeqCst);
            return done;
        }
    };
    let words = voice.override_text.clone().unwrap_or_else(|| text.to_string());
    let exe = settings.tts.executable.clone();
    let flag = done.clone();
    std::thread::spawn(move || {
        let child = Command::new(&exe)
            .args(["-v", &voice.voice, "-p", &voice.pitch.to_string(), "-s", &voice.speed.to_string(), "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(mut c) => {
                if let Some(mut stdin) = c.stdin.take() {
                    let _ = stdin.write_all(words.as_bytes());
                }
                let _ = c.wait();
            }
            Err(e) => eprintln!("[tts] cannot run '{exe}': {e} (is espeak-ng installed?)"),
        }
        flag.store(true, Ordering::SeqCst);
    });
    done
}
