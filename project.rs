//! Scratch 3 / TurboWarp project model + .sb3 loader.
#![allow(dead_code)]
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Project {
    pub targets: Vec<Target>,
    #[serde(default)]
    pub monitors: Vec<Value>,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub meta: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub is_stage: bool,
    pub name: String,
    #[serde(default)]
    pub variables: HashMap<String, Value>,
    #[serde(default)]
    pub lists: HashMap<String, Value>,
    #[serde(default)]
    pub broadcasts: HashMap<String, String>,
    #[serde(default)]
    pub blocks: HashMap<String, BlockEntry>,
    #[serde(default)]
    pub comments: HashMap<String, Comment>,
    #[serde(default)]
    pub costumes: Vec<Costume>,
    #[serde(default)]
    pub sounds: Vec<Sound>,
    #[serde(default)]
    pub volume: Option<f64>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub size: Option<f64>,
    #[serde(default)]
    pub direction: Option<f64>,
    #[serde(default)]
    pub visible: Option<bool>,
    #[serde(default)]
    pub layer_order: Option<i64>,
    #[serde(default)]
    pub current_costume: Option<usize>,
    #[serde(default)]
    pub rotation_style: Option<String>,
    #[serde(default)]
    pub draggable: Option<bool>,
}

/// A block is either a full object or a primitive array
/// (e.g. `[12, "my variable", "id"]` for a variable reporter).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BlockEntry {
    Block(Block),
    Primitive(Vec<Value>),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Block {
    pub opcode: String,
    pub next: Option<String>,
    pub parent: Option<String>,
    #[serde(default)]
    pub inputs: HashMap<String, Value>,
    #[serde(default)]
    pub fields: HashMap<String, Value>,
    #[serde(default)]
    pub top_level: bool,
    #[serde(default)]
    pub mutation: Option<Mutation>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct Mutation {
    #[serde(default)]
    pub proccode: String,
    #[serde(default)]
    pub argumentids: String, // JSON-encoded array, as stored by Scratch
    #[serde(default)]
    pub argumentnames: String,
    #[serde(default)]
    pub warp: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub block_id: Option<String>,
    pub text: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Costume {
    pub name: String,
    pub asset_id: String,
    pub md5ext: String,
    pub data_format: String, // "svg" | "png" | "jpg" ...
    #[serde(default)]
    pub rotation_center_x: f64,
    #[serde(default)]
    pub rotation_center_y: f64,
    #[serde(default)]
    pub bitmap_resolution: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sound {
    pub name: String,
    pub asset_id: String,
    pub md5ext: String,
    pub data_format: String, // "wav" | "mp3"
    #[serde(default)]
    pub rate: u32,
    #[serde(default)]
    pub sample_count: u64,
}

// ---------------------------------------------------------------- .sb3 ----

/// A project plus the raw bytes of every asset found inside the .sb3.
pub struct LoadedProject {
    pub project: Project,
    /// file name inside the .sb3 (e.g. "75cb...svg") -> bytes
    pub assets: HashMap<String, Vec<u8>>,
}

const ASSET_EXTENSIONS: [&str; 7] = ["png", "svg", "jpg", "jpeg", "bmp", "wav", "mp3"];

/// Opens a .sb3 (a zip), reads its `project.json` and keeps every costume /
/// sound file (.png .svg .jpg .wav .mp3 ...). LICENSE and README.md inside
/// the archive are ignored.
pub fn load_sb3(path: &Path) -> anyhow::Result<LoadedProject> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut project_json: Option<String> = None;
    let mut assets = HashMap::new();

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if !entry.is_file() {
            continue;
        }
        let name = entry.name().to_string();
        if name == "project.json" {
            let mut s = String::new();
            entry.read_to_string(&mut s)?;
            project_json = Some(s);
        } else {
            let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            if ASSET_EXTENSIONS.contains(&ext.as_str()) {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                assets.insert(name, buf);
            }
        }
    }

    let json = project_json.ok_or_else(|| anyhow::anyhow!("no project.json inside {}", path.display()))?;
    let project: Project = serde_json::from_str(&json)?;
    Ok(LoadedProject { project, assets })
}

impl LoadedProject {
    /// Asset files referenced by the project but absent from the .sb3.
    pub fn missing_assets(&self) -> Vec<String> {
        let mut missing = Vec::new();
        for t in &self.project.targets {
            for c in &t.costumes {
                if !self.assets.contains_key(&c.md5ext) { missing.push(c.md5ext.clone()); }
            }
            for s in &t.sounds {
                if !self.assets.contains_key(&s.md5ext) { missing.push(s.md5ext.clone()); }
            }
        }
        missing.sort();
        missing.dedup();
        missing
    }
}

// ----------------------------------------------------- Rouille3 blocks ----

/// Rouille3-only blocks. In Scratch/TurboWarp they are plain custom blocks
/// with no definition (so they do nothing there); Rouille3 recognises them
/// by their `proccode` and runs them natively.
#[derive(Debug, PartialEq)]
pub enum Intrinsic {
    /// `(Shader =) %s`
    Shader { enabled: bool },
    /// `(Effect =) %s (Name of this effect =) %s`
    Effect { enabled: bool, name: String },
    /// `(Remove All Effects =) %s`
    RemoveAllEffects { enabled: bool },
}

pub fn parse_bool(s: &str) -> bool {
    s.trim().eq_ignore_ascii_case("true")
}

/// Value of a literal text input like `[1, [10, "true"]]`.
fn literal(v: &Value) -> Option<String> {
    v.get(1)?.get(1)?.as_str().map(str::to_owned)
}

impl Block {
    pub fn intrinsic(&self) -> Option<Intrinsic> {
        if self.opcode != "procedures_call" {
            return None;
        }
        let m = self.mutation.as_ref()?;
        let ids: Vec<String> = serde_json::from_str(&m.argumentids).ok()?;
        let arg = |i: usize| ids.get(i).and_then(|id| self.inputs.get(id)).and_then(literal);
        match m.proccode.as_str() {
            "(Shader =) %s" => Some(Intrinsic::Shader { enabled: parse_bool(&arg(0)?) }),
            "(Effect =) %s (Name of this effect =) %s" => Some(Intrinsic::Effect {
                enabled: parse_bool(&arg(0)?),
                name: arg(1)?,
            }),
            "(Remove All Effects =) %s" => {
                Some(Intrinsic::RemoveAllEffects { enabled: parse_bool(&arg(0)?) })
            }
            _ => None,
        }
    }
}
