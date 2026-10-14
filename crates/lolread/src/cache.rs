//! The local cache prep writes from the player's League install and the DLL reads. Never shipped.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CACHE_VERSION: u32 = 5;

#[derive(Serialize, Deserialize, Clone)]
pub struct Joint {
    pub name: String,
    pub parent: i16,
    pub t: [f32; 3],
    pub r: [f32; 4],
    pub s: [f32; 3],
    /// inverse bind matrix, row-major, row vectors (v * M)
    pub ibind: [f32; 16],
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u16; 4],
    pub weights: [f32; 4],
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Submesh {
    pub name: String,
    pub first_index: u32,
    pub index_count: u32,
    pub texture: u32,
    pub hidden: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Tex {
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub mips: Vec<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Model {
    pub joints: Vec<Joint>,
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
    pub submeshes: Vec<Submesh>,
    pub textures: Vec<Tex>,
    pub height: f32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Track {
    pub joint: u16,
    pub t: Vec<[f32; 3]>,
    pub r: Vec<[f32; 4]>,
    pub s: Vec<[f32; 3]>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SubmeshEvent {
    pub start: f32,
    /// negative: until the clip ends
    pub end: f32,
    pub show: Vec<String>,
    pub hide: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Clip {
    pub id: String,
    pub league_name: String,
    pub fps: f32,
    pub frames: u32,
    pub tracks: Vec<Track>,
    pub submesh_events: Vec<SubmeshEvent>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Spell {
    /// DataValues and the spell's own per-rank arrays (CooldownTime, Mana, CastRange, castRadius, ...)
    pub values: BTreeMap<String, Vec<f32>>,
    pub cast_frame: f32,
    pub cast_time: f32,
    pub missile_speed: f32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Cache {
    pub version: u32,
    pub league_version: String,
    pub locale: String,
    pub model: Model,
    pub clips: Vec<Clip>,
    pub stats: BTreeMap<String, f32>,
    /// ability id (sheet) -> spell
    pub spells: BTreeMap<String, Spell>,
    /// league_assets id -> texture
    pub icons: BTreeMap<String, Tex>,
    /// sound id (sheet) -> wav files in cache/audio
    pub sounds: BTreeMap<String, Vec<String>>,
    /// Fiddlesticks' crow (particle model) and its flight loop, when the install has them
    pub raven: Option<(Model, Clip)>,
}

impl Spell {
    pub fn at(&self, name: &str, rank: u32) -> f32 {
        self.values
            .get(name)
            .and_then(|v| v.get(rank as usize).or(v.last()).copied())
            .unwrap_or(0.0)
    }
}

pub fn save(c: &Cache) -> anyhow::Result<Vec<u8>> {
    Ok(bincode::serialize(c)?)
}

pub fn load(b: &[u8]) -> anyhow::Result<Cache> {
    let c: Cache = bincode::deserialize(b)?;
    if c.version != CACHE_VERSION {
        anyhow::bail!("cache version {} (need {CACHE_VERSION})", c.version);
    }
    Ok(c)
}
