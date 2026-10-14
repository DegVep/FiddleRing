//! League particle systems (`VfxSystemDefinitionData`) read from a skin's bins into a compact form the DLL plays:
//! emitters with their curves, textures, meshes and children. Field meanings follow League's own defaults
//! (LeagueToolkit's meta classes); units are League units (the DLL scales them like Fiddlesticks' model).

use crate::bin::{Bin, Value};
use crate::cache::Tex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A League `Value*`: a constant, or a curve over time (0..1), times random multipliers per component.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Val {
    pub c: [f32; 4],
    pub times: Vec<f32>,
    pub vals: Vec<[f32; 4]>,
    /// per component: (key times, key values) of a probability table; a uniform draw picks a multiplier
    pub rand: Vec<(Vec<f32>, Vec<f32>)>,
}

impl Val {
    pub fn constant(c: [f32; 4]) -> Self {
        Val { c, ..Default::default() }
    }

    /// The value at `t` (0..1): the curve when there is one, else the constant.
    pub fn at(&self, t: f32) -> [f32; 4] {
        if self.times.is_empty() || self.vals.is_empty() {
            return self.c;
        }
        let n = self.times.len().min(self.vals.len());
        if t <= self.times[0] || n == 1 {
            return self.vals[0];
        }
        for i in 1..n {
            if t <= self.times[i] {
                let (t0, t1) = (self.times[i - 1], self.times[i]);
                let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1.0 };
                let (a, b) = (self.vals[i - 1], self.vals[i]);
                return [0, 1, 2, 3].map(|k| a[k] + (b[k] - a[k]) * f);
            }
        }
        self.vals[n - 1]
    }

    /// The value at `t` with its random multipliers drawn from `rnd` (uniform 0..1).
    pub fn sample(&self, t: f32, mut rnd: impl FnMut() -> f32) -> [f32; 4] {
        let mut v = self.at(t);
        for (k, (kt, kv)) in self.rand.iter().enumerate().take(4) {
            if kt.len() >= 2 && kv.len() == kt.len() {
                let r = rnd();
                let tab = Val { times: kt.clone(), vals: kv.iter().map(|&x| [x, 0.0, 0.0, 0.0]).collect(), ..Default::default() };
                v[k] *= tab.at(r)[0];
            }
        }
        v
    }

    pub fn is_set(&self) -> bool {
        !self.times.is_empty() || self.c != [0.0; 4]
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Prim {
    /// a camera-facing quad
    Billboard,
    /// a quad oriented by its rotation (flat on the ground for ground layers)
    Quad,
    /// a camera-facing quad stretched along its velocity
    Ray,
    /// a strip from the effect's origin to its target
    Beam { tiling: f32, segments: u32 },
    /// a static .scb mesh
    Mesh { mesh: String },
    /// Fiddlesticks' crow (skinned, animated)
    Crow,
    /// a ribbon along the particle's path
    Trail { tiling: f32 },
    /// drawn over the champion's own model in League: not reproduced
    Attached,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Shape {
    Point,
    Sphere(f32),
    Box([f32; 3]),
    Cylinder { radius: f32, height: f32 },
    /// emit offset, then rotated by angles (degrees) about axes
    Legacy { offset: Val, angles: Vec<Val>, axes: Vec<[f32; 3]> },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Mult {
    pub texture: String,
    pub tex_div: [f32; 2],
    pub scroll: Val,
    pub scroll_integrated: Val,
    pub uv_scale: Val,
    /// texAddressModeMult 2: the texture repeats (else it is clamped at its edges)
    pub wrap: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Erosion {
    pub map: String,
    pub drive: Val,
    pub feather_in: f32,
    pub feather_out: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum Child {
    /// a key of the skin's resource map
    Key(u32),
    /// a system by its object path hash
    System(u32),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Emitter {
    pub name: String,
    pub rate: Val,
    pub particle_lifetime: Val,
    /// how long it emits (None: as long as the effect lives)
    pub lifetime: Option<f32>,
    pub first_emission: f32,
    pub single: bool,
    pub linger: f32,
    pub birth_color: Val,
    pub color: Val,
    pub birth_scale: Val,
    pub scale: Val,
    pub uniform_scale: bool,
    pub birth_rotation: Val,
    pub birth_rot_velocity: Val,
    pub birth_velocity: Val,
    pub birth_acceleration: Val,
    pub world_acceleration: Val,
    pub birth_drag: Val,
    pub birth_orbital: Val,
    pub position: Val,
    pub shape: Shape,
    pub bind_weight: f32,
    pub texture: String,
    pub tex_div: [f32; 2],
    pub num_frames: u32,
    pub frame_rate: f32,
    pub random_frame: bool,
    pub uv_scroll_birth: Val,
    pub uv_scroll: Val,
    pub uv_scale: Val,
    pub uv_offset: Val,
    pub mult: Option<Mult>,
    pub erosion: Option<Erosion>,
    pub blend: u8,
    pub pass: i16,
    pub ground: bool,
    pub direction_oriented: bool,
    pub direction_velocity_scale: f32,
    pub prim: Prim,
    pub children: Vec<Child>,
    pub child_on_death: bool,
    /// a screen distortion (refraction) League draws into its distortion pass: not a visible layer
    pub distortion: bool,
    /// texAddressModeBase 2: the texture repeats (League's default clamps it at its edges)
    pub wrap: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct System {
    pub name: String,
    pub emitters: Vec<Emitter>,
}

/// A static particle mesh as a triangle soup (3 vertices per face): position and uv.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct StaticMesh {
    pub verts: Vec<([f32; 3], [f32; 2])>,
}

/// Every effect of one skin: its resource keys, systems (by object path hash), textures and meshes.
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct VfxLib {
    pub keys: BTreeMap<u32, u32>,
    pub systems: BTreeMap<u32, System>,
    pub textures: BTreeMap<String, Tex>,
    pub meshes: BTreeMap<String, StaticMesh>,
}

impl VfxLib {
    /// The system a resource key (e.g. "Fiddlesticks_R_AOE", or its hash) names in this skin.
    pub fn by_key(&self, key: u32) -> Option<(u32, &System)> {
        let path = *self.keys.get(&key)?;
        self.systems.get(&path).map(|s| (path, s))
    }
}

// ---------------------------------------------------------------- reading

fn f(v: Option<&Value>) -> Option<f32> {
    v.and_then(|v| v.as_f32())
}

fn vec4(v: &Value) -> Option<[f32; 4]> {
    match v {
        Value::Vec(x) => {
            let mut o = [0.0; 4];
            for (i, c) in x.iter().take(4).enumerate() {
                o[i] = *c;
            }
            Some(o)
        }
        Value::Color(c) => Some(c.map(|b| b as f32 / 255.0)),
        v => v.as_f32().map(|x| [x, 0.0, 0.0, 0.0]),
    }
}

/// A Value*/IntegratedValue* struct; `default` when the field is absent.
fn val(v: Option<&Value>, default: [f32; 4]) -> Val {
    let Some(v) = v else { return Val::constant(default) };
    let mut out = Val { c: v.field("constantValue").and_then(vec4).unwrap_or([0.0; 4]), ..Default::default() };
    if let Some(d) = v.field("dynamics") {
        out.times = d.field("times").map(|l| l.list().iter().filter_map(|x| x.as_f32()).collect()).unwrap_or_default();
        out.vals = d.field("values").map(|l| l.list().iter().filter_map(vec4).collect()).unwrap_or_default();
        for t in d.field("probabilityTables").map(|l| l.list()).unwrap_or(&[]) {
            let kt: Vec<f32> = t.field("keyTimes").map(|l| l.list().iter().filter_map(|x| x.as_f32()).collect()).unwrap_or_default();
            let kv: Vec<f32> = t.field("keyValues").map(|l| l.list().iter().filter_map(|x| x.as_f32()).collect()).unwrap_or_default();
            out.rand.push((kt, kv));
        }
        if v.field("constantValue").is_none() && out.vals.len() == 1 {
            out.c = out.vals[0];
        }
    }
    out
}

fn flag(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(x) => x.as_f32().map(|f| f != 0.0).unwrap_or(false),
        None => false,
    }
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn class_is(v: &Value, name: &str) -> bool {
    matches!(v, Value::Struct(c, _) if *c == crate::bin::fnv1a(name))
}

fn emitter(e: &Value) -> Option<Emitter> {
    if flag(e.field("disabled")) {
        return None;
    }
    let prim = match e.field("primitive") {
        None => Prim::Billboard,
        Some(p) if class_is(p, "VfxPrimitiveArbitraryQuad") => Prim::Quad,
        Some(p) if class_is(p, "VfxPrimitiveRay") => Prim::Ray,
        Some(p) if class_is(p, "VfxPrimitiveBeam") => {
            let b = p.field("mBeam");
            Prim::Beam {
                tiling: val(b.and_then(|b| b.field("mBirthTilingSize")), [0.0; 4]).c[1],
                segments: b.and_then(|b| f(b.field("mSegments"))).unwrap_or(0.0) as u32,
            }
        }
        Some(p) if class_is(p, "VfxPrimitiveMesh") => {
            let m = p.field("mMesh");
            let simple = str_of(m.and_then(|m| m.field("mSimpleMeshName")));
            if !simple.is_empty() {
                Prim::Mesh { mesh: simple }
            } else if str_of(m.and_then(|m| m.field("mMeshName"))).to_ascii_lowercase().contains("raven") {
                Prim::Crow
            } else {
                return None;
            }
        }
        Some(p) if class_is(p, "VfxPrimitiveArbitraryTrail") || class_is(p, "VfxPrimitiveCameraTrail") => {
            let t = p.field("mTrail");
            Prim::Trail { tiling: val(t.and_then(|t| t.field("mBirthTilingSize")), [0.0; 4]).c[0] }
        }
        Some(_) => Prim::Attached,
    };
    let shape = match e.field("SpawnShape") {
        Some(s) if class_is(s, "VfxShapeSphere") => Shape::Sphere(f(s.field("radius")).unwrap_or(0.0)),
        Some(s) if class_is(s, "VfxShapeBox") => Shape::Box(s.field("size").and_then(vec4).map(|v| [v[0], v[1], v[2]]).unwrap_or([0.0; 3])),
        Some(s) if class_is(s, "VfxShapeCylinder") => Shape::Cylinder { radius: f(s.field("radius")).unwrap_or(0.0), height: f(s.field("height")).unwrap_or(0.0) },
        Some(s) if class_is(s, "VfxShapeLegacy") => Shape::Legacy {
            offset: val(s.field("emitOffset"), [0.0; 4]),
            angles: s.field("emitRotationAngles").map(|l| l.list().iter().map(|a| val(Some(a), [0.0; 4])).collect()).unwrap_or_default(),
            axes: s.field("emitRotationAxes").map(|l| l.list().iter().filter_map(vec4).map(|v| [v[0], v[1], v[2]]).collect()).unwrap_or_default(),
        },
        _ => Shape::Point,
    };
    let mult = e.field("textureMult").map(|m| Mult {
        texture: str_of(m.field("textureMult")),
        tex_div: m.field("texDivMult").and_then(vec4).map(|v| [v[0].max(1.0), v[1].max(1.0)]).unwrap_or([1.0, 1.0]),
        scroll: val(m.field("birthUvScrollRateMult"), [0.0; 4]),
        scroll_integrated: val(m.field("particleIntegratedUvScrollMult"), [0.0; 4]),
        uv_scale: val(m.field("uvScaleMult"), [1.0, 1.0, 0.0, 0.0]),
        wrap: f(m.field("texAddressModeMult")) == Some(2.0),
    });
    let erosion = e.field("alphaErosionDefinition").map(|a| Erosion {
        map: str_of(a.field("erosionMapName")),
        drive: val(a.field("erosionDriveCurve"), [0.0; 4]),
        feather_in: f(a.field("erosionFeatherIn")).unwrap_or(0.1),
        feather_out: f(a.field("erosionFeatherOut")).unwrap_or(0.1),
    });
    let mut children = vec![];
    let mut child_on_death = false;
    if let Some(c) = e.field("childParticleSetDefinition") {
        child_on_death = flag(c.field("childEmitOnDeath"));
        for id in c.field("childrenIdentifiers").map(|l| l.list()).unwrap_or(&[]) {
            match (id.field("effectKey"), id.field("effect")) {
                (Some(Value::Hash(h)), _) => children.push(Child::Key(*h)),
                (_, Some(Value::ObjLink(h))) => children.push(Child::System(*h)),
                _ => {}
            }
        }
    }
    let tex_div = e.field("texDiv").and_then(vec4).map(|v| [v[0].max(1.0), v[1].max(1.0)]).unwrap_or([1.0, 1.0]);
    Some(Emitter {
        name: str_of(e.field("emitterName")),
        rate: val(e.field("rate"), [0.0; 4]),
        particle_lifetime: val(e.field("particleLifetime"), [0.0; 4]),
        lifetime: f(e.field("lifetime")),
        first_emission: f(e.field("timeBeforeFirstEmission")).unwrap_or(0.0),
        single: flag(e.field("isSingleParticle")),
        linger: f(e.field("particleLinger")).unwrap_or(0.0),
        birth_color: val(e.field("birthColor"), [1.0; 4]),
        color: val(e.field("Color"), [1.0; 4]),
        birth_scale: val(e.field("birthScale0"), [1.0, 1.0, 1.0, 0.0]),
        scale: val(e.field("scale0"), [1.0, 1.0, 1.0, 0.0]),
        uniform_scale: flag(e.field("isUniformScale")),
        birth_rotation: val(e.field("birthRotation0"), [0.0; 4]),
        birth_rot_velocity: val(e.field("birthRotationalVelocity0"), [0.0; 4]),
        birth_velocity: val(e.field("birthVelocity"), [0.0; 4]),
        birth_acceleration: val(e.field("birthAcceleration"), [0.0; 4]),
        world_acceleration: val(e.field("worldAcceleration"), [0.0; 4]),
        birth_drag: val(e.field("birthDrag"), [0.0; 4]),
        birth_orbital: val(e.field("birthOrbitalVelocity"), [0.0; 4]),
        position: val(e.field("EmitterPosition"), [0.0; 4]),
        shape,
        bind_weight: val(e.field("bindWeight"), [0.0; 4]).c[0],
        texture: str_of(e.field("texture")),
        tex_div,
        num_frames: f(e.field("numFrames")).unwrap_or(1.0) as u32,
        frame_rate: f(e.field("frameRate")).unwrap_or(0.0),
        random_frame: flag(e.field("isRandomStartFrame")),
        uv_scroll_birth: val(e.field("birthUvScrollRate"), [0.0; 4]),
        uv_scroll: val(e.field("particleUVScrollRate"), [0.0; 4]),
        uv_scale: val(e.field("uvScale"), [1.0, 1.0, 0.0, 0.0]),
        uv_offset: val(e.field("birthUVOffset"), [0.0; 4]),
        mult,
        erosion,
        blend: f(e.field("blendMode")).unwrap_or(0.0) as u8,
        pass: f(e.field("pass")).unwrap_or(0.0) as i16,
        ground: flag(e.field("isGroundLayer")),
        direction_oriented: flag(e.field("isDirectionOriented")),
        direction_velocity_scale: f(e.field("directionVelocityScale")).unwrap_or(0.0),
        prim,
        children,
        child_on_death,
        distortion: e.field("distortionDefinition").is_some(),
        wrap: f(e.field("texAddressModeBase")) == Some(2.0),
    })
}

/// One `VfxSystemDefinitionData` object.
pub fn system(o: &crate::bin::Object, name: &str) -> System {
    let v = o.as_value();
    let mut emitters = vec![];
    for list in ["complexEmitterDefinitionData", "simpleEmitterDefinitionData"] {
        for e in v.field(list).map(|l| l.list()).unwrap_or(&[]) {
            if let Some(em) = emitter(e) {
                emitters.push(em);
            }
        }
    }
    System { name: name.to_string(), emitters }
}

/// Every particle system object in a set of bins, by object path hash.
pub fn systems_in<'a>(bins: &'a [Bin]) -> impl Iterator<Item = &'a crate::bin::Object> {
    let class = crate::bin::fnv1a("VfxSystemDefinitionData");
    bins.iter().flat_map(|b| b.objects.iter()).filter(move |o| o.class == class)
}

/// League's .scb static mesh (r3d2Mesh) as a triangle soup.
pub fn read_scb(b: &[u8]) -> anyhow::Result<StaticMesh> {
    use anyhow::bail;
    if b.len() < 8 || &b[0..8] != b"r3d2Mesh" {
        bail!("not an scb");
    }
    let rd = |o: usize| -> anyhow::Result<u32> { Ok(u32::from_le_bytes(b.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("scb truncated"))?.try_into()?)) };
    let rf = |o: usize| -> anyhow::Result<f32> { Ok(f32::from_bits(rd(o)?)) };
    let major = u16::from_le_bytes([b[8], b[9]]);
    let minor = u16::from_le_bytes([b[10], b[11]]);
    let mut o = 12 + 128;
    let nv = rd(o)? as usize;
    let nf = rd(o + 4)? as usize;
    o += 12 + 24; // counts, flags, bounding box
    let colors = if major >= 3 && minor >= 2 {
        let c = rd(o)? == 1;
        o += 4;
        c
    } else {
        false
    };
    let mut pos = Vec::with_capacity(nv);
    for i in 0..nv {
        pos.push([rf(o + i * 12)?, rf(o + i * 12 + 4)?, rf(o + i * 12 + 8)?]);
    }
    o += nv * 12;
    if colors {
        o += nv * 4;
    }
    o += 12; // central point
    let mut verts = Vec::with_capacity(nf * 3);
    for _ in 0..nf {
        let idx = [rd(o)? as usize, rd(o + 4)? as usize, rd(o + 8)? as usize];
        let uo = o + 12 + 64;
        let (u, v) = ([rf(uo)?, rf(uo + 4)?, rf(uo + 8)?], [rf(uo + 12)?, rf(uo + 16)?, rf(uo + 20)?]);
        for k in 0..3 {
            verts.push((*pos.get(idx[k]).unwrap_or(&[0.0; 3]), [u[k], v[k]]));
        }
        o = uo + 24;
    }
    Ok(StaticMesh { verts })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn curve_and_random() {
        let v = Val { c: [9.0; 4], times: vec![0.0, 1.0], vals: vec![[0.0; 4], [2.0; 4]], rand: vec![(vec![0.0, 1.0], vec![1.0, 3.0])] };
        assert_eq!(v.at(0.5)[1], 1.0);
        assert_eq!(v.at(2.0)[0], 2.0);
        assert_eq!(v.sample(1.0, || 0.5)[0], 4.0); // 2 x (1..3 at 0.5 = 2)
        assert_eq!(Val::constant([3.0; 4]).at(0.7)[2], 3.0);
    }
}
