//! What the game task hands the renderer each frame (built on the game thread, drawn in Present).

use lolread::math::M4;
use std::sync::Mutex;

#[derive(Clone, Copy)]
pub struct Sprite {
    /// texture key: a league_assets id, "white" or "font"
    pub tex: &'static str,
    pub additive: bool,
    /// corners in order top-left, top-right, bottom-right, bottom-left
    pub pos: [[f32; 3]; 4],
    pub uv: [[f32; 2]; 4],
    pub color: [f32; 4],
}

/// How a League particle draws: its texture, multiply texture and erosion map (texture keys, "white" when unused)
/// and its blend (0 alpha, 1 additive, 2 premultiplied, 3 screen, 4 multiply).
#[derive(Clone, Copy, PartialEq)]
pub struct VfxMat {
    pub tex: &'static str,
    pub mult: &'static str,
    pub erode: &'static str,
    pub blend: u8,
    /// bit 1: the texture repeats, bit 2: the multiply texture repeats (else clamped at the edges)
    pub wrap: u8,
}

#[derive(Clone, Copy)]
pub struct VfxVert {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    /// multiply texture uv
    pub uv2: [f32; 2],
    pub color: [f32; 4],
    /// erosion: threshold, feather (feather 0 = off)
    pub erode: [f32; 2],
}

#[derive(Clone, Default)]
pub struct Scene {
    /// Fiddlesticks (model 0) and his crows (model 1) in the world
    pub models: Vec<ModelDraw>,
    /// camera world matrix (rows right, up, forward, position) and vertical fov in radians
    pub cam: M4,
    pub fov: f32,
    pub near: f32,
    pub far: f32,
    /// billboards and beams in world space (metres)
    pub world: Vec<Sprite>,
    /// League particles in world space: triangle lists by material, drawn in order before the billboards
    pub vfx: Vec<(VfxMat, Vec<VfxVert>)>,
    /// HUD in a 1920x1080 reference frame (z unused)
    pub screen: Vec<Sprite>,
    /// save this frame as a PNG at this path (test runs)
    pub shot: Option<String>,
}

#[derive(Clone)]
pub struct ModelDraw {
    /// 0 = Fiddlesticks, 1 = his crow
    pub model: usize,
    pub world: M4,
    pub palette: Vec<M4>,
    pub visible: Vec<bool>,
}

pub static SCENE: Mutex<Option<Scene>> = Mutex::new(None);

pub fn quad_uv() -> [[f32; 2]; 4] {
    [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
}
