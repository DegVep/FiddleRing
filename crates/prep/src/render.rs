//! Offline oracle: rasterises the cached model in a pose to a PNG (front and side views), so the skeleton,
//! skinning and animation maths can be checked before going into Elden Ring.

use anyhow::Result;
use lolread::cache::{self, Tex};
use lolread::{math, pose, texture};

fn bc1_texel(t: &Tex, u: f32, v: f32) -> [f32; 3] {
    if t.format != texture::DXGI_BC1 || t.mips.is_empty() {
        return [0.7, 0.7, 0.7];
    }
    let (w, h) = (t.width as i32, t.height as i32);
    let x = ((u.rem_euclid(1.0)) * w as f32) as i32 % w;
    let y = ((v.rem_euclid(1.0)) * h as f32) as i32 % h;
    let bw = (w + 3) / 4;
    let block = ((y / 4) * bw + (x / 4)) as usize * 8;
    let b = &t.mips[0][block..block + 8];
    let c0 = u16::from_le_bytes([b[0], b[1]]);
    let c1 = u16::from_le_bytes([b[2], b[3]]);
    let rgb = |c: u16| [((c >> 11) & 31) as f32 / 31.0, ((c >> 5) & 63) as f32 / 63.0, (c & 31) as f32 / 31.0];
    let (a, bb) = (rgb(c0), rgb(c1));
    let idx = (u32::from_le_bytes([b[4], b[5], b[6], b[7]]) >> (2 * ((y % 4) * 4 + (x % 4)))) & 3;
    let mix = |p: f32| [a[0] + (bb[0] - a[0]) * p, a[1] + (bb[1] - a[1]) * p, a[2] + (bb[2] - a[2]) * p];
    match (idx, c0 > c1) {
        (0, _) => a,
        (1, _) => bb,
        (2, true) => mix(1.0 / 3.0),
        (3, true) => mix(2.0 / 3.0),
        (2, false) => mix(0.5),
        _ => [0.0, 0.0, 0.0],
    }
}

pub fn render(cache_path: &str, clip_id: &str, time: f32, out: &str) -> Result<()> {
    let c = cache::load(&std::fs::read(cache_path)?)?;
    let clip = c.clips.iter().find(|k| k.id == clip_id);
    let locals = pose::sample_locals(&c.model, clip, time, true);
    let pal = pose::palette(&c.model, &locals);
    let skinned: Vec<([f32; 3], [f32; 3])> = c
        .model
        .vertices
        .iter()
        .map(|v| {
            let mut p = [0f32; 3];
            let mut n = [0f32; 3];
            for k in 0..4 {
                let w = v.weights[k];
                if w <= 0.0 {
                    continue;
                }
                let m = &pal[v.joints[k] as usize];
                let q = math::transform_point(m, v.pos);
                let nn = [
                    v.normal[0] * m[0] + v.normal[1] * m[4] + v.normal[2] * m[8],
                    v.normal[0] * m[1] + v.normal[1] * m[5] + v.normal[2] * m[9],
                    v.normal[0] * m[2] + v.normal[1] * m[6] + v.normal[2] * m[10],
                ];
                for i in 0..3 {
                    p[i] += q[i] * w;
                    n[i] += nn[i] * w;
                }
            }
            (p, n)
        })
        .collect();
    let (wid, hei) = (800usize, 600usize);
    let mut img = vec![[30u8, 30, 40]; wid * hei];
    let mut zb = vec![f32::MIN; wid * hei];
    // views: front (looking at the model's +Z face) on the left, side (+X face) on the right
    for (view, ox) in [(0, 0usize), (1, wid / 2)] {
        let proj = |p: [f32; 3]| -> (f32, f32, f32) {
            let (x, z) = if view == 0 { (p[0], p[2]) } else { (-p[2], p[0]) };
            (ox as f32 + wid as f32 / 4.0 + x * 1.6, hei as f32 - 40.0 - p[1] * 1.6, z)
        };
        for sm in c.model.submeshes.iter().filter(|s| !s.hidden) {
            let tex = &c.model.textures[sm.texture as usize];
            for tri in c.model.indices[sm.first_index as usize..(sm.first_index + sm.index_count) as usize].chunks(3) {
                let vs: Vec<_> = tri.iter().map(|&i| (proj(skinned[i as usize].0), skinned[i as usize].1, c.model.vertices[i as usize].uv)).collect();
                let (a, b, cc) = (vs[0].0, vs[1].0, vs[2].0);
                let area = (b.0 - a.0) * (cc.1 - a.1) - (b.1 - a.1) * (cc.0 - a.0);
                if area.abs() < 1e-6 {
                    continue;
                }
                let minx = a.0.min(b.0).min(cc.0).max(ox as f32) as i32;
                let maxx = a.0.max(b.0).max(cc.0).min((ox + wid / 2 - 1) as f32) as i32;
                let miny = a.1.min(b.1).min(cc.1).max(0.0) as i32;
                let maxy = a.1.max(b.1).max(cc.1).min(hei as f32 - 1.0) as i32;
                for y in miny..=maxy {
                    for x in minx..=maxx {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let w0 = ((b.0 - px) * (cc.1 - py) - (b.1 - py) * (cc.0 - px)) / area;
                        let w1 = ((cc.0 - px) * (a.1 - py) - (cc.1 - py) * (a.0 - px)) / area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let z = a.2 * w0 + b.2 * w1 + cc.2 * w2;
                        let i = y as usize * wid + x as usize;
                        if z <= zb[i] {
                            continue;
                        }
                        zb[i] = z;
                        let uv = [0, 1].map(|k| vs[0].2[k] * w0 + vs[1].2[k] * w1 + vs[2].2[k] * w2);
                        let n = [0, 1, 2].map(|k| vs[0].1[k] * w0 + vs[1].1[k] * w1 + vs[2].1[k] * w2);
                        let nl = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
                        let light = 0.45 + 0.55 * ((n[1] * 0.5 + if view == 0 { n[2] } else { n[0] } * 0.8) / nl).max(0.0);
                        let col = bc1_texel(tex, uv[0], uv[1]);
                        img[i] = col.map(|v| (v * light * 255.0).clamp(0.0, 255.0) as u8);
                    }
                }
            }
        }
    }
    let f = std::fs::File::create(out)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), wid as u32, hei as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header()?;
    w.write_image_data(&img.concat())?;
    println!("height {:.1} units, {} joints, clip {:?}", c.model.height, c.model.joints.len(), clip.map(|k| &k.league_name));
    Ok(())
}
