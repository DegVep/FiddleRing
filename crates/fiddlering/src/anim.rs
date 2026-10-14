//! Fiddlesticks' animation states (animations sheet) sampled from his League clips, with cross-fades and the
//! clips' own submesh visibility events (demon arms, lantern, tongue).

use lolread::cache::{Cache, Clip};
use lolread::math::M4;
use lolread::{pose, sheet};

type Locals = Vec<([f32; 3], [f32; 4], [f32; 3])>;

pub struct Anim {
    pub cur: usize,
    pub t: f32,
    /// playback speed multiplier on top of the sheet's (locomotion scales it with ground speed)
    pub rate: f32,
    prev: Option<(usize, f32)>,
    fade: f32,
    fade_len: f32,
}

fn index(id: &str) -> usize {
    sheet::ANIMS.iter().position(|a| a.id == id).unwrap_or(0)
}

fn clip<'a>(c: &'a Cache, i: usize) -> Option<&'a Clip> {
    c.clips.iter().find(|k| k.id == sheet::ANIMS[i].id)
}

pub fn length(c: &Cache, id: &str) -> f32 {
    clip(c, index(id)).map(|k| (k.frames.max(1) - 1) as f32 / k.fps.max(1.0)).unwrap_or(0.0)
}

impl Anim {
    pub fn new() -> Self {
        Anim { cur: index("idle"), t: 0.0, rate: 1.0, prev: None, fade: 0.0, fade_len: 0.0 }
    }

    pub fn id(&self) -> &'static str {
        sheet::ANIMS[self.cur].id
    }

    /// Switches state (restarts it if `restart` or it is a one-shot).
    pub fn play(&mut self, id: &str, restart: bool) {
        let i = index(id);
        if i == self.cur && !restart && sheet::ANIMS[i].looped {
            return;
        }
        self.prev = Some((self.cur, self.t));
        self.fade_len = sheet::ANIMS[i].blend.max(0.01);
        self.fade = 0.0;
        self.cur = i;
        self.t = 0.0;
        self.rate = 1.0;
    }

    /// True while a one-shot (attack, spell, emote) is still playing.
    pub fn busy(&self, c: &Cache) -> bool {
        let a = &sheet::ANIMS[self.cur];
        !a.looped && self.t < length(c, a.id)
    }

    pub fn update(&mut self, c: &Cache, dt: f32) {
        let a = &sheet::ANIMS[self.cur];
        self.t += dt * a.speed * self.rate;
        if let Some((p, pt)) = self.prev.as_mut() {
            *pt += dt * sheet::ANIMS[*p].speed;
            self.fade += dt;
            if self.fade >= self.fade_len {
                self.prev = None;
            }
        }
        if !a.looped && self.t >= length(c, a.id) && a.next != "none" {
            self.play(a.next, true);
        }
    }

    fn locals(&self, c: &Cache, i: usize, t: f32) -> Locals {
        pose::sample_locals(&c.model, clip(c, i), t, sheet::ANIMS[i].looped)
    }

    pub fn palette(&self, c: &Cache) -> Vec<M4> {
        let mut l = self.locals(c, self.cur, self.t);
        if let Some((p, pt)) = self.prev {
            let w = (self.fade / self.fade_len).clamp(0.0, 1.0);
            l = pose::blend(&self.locals(c, p, pt), &l, w);
        }
        pose::palette(&c.model, &l)
    }

    /// Submesh visibility: the skin's hidden list, then this clip's show/hide events active at its time.
    pub fn visible(&self, c: &Cache) -> Vec<bool> {
        let mut vis: Vec<bool> = c.model.submeshes.iter().map(|s| !s.hidden).collect();
        if let Some(k) = clip(c, self.cur) {
            // League event times are frames at 30 fps
            let f = self.t * 30.0;
            for e in &k.submesh_events {
                if f >= e.start && (e.end < 0.0 || f <= e.end) {
                    for (i, s) in c.model.submeshes.iter().enumerate() {
                        if e.show.iter().any(|n| n.eq_ignore_ascii_case(&s.name)) {
                            vis[i] = true;
                        }
                        if e.hide.iter().any(|n| n.eq_ignore_ascii_case(&s.name)) {
                            vis[i] = false;
                        }
                    }
                }
            }
        }
        vis
    }
}
