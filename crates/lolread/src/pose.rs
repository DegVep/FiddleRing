//! Animation sampling and skinning palette (shared by the DLL and prep's offline render check).

use crate::cache::{Clip, Model};
use crate::math::{self, M4};

/// Local TRS for every joint at `time` seconds (bind pose for joints the clip doesn't move).
pub fn sample_locals(model: &Model, clip: Option<&Clip>, time: f32, looped: bool) -> Vec<([f32; 3], [f32; 4], [f32; 3])> {
    let mut locals: Vec<_> = model.joints.iter().map(|j| (j.t, j.r, j.s)).collect();
    let Some(c) = clip else { return locals };
    if c.frames == 0 {
        return locals;
    }
    let last = (c.frames - 1) as f32;
    let mut f = time * c.fps;
    if looped && last > 0.0 {
        f %= last;
    }
    let f = f.clamp(0.0, last);
    let i0 = f.floor() as usize;
    let i1 = (i0 + 1).min(c.frames as usize - 1);
    let a = f - i0 as f32;
    for t in &c.tracks {
        let j = t.joint as usize;
        if j >= locals.len() || t.r.len() <= i1 {
            continue;
        }
        locals[j] = (math::lerp3(t.t[i0], t.t[i1], a), math::nlerp(t.r[i0], t.r[i1], a), math::lerp3(t.s[i0], t.s[i1], a));
    }
    locals
}

pub fn blend(a: &[([f32; 3], [f32; 4], [f32; 3])], b: &[([f32; 3], [f32; 4], [f32; 3])], w: f32) -> Vec<([f32; 3], [f32; 4], [f32; 3])> {
    a.iter()
        .zip(b)
        .map(|(x, y)| (math::lerp3(x.0, y.0, w), math::nlerp(x.1, y.1, w), math::lerp3(x.2, y.2, w)))
        .collect()
}

/// Skinning matrices: inverse bind * global, row-vector convention.
pub fn palette(model: &Model, locals: &[([f32; 3], [f32; 4], [f32; 3])]) -> Vec<M4> {
    let n = model.joints.len();
    let mut global: Vec<M4> = vec![math::IDENTITY; n];
    // Parents come before children in League skeletons; fall back to a second pass if not.
    for _ in 0..2 {
        for (i, j) in model.joints.iter().enumerate() {
            let (t, r, s) = locals[i];
            let local = math::trs(t, r, s);
            global[i] = if j.parent >= 0 && (j.parent as usize) < n { math::mul(&local, &global[j.parent as usize]) } else { local };
        }
        if model.joints.iter().enumerate().all(|(i, j)| j.parent < i as i16) {
            break;
        }
    }
    model.joints.iter().zip(&global).map(|(j, g)| math::mul(&j.ibind, g)).collect()
}
