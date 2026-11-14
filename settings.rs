//! `settings.json5` - every field has a default so the file stays optional.
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Settings {
    pub game: Game,
    pub window: Window,
    pub intro: Intro,
    pub controls: Controls,
    pub downloads: Downloads,
    pub speech_bubbles: SpeechBubbles,
    pub fonts: Vec<FontEntry>,
    pub tts: Tts,
    pub custom_blocks: CustomBlocks,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Game {
    pub title: String,
    pub project_dir: String,
    /// None = auto-detect the first .sb3 in `project_dir`.
    pub sb3: Option<String>,
}
impl Default for Game {
    fn default() -> Self {
        Self { title: "Rouille3".into(), project_dir: "project".into(), sb3: None }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Window {
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub fullscreen: bool,
    pub fps: u32,
    /// Window / taskbar icon (null = default icon).
    pub icon: Option<String>,
    /// "fit" = keep 4:3 and fill the window (black bars), "stretch" = fill without keeping the ratio.
    pub scaling: String,
}
impl Default for Window {
    fn default() -> Self {
        Self { width: 480, height: 360, resizable: true, fullscreen: false, fps: 30, icon: Some("icon.png".into()), scaling: "fit".into() }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Intro { pub enabled: bool, pub file: String, pub pausable: bool, pub skippable: bool }
impl Default for Intro {
    fn default() -> Self {
        Self { enabled: false, file: "project/other/intro.mp4".into(), pausable: false, skippable: false }
    }
}

fn path(p: &str) -> Option<String> { Some(p.to_string()) }

/// Green flag. After it is pressed it turns into a pause / resume button.
#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct FlagButton {
    pub visible: bool,
    /// top-left, top-center, top-right, bottom-left, bottom-center, bottom-right
    pub position: String,
    pub size: f32,
    pub margin: f32,
    /// Icon shown while the project is not running, or paused (click = start / resume).
    pub icon_play: Option<String>,
    /// Icon shown while the project is running (click = pause).
    pub icon_pause: Option<String>,
}
impl Default for FlagButton {
    fn default() -> Self {
        Self {
            visible: true, position: "top-left".into(), size: 32.0, margin: 8.0,
            icon_play: path("project/other/Play.png"), icon_pause: path("project/other/Pause.png"),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct StopButton {
    pub visible: bool,
    pub position: String,
    pub size: f32,
    pub margin: f32,
    pub icon: Option<String>,
    /// "restart" = start the project again from the beginning, "stop" = stop it.
    pub mode: String,
}
impl Default for StopButton {
    fn default() -> Self {
        Self {
            visible: true, position: "top-left".into(), size: 32.0, margin: 8.0,
            icon: path("project/other/Redo.png"), mode: "restart".into(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct VolumeButton {
    pub visible: bool,
    pub position: String,
    pub size: f32,
    pub margin: f32,
    /// Icon while the sound is on (click = mute).
    pub icon_on: Option<String>,
    /// Icon while the sound is muted (click = unmute).
    pub icon_off: Option<String>,
}
impl Default for VolumeButton {
    fn default() -> Self {
        Self {
            visible: true, position: "top-left".into(), size: 32.0, margin: 8.0,
            icon_on: path("project/other/Speaker-0.png"), icon_off: path("project/other/Speaker-Crossed.png"),
        }
    }
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Controls {
    pub green_flag: FlagButton,
    pub stop: StopButton,
    pub volume: VolumeButton,
    pub auto_start: bool,
}

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Downloads { pub enabled: bool, pub assets: HashMap<String, String> }

#[derive(Debug, Deserialize, Default, Clone)]
#[serde(default)]
pub struct SpeechBubbles {
    /// Path to a .ttf / .otf used for speech bubbles. No default font on purpose.
    pub font: Option<String>,
    /// Escape hatch: use the engine's built-in UI font when `font` is null.
    pub use_builtin_font: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FontEntry { pub name: String, pub file: String }

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct Tts { pub enabled: bool, pub executable: String, pub voices: HashMap<String, Voice> }
impl Default for Tts {
    fn default() -> Self { Self { enabled: true, executable: "espeak-ng".into(), voices: HashMap::new() } }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Voice {
    pub voice: String,
    pub pitch: u32, // espeak-ng -p, 0..99
    pub speed: u32, // espeak-ng -s, words per minute
    #[serde(default)]
    pub override_text: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct CustomBlocks { pub shader: bool, pub effects: bool }
impl Default for CustomBlocks { fn default() -> Self { Self { shader: true, effects: true } } }

fn default_voices() -> HashMap<String, Voice> {
    let v = |pitch, speed, text: Option<&str>| Voice {
        voice: "en".into(), pitch, speed, override_text: text.map(str::to_owned),
    };
    let mut m = HashMap::new();
    m.insert("alto".into(), v(55, 175, None));
    m.insert("tenor".into(), v(35, 170, None));
    m.insert("squeak".into(), v(90, 190, None));
    m.insert("giant".into(), v(5, 130, None));
    m.insert("kitten".into(), v(99, 175, Some("meow")));
    m
}

impl Settings {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let mut s: Settings = match std::fs::read_to_string(path) {
            Ok(text) => json5::from_str(&text).map_err(|e| anyhow::anyhow!("settings.json5: {e}"))?,
            Err(_) => Settings::default(), // no settings.json5 -> defaults
        };
        for (k, v) in default_voices() {
            s.tts.voices.entry(k).or_insert(v);
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_settings_file_and_icons_load() {
        let s = Settings::load("settings.json5").expect("settings.json5 must parse");
        assert!(s.controls.green_flag.visible && s.controls.stop.visible && s.controls.volume.visible);
        assert_eq!(s.controls.stop.mode, "restart");
        assert_eq!(s.window.icon.as_deref(), Some("icon.png"));
        let c = &s.controls;
        let icons = [
            &c.green_flag.icon_play, &c.green_flag.icon_pause, &c.stop.icon, &c.volume.icon_on, &c.volume.icon_off,
        ];
        for p in icons {
            let p = p.as_ref().expect("icon path set");
            let bytes = std::fs::read(p).unwrap_or_else(|_| panic!("missing {p}"));
            let (img, w, h) = crate::assets::decode_image(&bytes, "png").unwrap_or_else(|e| panic!("{p}: {e}"));
            println!("{p}: {}x{} ({w}x{h})", img.size[0], img.size[1]);
        }
        assert!(std::fs::read("icon.png").is_ok());
        // an explicit null disables an icon (fallback drawing is used instead)
        let n: Settings = json5::from_str("{ controls: { stop: { icon: null, visible: false } } }").unwrap();
        assert!(n.controls.stop.icon.is_none() && !n.controls.stop.visible);
        assert!(n.controls.volume.icon_on.is_some()); // others keep their defaults
    }
}
