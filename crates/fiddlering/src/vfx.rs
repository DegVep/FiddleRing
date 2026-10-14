//! League particle systems played in Elden Ring: each effect instance runs its emitters (rates, single particles,
//! lifetimes), simulates their particles (velocity, drag, acceleration, orbit, rotation) and turns them into
//! triangles for the renderer: billboards, oriented and ground quads, rays, beams, trails and .scb meshes, with
//! their colour/scale curves, atlas frames, uv scrolling, multiply texture and alpha erosion. League units are
//! scaled like Fiddlesticks' model; where an effect sits each frame comes from the game (`Follow`).

use crate::scene::{ModelDraw, VfxMat, VfxVert};
use lolread::sheet::tuning;
use lolread::vfx::{Child, Emitter, Prim, Shape, VfxLib};

pub type V3 = [f32; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn len(a: V3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn norm(a: V3) -> V3 {
    mul(a, 1.0 / len(a).max(1e-6))
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn v3(v: [f32; 4]) -> V3 {
    [v[0], v[1], v[2]]
}

/// Rotates `p` by Euler angles in degrees (X, then Y, then Z).
fn euler(p: V3, deg: V3) -> V3 {
    let (sx, cx) = deg[0].to_radians().sin_cos();
    let (sy, cy) = deg[1].to_radians().sin_cos();
    let (sz, cz) = deg[2].to_radians().sin_cos();
    let p = [p[0], p[1] * cx - p[2] * sx, p[1] * sx + p[2] * cx];
    let p = [p[0] * cy + p[2] * sy, p[1], -p[0] * sy + p[2] * cy];
    [p[0] * cz - p[1] * sz, p[0] * sz + p[1] * cz, p[2]]
}

/// Rotates `p` about unit `axis` by `deg` degrees (Rodrigues).
fn about(p: V3, axis: V3, deg: f32) -> V3 {
    let a = norm(axis);
    let (s, c) = deg.to_radians().sin_cos();
    let d = a[0] * p[0] + a[1] * p[1] + a[2] * p[2];
    add(add(mul(p, c), mul(cross(a, p), s)), mul(a, d * (1.0 - c)))
}

/// Where an effect sits: the game resolves it every frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Follow {
    /// stays where it was spawned
    Fixed,
    /// Fiddlesticks' feet, turned with him
    Caster,
    /// one of his bones (index into the sheet's VFX_EVENTS attach names)
    Bone(&'static str),
    /// an Elden Ring character (pointer)
    Chr(usize),
    /// a missile of the game, by id
    Missile(usize),
    /// from his chest to a character
    Beam(usize),
    /// flies from where it spawned to his chest
    ToCaster,
}

/// An effect's place this frame: origin, flat forward (League +Z) and, for beams, the far end.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub pos: V3,
    pub fwd: V3,
    pub target: Option<V3>,
}

impl Frame {
    fn right(&self) -> V3 {
        [self.fwd[2], 0.0, -self.fwd[0]]
    }
    /// League local (units) -> world offset (metres)
    fn to_world(&self, p: V3, unit: f32) -> V3 {
        let r = self.right();
        add(add(mul(r, p[0] * unit), [0.0, p[1] * unit, 0.0]), mul(self.fwd, p[2] * unit))
    }
}

struct Particle {
    /// League units: local to the effect when bound, else world (scaled to metres at birth)
    pos: V3,
    vel: V3,
    bound: bool,
    age: f32,
    life: f32,
    color: [f32; 4],
    scale: V3,
    rot: V3,
    rotv: V3,
    drag: V3,
    accel: V3,
    orbit: V3,
    frame0: f32,
    uv0: [f32; 2],
    uv_scroll: [f32; 2],
    trail: Vec<V3>,
    children: Vec<u64>,
}

struct EmitterState {
    acc: f32,
    single_done: bool,
    parts: Vec<Particle>,
}

/// What emitting and simulating need of an instance (copied so its emitters can be borrowed mutably).
#[derive(Clone, Copy)]
struct Ctx {
    id: u64,
    frame: Frame,
    unit: f32,
    age: f32,
    max_age: f32,
    stopped: bool,
}

struct Instance {
    id: u64,
    sys: u32,
    follow: Follow,
    frame: Frame,
    /// metres per League unit
    unit: f32,
    age: f32,
    max_age: f32,
    stopped: bool,
    /// destroyed (its missile landed, its target went, the game stopped it): particles got their linger
    killed: bool,
    ems: Vec<EmitterState>,
    /// fly: start point and progress
    fly_from: V3,
}

pub struct Vfx {
    insts: Vec<Instance>,
    next: u64,
    seed: u64,
}

impl Default for Vfx {
    fn default() -> Self {
        Self::new()
    }
}

const MAX_PER_EMITTER: usize = 256;

impl Vfx {
    pub fn new() -> Self {
        Vfx { insts: vec![], next: 1, seed: 0x2545_f491_4f6c_dd1d ^ std::process::id() as u64 }
    }

    fn rnd(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed % 1_000_000) as f32 / 1_000_000.0
    }

    /// Starts system `sys` of `lib` at `frame`. `max_age` ends it even if its emitters run forever.
    pub fn spawn(&mut self, lib: &VfxLib, sys: u32, follow: Follow, frame: Frame, unit: f32, max_age: f32) -> Option<u64> {
        let s = lib.systems.get(&sys)?;
        // the first time a system plays, say what it is made of (to tune blends and spot odd layers)
        {
            use std::sync::Mutex;
            static SEEN: Mutex<Vec<u32>> = Mutex::new(vec![]);
            let mut seen = SEEN.lock().unwrap();
            if !seen.contains(&sys) {
                seen.push(sys);
                let ems: Vec<String> = s
                    .emitters
                    .iter()
                    .map(|e| format!("{}[{:?} b{} {}{}]", e.name, e.prim, e.blend, e.texture.rsplit('/').next().unwrap_or(""), if e.mult.is_some() { " x" } else { "" }))
                    .collect();
                crate::log!("VFX {sys:08x}: {}", ems.join(" "));
            }
        }
        let id = self.next;
        self.next += 1;
        self.insts.push(Instance {
            id,
            sys,
            follow,
            frame,
            unit,
            age: 0.0,
            max_age,
            stopped: false,
            killed: false,
            ems: s.emitters.iter().map(|_| EmitterState { acc: 0.0, single_done: false, parts: vec![] }).collect(),
            fly_from: frame.pos,
        });
        Some(id)
    }

    /// Stops emitting (its particles finish their lives).
    pub fn stop(&mut self, id: u64) {
        if let Some(i) = self.insts.iter_mut().find(|i| i.id == id) {
            i.stopped = true;
        }
    }

    pub fn alive(&self, id: u64) -> bool {
        self.insts.iter().any(|i| i.id == id)
    }

    pub fn clear(&mut self) {
        self.insts.clear();
    }

    /// Advances every effect; `place` says where a followed thing is now (None: it is gone, the effect stops).
    pub fn update(&mut self, lib: &VfxLib, dt: f32, place: &dyn Fn(Follow) -> Option<Frame>) {
        let mut spawns: Vec<(u32, Frame, f32, u64, usize, usize)> = vec![];
        let mut moves: Vec<(u64, V3)> = vec![];
        let seed = &mut self.seed;
        for inst in self.insts.iter_mut() {
            let Some(sys) = lib.systems.get(&inst.sys) else {
                inst.max_age = 0.0;
                continue;
            };
            inst.age += dt;
            match inst.follow {
                Follow::Fixed => {}
                Follow::ToCaster => {
                    if let Some(c) = place(Follow::Caster) {
                        let to = add(c.pos, [0.0, 1.0, 0.0]);
                        let f = (inst.age / tuning::VFX_FLY_TIME).min(1.0);
                        inst.frame.pos = add(inst.fly_from, mul(sub(to, inst.fly_from), f * f));
                        if f >= 1.0 {
                            inst.stopped = true;
                        }
                    }
                }
                f => match place(f) {
                    Some(fr) => inst.frame = fr,
                    None => inst.stopped = true,
                },
            }
            if inst.age > inst.max_age {
                inst.stopped = true;
            }
            if inst.stopped && !inst.killed {
                // like League destroying an effect: each particle lives on only for its emitter's linger
                inst.killed = true;
                for (em, st) in sys.emitters.iter().zip(inst.ems.iter_mut()) {
                    for p in st.parts.iter_mut() {
                        p.life = p.life.min(p.age + em.linger.max(0.0)).max(p.age + 0.01);
                    }
                }
            }
            let cx = Ctx { id: inst.id, frame: inst.frame, unit: inst.unit, age: inst.age, max_age: inst.max_age, stopped: inst.stopped };
            for (ei, (em, st)) in sys.emitters.iter().zip(inst.ems.iter_mut()).enumerate() {
                emit(em, st, &cx, dt, seed);
                // particles
                for p in st.parts.iter_mut() {
                    p.age += dt;
                    let t = (p.age / p.life).clamp(0.0, 1.0);
                    let wa = v3(em.world_acceleration.at(t));
                    let acc = add(p.accel, wa);
                    let k = if p.bound { 1.0 } else { cx.unit };
                    p.vel = add(p.vel, mul(acc, dt * k));
                    for c in 0..3 {
                        p.vel[c] *= (1.0 - p.drag[c] * dt).max(0.0);
                    }
                    if p.orbit != [0.0; 3] {
                        let w = mul(p.orbit, dt);
                        let ang = len(w).to_degrees();
                        if ang > 0.0 {
                            let center = if p.bound { [0.0; 3] } else { cx.frame.pos };
                            p.pos = add(center, about(sub(p.pos, center), w, ang));
                        }
                    }
                    p.pos = add(p.pos, mul(p.vel, dt));
                    p.rot = add(p.rot, mul(p.rotv, dt));
                    if matches!(em.prim, Prim::Trail { .. }) {
                        let w = if p.bound { add(cx.frame.pos, cx.frame.to_world(p.pos, cx.unit)) } else { p.pos };
                        p.trail.push(w);
                        if p.trail.len() > 24 {
                            p.trail.remove(0);
                        }
                    }
                    if !p.children.is_empty() {
                        let w = if p.bound { add(cx.frame.pos, cx.frame.to_world(p.pos, cx.unit)) } else { p.pos };
                        for c in &p.children {
                            moves.push((*c, w));
                        }
                    }
                }
                // children of particles born this frame
                for (pi, p) in st.parts.iter_mut().enumerate() {
                    if p.age <= dt && !em.children.is_empty() && !em.child_on_death && p.children.is_empty() {
                        let w = if p.bound { add(cx.frame.pos, cx.frame.to_world(p.pos, cx.unit)) } else { p.pos };
                        for c in &em.children {
                            let target = match c {
                                Child::Key(k) => lib.keys.get(k).copied(),
                                Child::System(s) => Some(*s),
                            };
                            if let Some(s) = target {
                                spawns.push((s, Frame { pos: w, ..cx.frame }, cx.unit, cx.id, ei, pi));
                            }
                        }
                    }
                }
                let dead_children: Vec<u64> = st.parts.iter().filter(|p| p.age >= p.life).flat_map(|p| p.children.clone()).collect();
                for c in dead_children {
                    moves.push((c, [f32::NAN; 3]));
                }
                st.parts.retain(|p| p.age < p.life);
            }
        }
        // child effects: born at their particle, then carried by it
        for (s, frame, unit, parent, ei, pi) in spawns {
            if let Some(id) = self.spawn(lib, s, Follow::Fixed, frame, unit, 8.0) {
                if let Some(p) = self.insts.iter_mut().find(|i| i.id == parent).and_then(|i| i.ems.get_mut(ei)).and_then(|e| e.parts.get_mut(pi)) {
                    p.children.push(id);
                }
            }
        }
        for (id, pos) in moves {
            if let Some(i) = self.insts.iter_mut().find(|i| i.id == id) {
                if pos[0].is_nan() {
                    i.stopped = true;
                } else {
                    i.frame.pos = pos;
                }
            }
        }
        self.insts.retain(|i| !(i.stopped && i.ems.iter().all(|e| e.parts.is_empty())) && i.age < i.max_age + 6.0);
    }

    /// Triangles (and crows) of every live particle, ordered by League's pass.
    pub fn draw(&self, lib: &VfxLib, cam_pos: V3, cam_right: V3, cam_up: V3, recolor: &dyn Fn([f32; 4]) -> [f32; 4], out: &mut Vec<(VfxMat, Vec<VfxVert>)>, crows: &mut Vec<(V3, V3, f32, f32)>) {
        let mut layers: Vec<(i16, usize, VfxMat, Vec<VfxVert>)> = vec![];
        let mut order = 0usize;
        for inst in &self.insts {
            let Some(sys) = lib.systems.get(&inst.sys) else { continue };
            for (em, st) in sys.emitters.iter().zip(inst.ems.iter()) {
                order += 1;
                if st.parts.is_empty() {
                    continue;
                }
                let mat = VfxMat {
                    tex: key_of(lib, &em.texture),
                    mult: em.mult.as_ref().map(|m| key_of(lib, &m.texture)).unwrap_or("white"),
                    erode: em.erosion.as_ref().map(|e| key_of(lib, &e.map)).unwrap_or("white"),
                    blend: blend_of(em.blend),
                    // beams and trails tile along their length
                    wrap: (em.wrap || matches!(em.prim, Prim::Beam { .. } | Prim::Trail { .. })) as u8 | (em.mult.as_ref().map(|m| m.wrap).unwrap_or(false) as u8) << 1,
                };
                if em.distortion || (mat.tex == "white" && !matches!(em.prim, Prim::Crow)) {
                    continue; // a refraction pass, or its texture is not in the install
                }
                let mut tris: Vec<VfxVert> = vec![];
                for p in &st.parts {
                    particle(lib, em, inst, p, cam_pos, cam_right, cam_up, recolor, &mut tris, crows);
                }
                if !tris.is_empty() {
                    layers.push((em.pass, order, mat, tris));
                }
            }
        }
        layers.sort_by_key(|l| (l.0, l.1));
        out.extend(layers.into_iter().map(|l| (l.2, l.3)));
    }
}

/// League blend mode -> renderer blend (tuning sheet: vfx_blend_<n>).
fn blend_of(b: u8) -> u8 {
    (match b {
        1 => tuning::VFX_BLEND_1,
        2 => tuning::VFX_BLEND_2,
        3 => tuning::VFX_BLEND_3,
        4 => tuning::VFX_BLEND_4,
        5 => tuning::VFX_BLEND_5,
        _ => tuning::VFX_BLEND_0,
    }) as u8
}

/// The renderer's key for a texture path (static: textures load once per skin).
fn key_of(lib: &VfxLib, path: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static KEYS: Mutex<Option<HashMap<String, &'static str>>> = Mutex::new(None);
    if path.is_empty() || !lib.textures.contains_key(path) {
        return "white";
    }
    let mut g = KEYS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if let Some(k) = m.get(path) {
        return k;
    }
    let k: &'static str = Box::leak(path.to_string().into_boxed_str());
    m.insert(path.to_string(), k);
    k
}

fn emit(em: &Emitter, st: &mut EmitterState, inst: &Ctx, dt: f32, seed: &mut u64) {
    let t_e = inst.age - em.first_emission;
    if t_e < 0.0 || st.parts.len() >= MAX_PER_EMITTER {
        return;
    }
    let span = em.lifetime.unwrap_or(0.0);
    let fr = if span > 0.0 { (t_e / span).clamp(0.0, 1.0) } else { 0.0 };
    let mut n = 0;
    if em.single {
        if !st.single_done {
            st.single_done = true;
            n = 1;
        }
    } else {
        let active = !inst.stopped && (span <= 0.0 || t_e < span);
        if active {
            st.acc += em.rate.at(fr)[0].max(0.0) * dt;
            n = st.acc.floor() as usize;
            st.acc -= n as f32;
        }
    }
    for _ in 0..n.min(MAX_PER_EMITTER - st.parts.len()) {
        let mut r = || Vfx::rnd(seed);
        let mut pos = v3(em.position.sample(fr, &mut r));
        let mut vel = v3(em.birth_velocity.sample(fr, &mut r));
        match &em.shape {
            Shape::Point => {}
            Shape::Sphere(rad) => {
                let d = norm([r() * 2.0 - 1.0, r() * 2.0 - 1.0, r() * 2.0 - 1.0]);
                pos = add(pos, mul(d, rad * r().cbrt()));
            }
            Shape::Box(s) => pos = add(pos, [(r() - 0.5) * s[0], (r() - 0.5) * s[1], (r() - 0.5) * s[2]]),
            Shape::Cylinder { radius, height } => {
                let a = r() * std::f32::consts::TAU;
                let rr = radius * r().sqrt();
                pos = add(pos, [a.cos() * rr, r() * height, a.sin() * rr]);
            }
            Shape::Legacy { offset, angles, axes } => {
                let mut o = v3(offset.sample(fr, &mut r));
                for (ang, axis) in angles.iter().zip(axes.iter()) {
                    let deg = ang.sample(fr, &mut r)[0];
                    o = about(o, *axis, deg);
                    vel = about(vel, *axis, deg);
                }
                pos = add(pos, o);
            }
        }
        let mut scale = v3(em.birth_scale.sample(fr, &mut r));
        if em.uniform_scale {
            scale = [scale[0]; 3];
        }
        let life = em.particle_lifetime.sample(fr, &mut r)[0];
        // particleLinger only matters when an effect is cut short: it never stretches a particle's own life (its curves)
        let life = if life > 0.0 { life } else { span.max(0.5).min(inst.max_age.max(0.5)) };
        let bound = em.bind_weight >= 0.5;
        let (pos, vel) = if bound { (pos, vel) } else { (add(inst.frame.pos, inst.frame.to_world(pos, inst.unit)), inst.frame.to_world(vel, inst.unit)) };
        let frames = em.num_frames.max(1) as f32;
        st.parts.push(Particle {
            pos,
            vel,
            bound,
            age: 0.0,
            life: life.max(0.05),
            color: em.birth_color.sample(fr, &mut r),
            scale,
            rot: v3(em.birth_rotation.sample(fr, &mut r)),
            rotv: v3(em.birth_rot_velocity.sample(fr, &mut r)),
            drag: v3(em.birth_drag.sample(fr, &mut r)),
            accel: v3(em.birth_acceleration.sample(fr, &mut r)),
            orbit: v3(em.birth_orbital.sample(fr, &mut r)),
            frame0: if em.random_frame { (r() * frames).floor() } else { 0.0 },
            uv0: {
                let o = em.uv_offset.sample(fr, &mut r);
                [o[0], o[1]]
            },
            uv_scroll: {
                let s = em.uv_scroll_birth.sample(fr, &mut r);
                [s[0], s[1]]
            },
            trail: vec![],
            children: vec![],
        });
    }
}

fn mulc(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

#[allow(clippy::too_many_arguments)]
fn particle(
    lib: &VfxLib,
    em: &Emitter,
    inst: &Instance,
    p: &Particle,
    cam_pos: V3,
    cam_right: V3,
    cam_up: V3,
    recolor: &dyn Fn([f32; 4]) -> [f32; 4],
    tris: &mut Vec<VfxVert>,
    crows: &mut Vec<(V3, V3, f32, f32)>,
) {
    let t = (p.age / p.life).clamp(0.0, 1.0);
    let mut color = recolor(mulc(p.color, em.color.at(t)));
    color[3] = color[3].clamp(0.0, 1.0);
    if color[3] <= 0.002 {
        return;
    }
    if blend_of(em.blend) == 2 {
        // premultiplied: the vertex colour carries its alpha in its rgb too
        color = [color[0] * color[3], color[1] * color[3], color[2] * color[3], color[3]];
    }
    let sc = v3(em.scale.at(t));
    let s = if em.uniform_scale { [p.scale[0] * sc[0]; 3] } else { [p.scale[0] * sc[0], p.scale[1] * sc[1], p.scale[2] * sc[2]] };
    let u = inst.unit;
    let world = if p.bound { add(inst.frame.pos, inst.frame.to_world(p.pos, u)) } else { p.pos };
    let wvel = if p.bound { inst.frame.to_world(p.vel, u) } else { p.vel };

    // texture frame (atlas cells), offset and scroll
    let (cols, rows) = (em.tex_div[0].max(1.0), em.tex_div[1].max(1.0));
    let frames = (em.num_frames.max(1) as f32).min(cols * rows);
    let fi = if frames > 1.0 {
        let f = if em.frame_rate > 0.0 { p.frame0 + p.age * em.frame_rate } else if em.random_frame { p.frame0 } else { t * frames };
        (f.floor() % frames).max(0.0)
    } else {
        0.0
    };
    let cell = [(fi % cols) / cols, (fi / cols).floor() / rows];
    let us = em.uv_scale.at(t);
    let scroll = em.uv_scroll.at(t);
    let off = [p.uv0[0] + (p.uv_scroll[0] + scroll[0]) * p.age, p.uv0[1] + (p.uv_scroll[1] + scroll[1]) * p.age];
    let off = if em.scroll_clamp { off.map(|o| o.clamp(-1.0, 1.0)) } else { off };
    let uv_at = |a: f32, b: f32| -> [f32; 2] { [cell[0] + (a * us[0].max(0.0001) + off[0]) / cols, cell[1] + (b * us[1].max(0.0001) + off[1]) / rows] };
    let uv2_at = |a: f32, b: f32| -> [f32; 2] {
        match &em.mult {
            Some(m) => {
                let ms = m.uv_scale.at(t);
                let sc = m.scroll.at(t);
                let si = m.scroll_integrated.at(t);
                let mut off = [m.offset[0] + (sc[0] + si[0]) * p.age, m.offset[1] + (sc[1] + si[1]) * p.age];
                if m.scroll_clamp {
                    // the mask slides across once instead of repeating
                    off = off.map(|o| o.clamp(-1.0, 1.0));
                }
                [(a * ms[0].max(0.0001)) / m.tex_div[0] + off[0], (b * ms[1].max(0.0001)) / m.tex_div[1] + off[1]]
            }
            None => [0.0, 0.0],
        }
    };
    let erode = match &em.erosion {
        Some(e) if lib.textures.contains_key(&e.map) => [e.drive.at(t)[0], e.feather_in.max(0.02)],
        _ => [0.0, 0.0],
    };
    let vert = |pos: V3, a: f32, b: f32| VfxVert { pos, uv: uv_at(a, b), uv2: uv2_at(a, b), color, erode };
    let quad = |c: [V3; 4], tris: &mut Vec<VfxVert>| {
        let uvs = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for k in [0, 1, 2, 0, 2, 3] {
            tris.push(vert(c[k], uvs[k].0, uvs[k].1));
        }
    };
    let roll = if p.rot[2] != 0.0 { p.rot[2] } else { p.rot[0] };
    let (hx, hy) = (s[0] * u * 0.5, s[1] * u * 0.5);

    match &em.prim {
        Prim::Attached => {}
        Prim::Billboard => {
            let (sn, cs) = roll.to_radians().sin_cos();
            let r = add(mul(cam_right, cs * hx), mul(cam_up, sn * hx));
            let up = add(mul(cam_up, cs * hy), mul(cam_right, -sn * hy));
            quad([add(sub(world, r), up), add(add(world, r), up), sub(add(world, r), up), sub(sub(world, r), up)], tris);
        }
        Prim::Quad => {
            // a plane in League's local XY (or XZ on the ground), turned by its rotation, then into the effect's frame
            let prot = p.rot;
            let local = |x: f32, y: f32| -> V3 {
                let q = if em.ground { [x, 0.0, y] } else { [x, y, 0.0] };
                euler(q, if em.ground { [0.0, roll, 0.0] } else { prot.rot_or(roll) })
            };
            let lift = if em.ground { [0.0, 0.03, 0.0] } else { [0.0; 3] };
            let w = |x: f32, y: f32| add(add(world, lift), inst.frame.to_world(local(x, y), 1.0));
            quad([w(-hx, hy), w(hx, hy), w(hx, -hy), w(-hx, -hy)], tris);
        }
        Prim::Ray => {
            let speed = len(wvel);
            let axis = if speed > 1e-4 { mul(wvel, 1.0 / speed) } else { cam_up };
            let side = mul(norm(cross(axis, norm(sub(cam_pos, world)))), hx);
            let half = mul(axis, hy * (1.0 + em.direction_velocity_scale * speed / u.max(1e-4) * 0.01));
            // a streak's texture runs along V: V follows the velocity, U goes across
            let c = [add(add(world, half), side), add(sub(world, half), side), sub(sub(world, half), side), sub(add(world, half), side)];
            let uvs = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
            for k in [0, 1, 2, 0, 2, 3] {
                tris.push(vert(c[k], uvs[k].0, uvs[k].1));
            }
        }
        Prim::Beam { tiling, .. } => {
            let Some(end) = inst.frame.target else { return };
            let a = inst.frame.pos;
            let d = sub(end, a);
            let length = len(d);
            let side = mul(norm(cross(norm(d), norm(sub(cam_pos, a)))), hx);
            let tiles = if *tiling > 0.0 { length / (tiling * u) } else { 1.0 };
            let c = [add(a, side), add(end, side), sub(end, side), sub(a, side)];
            let uvs = [(0.0, 0.0), (tiles, 0.0), (tiles, 1.0), (0.0, 1.0)];
            for k in [0, 1, 2, 0, 2, 3] {
                tris.push(vert(c[k], uvs[k].0, uvs[k].1));
            }
        }
        Prim::Trail { tiling } => {
            if p.trail.len() < 2 {
                return;
            }
            let n = p.trail.len();
            let mut along = 0.0;
            for i in 0..n - 1 {
                let (a, b) = (p.trail[i], p.trail[i + 1]);
                let seg = len(sub(b, a));
                let dir = norm(sub(b, a));
                let side = mul(norm(cross(dir, norm(sub(cam_pos, a)))), hx);
                let (u0, u1) = if *tiling > 0.0 { (along / (tiling * u), (along + seg) / (tiling * u)) } else { (i as f32 / (n - 1) as f32, (i + 1) as f32 / (n - 1) as f32) };
                along += seg;
                let c = [add(a, side), add(b, side), sub(b, side), sub(a, side)];
                let uvs = [(u0, 0.0), (u1, 0.0), (u1, 1.0), (u0, 1.0)];
                for k in [0, 1, 2, 0, 2, 3] {
                    tris.push(vert(c[k], uvs[k].0, uvs[k].1));
                }
            }
        }
        Prim::Mesh { mesh } => {
            let Some(m) = lib.meshes.get(mesh) else { return };
            for (pos, uv) in &m.verts {
                let local = euler([pos[0] * s[0], pos[1] * s[1], pos[2] * s[2]], p.rot);
                let w = add(world, inst.frame.to_world(local, u));
                tris.push(vert(w, uv[0], uv[1]));
            }
        }
        Prim::Crow => {
            let fwd = if len(wvel) > 1e-4 { norm(wvel) } else { inst.frame.fwd };
            crows.push((world, fwd, s[0], color[3]));
        }
    }
}

trait RotOr {
    fn rot_or(&self, roll: f32) -> V3;
}

impl RotOr for V3 {
    /// a quad's own Euler rotation, or a roll about its normal when only that is given
    fn rot_or(&self, roll: f32) -> V3 {
        if self[0] != 0.0 || self[1] != 0.0 { *self } else { [0.0, 0.0, roll] }
    }
}

/// League systems keyed by the vfx_events sheet for a skin.
pub fn event_system(lib: &VfxLib, key: &str) -> Option<u32> {
    let h = match key.strip_prefix('#') {
        Some(h) => u32::from_str_radix(h, 16).ok()?,
        None => lolread::bin::fnv1a(key),
    };
    lib.by_key(h).map(|(p, _)| p)
}

pub fn crow_draw(raven: &(lolread::cache::Model, lolread::cache::Clip), pos: V3, fwd: V3, size: f32, clock: f32) -> ModelDraw {
    let k = tuning::MODEL_SCALE * size;
    let f = norm(fwd);
    let r = norm(cross([0.0, 1.0, 0.0], f));
    let r = if len(r) < 0.01 { [1.0, 0.0, 0.0] } else { r };
    let u = cross(f, r);
    let mx = tuning::MODEL_MIRROR_X;
    let world = [r[0] * k * mx, r[1] * k * mx, r[2] * k * mx, 0., u[0] * k, u[1] * k, u[2] * k, 0., f[0] * k, f[1] * k, f[2] * k, 0., pos[0], pos[1], pos[2], 1.];
    let locals = lolread::pose::sample_locals(&raven.0, Some(&raven.1), clock, true);
    ModelDraw { model: 1, world, palette: lolread::pose::palette(&raven.0, &locals), visible: vec![true; raven.0.submeshes.len()] }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotations() {
        let p = euler([1.0, 0.0, 0.0], [0.0, 90.0, 0.0]);
        assert!((p[2] + 1.0).abs() < 1e-5 && p[0].abs() < 1e-5, "{p:?}");
        let q = about([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], 90.0);
        assert!((q[2] + 1.0).abs() < 1e-5, "{q:?}");
        let f = Frame { pos: [0.0; 3], fwd: [0.0, 0.0, 1.0], target: None };
        assert_eq!(f.to_world([0.0, 0.0, 100.0], 0.01), [0.0, 0.0, 1.0]);
    }
}
