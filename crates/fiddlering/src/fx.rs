//! Particle system driven by the effects sheet: every row is an emitter (burst or rate), particles move freely,
//! orbit Fiddlesticks or fly home to him, and become billboards, velocity-stretched sparks, ground decals, beams
//! or 3D crows.

use crate::scene::{Sprite, quad_uv};
use lolread::sheet::{self, Effect};
use std::collections::HashMap;

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
    mul(a, 1.0 / len(a).max(1e-5))
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)
}

struct Particle {
    e: &'static Effect,
    pos: V3,
    vel: V3,
    age: f32,
    life: f32,
    rot: f32,
    rotv: f32,
    cell: [f32; 2],
    /// orbit: angle, radius and height above the caster's feet
    ang: f32,
    orad: f32,
    h: f32,
    /// ground decals: direction they face; models: animation clock
    dir: V3,
    anim: f32,
}

/// A crow to draw this frame (3D raven model).
pub struct Crow {
    pub pos: V3,
    pub forward: V3,
    pub scale: f32,
    pub anim: f32,
    pub alpha: f32,
}

/// What a frame needs from the camera to face sprites at it.
pub struct View {
    pub right: V3,
    pub up: V3,
    pub pos: V3,
}

pub struct Fx {
    parts: Vec<Particle>,
    acc: HashMap<(usize, usize), f32>,
    seed: u64,
}

const MAX_PARTICLES: usize = 2500;

impl Fx {
    pub fn new() -> Self {
        Fx { parts: vec![], acc: HashMap::new(), seed: 0x9e37_79b9_7f4a_7c15 ^ std::process::id() as u64 }
    }

    fn rnd(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed % 100_000) as f32 / 100_000.0
    }

    fn rdir(&mut self) -> V3 {
        loop {
            let v = [self.rnd() * 2.0 - 1.0, self.rnd() * 2.0 - 1.0, self.rnd() * 2.0 - 1.0];
            let l = len(v);
            if l > 0.05 && l <= 1.0 {
                return mul(v, 1.0 / l);
            }
        }
    }

    fn rows(effect: &str, trigger: &str) -> impl Iterator<Item = (usize, &'static Effect)> {
        let (effect, trigger) = (effect.to_string(), trigger.to_string());
        sheet::EFFECTS.iter().enumerate().filter(move |(_, e)| e.effect == effect && e.trigger == trigger)
    }

    fn spawn(&mut self, e: &'static Effect, at: V3, dir: V3, ability_radius: f32) {
        if self.parts.len() >= MAX_PARTICLES {
            return;
        }
        let radius = if e.radius < 0.0 { ability_radius } else { e.radius };
        let (ang, orad, h) = (self.rnd() * std::f32::consts::TAU, radius * (0.35 + 0.65 * self.rnd()), 0.3 + self.rnd() * 1.6);
        let mut pos = at;
        if e.motion != "orbit" && radius > 0.0 && e.render != "ground" {
            let (a, r) = (self.rnd() * std::f32::consts::TAU, radius * self.rnd().sqrt());
            pos = add(pos, [a.cos() * r, 0.0, a.sin() * r]);
        }
        let rd = self.rdir();
        let d = norm(add(mul(norm(dir), 1.0 - e.spread), mul(rd, e.spread.max(0.0001))));
        let speed = e.speed * (0.6 + 0.4 * self.rnd());
        let vel = add(mul(d, speed), [0.0, e.up, 0.0]);
        let cell = if e.frames > 1 {
            let n = e.frames as f32;
            [(self.rnd() * n).floor() / n, (self.rnd() * n).floor() / n]
        } else {
            [0.0, 0.0]
        };
        let life = e.life.max(0.05) * (0.8 + 0.4 * self.rnd());
        let rot = self.rnd() * std::f32::consts::TAU;
        let rotv = (self.rnd() * 2.0 - 1.0) * e.spin;
        let anim = self.rnd() * 3.0;
        self.parts.push(Particle { e, pos, vel, age: 0.0, life, rot, rotv, cell, ang, orad, h, dir: norm([dir[0], 0.0, dir[2]]), anim });
    }

    /// One-shot emitters (count) of an effect for a trigger.
    pub fn burst(&mut self, effect: &str, trigger: &str, at: V3, dir: V3, ability_radius: f32) {
        let rows: Vec<&'static Effect> = Self::rows(effect, trigger).map(|(_, e)| e).collect();
        for e in rows {
            for _ in 0..e.count {
                self.spawn(e, at, dir, ability_radius);
            }
        }
    }

    /// Continuous emitters (rate per second); `key` keeps one accumulator per source (missile, target...).
    pub fn stream(&mut self, effect: &str, trigger: &str, key: usize, at: V3, dir: V3, ability_radius: f32, dt: f32) {
        let rows: Vec<(usize, &'static Effect)> = Self::rows(effect, trigger).collect();
        for (i, e) in rows {
            let a = self.acc.entry((i, key)).or_insert(0.0);
            *a += e.rate * dt;
            let n = a.floor() as u32;
            *a -= n as f32;
            for _ in 0..n {
                self.spawn(e, at, dir, ability_radius);
            }
        }
    }

    /// `home` is where homing particles fly to (his chest).
    pub fn update(&mut self, dt: f32, caster: V3, home: V3) {
        for p in self.parts.iter_mut() {
            p.age += dt;
            p.rot += p.rotv * dt;
            p.anim += dt;
            match p.e.motion {
                "orbit" => {
                    p.ang += p.e.spin * dt;
                    p.h += (p.e.up - p.e.gravity * p.age) * dt;
                    let pos = add(caster, [p.ang.cos() * p.orad, p.h, p.ang.sin() * p.orad]);
                    p.vel = mul(sub(pos, p.pos), 1.0 / dt.max(1e-4));
                    p.pos = pos;
                }
                "home" => {
                    let d = sub(home, p.pos);
                    let remaining = (p.life - p.age).max(0.05);
                    p.vel = mul(norm(d), (len(d) / remaining).max(4.0));
                    p.pos = add(p.pos, mul(p.vel, dt));
                    if len(d) < 0.35 {
                        p.age = p.life;
                    }
                }
                _ => {
                    p.vel[1] -= p.e.gravity * dt;
                    p.vel = mul(p.vel, (1.0 - p.e.drag * dt).max(0.0));
                    p.pos = add(p.pos, mul(p.vel, dt));
                }
            }
        }
        self.parts.retain(|p| p.age < p.life);
    }

    /// Sprites (and crows) for every live particle: the items' ones (their own colours, not the skin's) or the rest.
    pub fn draw(&self, view: &View, ability_radius: impl Fn(&str) -> f32, out: &mut Vec<Sprite>, crows: &mut Vec<Crow>, items: bool) {
        for p in self.parts.iter().filter(|p| (p.e.effect == "items") == items) {
            let e = p.e;
            let t = (p.age / p.life).clamp(0.0, 1.0);
            let size = e.size_start + (e.size_end - e.size_start) * t;
            let color = lerp4(e.color_start, e.color_end, t);
            match e.render {
                "model" => crows.push(Crow { pos: p.pos, forward: norm(p.vel), scale: size, anim: p.anim, alpha: color[3] }),
                "stretched" => out.push(stretched(e, p.pos, p.vel, size, color, view)),
                "ground" => {
                    let r = if e.radius < 0.0 { ability_radius(e.effect) } else { 1.0 };
                    out.push(ground(e.texture, e.additive, p.pos, p.dir, size * r * 2.0, color));
                }
                _ => {
                    let w = 1.0 / e.frames.max(1) as f32;
                    let (u, v) = (p.cell[0], p.cell[1]);
                    let uv = [[u, v], [u + w, v], [u + w, v + w], [u, v + w]];
                    out.push(billboard(e.texture, e.additive, p.pos, size, p.rot, color, uv, view));
                }
            }
        }
    }
}

pub fn billboard(tex: &'static str, additive: bool, at: V3, size: f32, rot: f32, color: [f32; 4], uv: [[f32; 2]; 4], v: &View) -> Sprite {
    let (s, c) = rot.sin_cos();
    let r = add(mul(v.right, c * size * 0.5), mul(v.up, s * size * 0.5));
    let u = add(mul(v.up, c * size * 0.5), mul(v.right, -s * size * 0.5));
    Sprite { tex, additive, pos: [add(sub(at, r), u), add(add(at, r), u), sub(add(at, r), u), sub(sub(at, r), u)], uv, color }
}

/// A spark stretched along its velocity (texture's long axis is V).
fn stretched(e: &Effect, at: V3, vel: V3, size: f32, color: [f32; 4], v: &View) -> Sprite {
    let speed = len(vel);
    let axis = if speed > 0.01 { mul(vel, 1.0 / speed) } else { v.up };
    let side = mul(norm(cross(axis, norm(sub(v.pos, at)))), size * 0.25);
    let half = mul(axis, size * (0.6 + speed * 0.06));
    Sprite {
        tex: e.texture,
        additive: e.additive,
        pos: [add(add(at, half), side), add(sub(at, half), side), sub(sub(at, half), side), sub(add(at, half), side)],
        uv: [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]],
        color,
    }
}

/// A flat decal on the ground, its texture's top edge facing `dir`.
pub fn ground(tex: &'static str, additive: bool, at: V3, dir: V3, size: f32, color: [f32; 4]) -> Sprite {
    let f = mul(norm(if len(dir) < 0.01 { [0.0, 0.0, 1.0] } else { dir }), size * 0.5);
    let r = [f[2], 0.0, -f[0]];
    let p = add(at, [0.0, 0.08, 0.0]);
    Sprite {
        tex,
        additive,
        pos: [sub(add(p, f), r), add(add(p, f), r), add(sub(p, f), r), sub(sub(p, f), r)],
        uv: quad_uv(),
        color,
    }
}

/// The rows of an effect for a trigger (missile heads and beams are drawn by the game from its own state).
pub fn rows(effect: &str, trigger: &str) -> Vec<&'static Effect> {
    sheet::EFFECTS.iter().filter(|e| e.effect == effect && e.trigger == trigger).collect()
}
