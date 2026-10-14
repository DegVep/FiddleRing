//! Skinned meshes (`.skn`), skeletons (`.skl`) and uncompressed animations (`.anm`, r3d2anmd v4/v5).

use anyhow::{Result, bail};

pub(crate) struct Rd<'a> {
    pub b: &'a [u8],
    pub o: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Rd { b, o: 0 }
    }
    pub fn at(b: &'a [u8], o: usize) -> Self {
        Rd { b, o }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.o + n > self.b.len() {
            bail!("truncated at {} (+{n})", self.o);
        }
        let s = &self.b[self.o..self.o + n];
        self.o += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into()?))
    }
    pub fn v3(&mut self) -> Result<[f32; 3]> {
        Ok([self.f32()?, self.f32()?, self.f32()?])
    }
    pub fn v4(&mut self) -> Result<[f32; 4]> {
        Ok([self.f32()?, self.f32()?, self.f32()?, self.f32()?])
    }
    pub fn cstr(&mut self) -> Result<String> {
        let start = self.o;
        while self.o < self.b.len() && self.b[self.o] != 0 {
            self.o += 1;
        }
        let s = String::from_utf8_lossy(&self.b[start..self.o]).into_owned();
        self.o += 1;
        Ok(s)
    }
    pub fn padded(&mut self, n: usize) -> Result<String> {
        let s = self.take(n)?;
        let end = s.iter().position(|&c| c == 0).unwrap_or(n);
        Ok(String::from_utf8_lossy(&s[..end]).into_owned())
    }
}

pub fn elf_hash(s: &str) -> u32 {
    let mut h: u32 = 0;
    for c in s.to_ascii_lowercase().bytes() {
        h = (h << 4).wrapping_add(c as u32);
        let high = h & 0xF000_0000;
        if high != 0 {
            h ^= high >> 24;
        }
        h &= !high;
    }
    h
}

#[derive(Clone, Debug)]
pub struct SknVertex {
    pub pos: [f32; 3],
    pub bones: [u8; 4],
    pub weights: [f32; 4],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

pub struct SknRange {
    pub material: String,
    pub first_vertex: u32,
    pub vertex_count: u32,
    pub first_index: u32,
    pub index_count: u32,
}

pub struct Skn {
    pub ranges: Vec<SknRange>,
    pub indices: Vec<u16>,
    pub vertices: Vec<SknVertex>,
}

pub fn read_skn(b: &[u8]) -> Result<Skn> {
    let mut r = Rd::new(b);
    if r.u32()? != 0x0011_2233 {
        bail!("not a skn");
    }
    let major = r.u16()?;
    let _minor = r.u16()?;
    let mut ranges = vec![];
    let (index_count, vertex_count);
    let mut vsize = 52u32;
    if major == 0 {
        index_count = r.u32()?;
        vertex_count = r.u32()?;
    } else {
        let n = r.u32()?;
        for _ in 0..n {
            ranges.push(SknRange {
                material: r.padded(64)?,
                first_vertex: r.u32()?,
                vertex_count: r.u32()?,
                first_index: r.u32()?,
                index_count: r.u32()?,
            });
        }
        if major == 4 {
            r.u32()?; // flags
        }
        index_count = r.u32()?;
        vertex_count = r.u32()?;
        if major == 4 {
            vsize = r.u32()?;
            r.u32()?; // vertex type
            r.take(24)?; // box
            r.take(16)?; // sphere
        }
    }
    if ranges.is_empty() {
        ranges.push(SknRange { material: "Base".into(), first_vertex: 0, vertex_count, first_index: 0, index_count });
    }
    let mut indices = Vec::with_capacity(index_count as usize);
    for _ in 0..index_count {
        indices.push(r.u16()?);
    }
    let mut vertices = Vec::with_capacity(vertex_count as usize);
    for _ in 0..vertex_count {
        let start = r.o;
        let pos = r.v3()?;
        let bi = r.take(4)?;
        let weights = r.v4()?;
        let normal = r.v3()?;
        let uv = [r.f32()?, r.f32()?];
        r.o = start + vsize as usize;
        vertices.push(SknVertex { pos, bones: [bi[0], bi[1], bi[2], bi[3]], weights, normal, uv });
    }
    Ok(Skn { ranges, indices, vertices })
}

#[derive(Clone, Debug)]
pub struct SklJoint {
    pub name: String,
    pub hash: u32,
    pub parent: i16,
    pub local_t: [f32; 3],
    pub local_s: [f32; 3],
    pub local_r: [f32; 4],
    pub ibind_t: [f32; 3],
    pub ibind_s: [f32; 3],
    pub ibind_r: [f32; 4],
}

pub struct Skl {
    pub joints: Vec<SklJoint>,
    /// mesh bone index -> joint index
    pub influences: Vec<i16>,
}

pub fn read_skl(b: &[u8]) -> Result<Skl> {
    let mut r = Rd::new(b);
    r.u32()?; // file size
    let magic = r.u32()?;
    if magic != 0x22FD_4FC3 {
        bail!("legacy skl not supported (magic {magic:08x})");
    }
    r.u32()?; // version
    r.u16()?; // flags
    let joint_count = r.u16()? as usize;
    let influence_count = r.u32()? as usize;
    let joints_off = r.i32()? as usize;
    let _joint_indices_off = r.i32()?;
    let influences_off = r.i32()? as usize;
    let mut joints = Vec::with_capacity(joint_count);
    let mut j = Rd::at(b, joints_off);
    for _ in 0..joint_count {
        j.u16()?; // flags
        let _id = j.i16()?;
        let parent = j.i16()?;
        j.i16()?;
        let hash = j.u32()?;
        j.f32()?; // radius
        let local_t = j.v3()?;
        let local_s = j.v3()?;
        let local_r = j.v4()?;
        let ibind_t = j.v3()?;
        let ibind_s = j.v3()?;
        let ibind_r = j.v4()?;
        let name_off = j.i32()?;
        let name = Rd::at(b, j.o - 4 + name_off as usize).cstr()?;
        joints.push(SklJoint { name, hash, parent, local_t, local_s, local_r, ibind_t, ibind_s, ibind_r });
    }
    let mut ir = Rd::at(b, influences_off);
    let influences = (0..influence_count).map(|_| ir.i16()).collect::<Result<_>>()?;
    Ok(Skl { joints, influences })
}

fn decompress_quat(d: &[u8]) -> [f32; 4] {
    let bits = u16::from_le_bytes([d[0], d[1]]) as u64
        | (u16::from_le_bytes([d[2], d[3]]) as u64) << 16
        | (u16::from_le_bytes([d[4], d[5]]) as u64) << 32;
    let max = (bits >> 45) & 3;
    let f = |v: u64| (v as f64 / 32767.0 * std::f64::consts::SQRT_2 - std::f64::consts::FRAC_1_SQRT_2) as f32;
    let a = f((bits >> 30) & 0x7FFF);
    let b = f((bits >> 15) & 0x7FFF);
    let c = f(bits & 0x7FFF);
    let d = (1.0 - (a * a + b * b + c * c)).max(0.0).sqrt();
    let q = match max {
        0 => [d, a, b, c],
        1 => [a, d, b, c],
        2 => [a, b, d, c],
        _ => [a, b, c, d],
    };
    let n = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    [q[0] / n, q[1] / n, q[2] / n, q[3] / n]
}

pub struct AnmTrack {
    pub joint_hash: u32,
    pub t: Vec<[f32; 3]>,
    pub r: Vec<[f32; 4]>,
    pub s: Vec<[f32; 3]>,
}

pub struct Anm {
    pub fps: f32,
    pub frames: u32,
    pub tracks: Vec<AnmTrack>,
}

/// Compressed animations (r3d2canm): a key stream of (time, joint|channel, 3 x u16). Baked to evenly spaced
/// frames with linear interpolation between keys.
fn read_canm(b: &[u8]) -> Result<Anm> {
    let mut r = Rd::at(b, 12);
    r.u32()?; // resource size
    r.u32()?; // format token
    r.u32()?; // flags
    let joint_count = r.i32()? as usize;
    let frame_count = r.i32()? as usize;
    r.i32()?; // jump caches
    let duration = r.f32()?;
    let fps = r.f32()?;
    r.take(24)?; // error metrics
    let t_min = r.v3()?;
    let t_max = r.v3()?;
    let s_min = r.v3()?;
    let s_max = r.v3()?;
    let frames_off = r.i32()? as usize;
    r.i32()?;
    let hashes_off = r.i32()? as usize;
    let mut hr = Rd::at(b, hashes_off + 12);
    let hashes: Vec<u32> = (0..joint_count).map(|_| hr.u32()).collect::<Result<_>>()?;
    // keys[joint][channel] = (time, value)
    let mut keys: Vec<[Vec<(f32, [f32; 4])>; 3]> = (0..joint_count).map(|_| [vec![], vec![], vec![]]).collect();
    let mut fr = Rd::at(b, frames_off + 12);
    let dv = |v: &[u16], lo: [f32; 3], hi: [f32; 3]| {
        [0, 1, 2].map(|i| lo[i] + (hi[i] - lo[i]) * (v[i] as f32 / 65535.0))
    };
    for _ in 0..frame_count {
        let time = fr.u16()? as f32 / 65535.0 * duration;
        let jid = fr.u16()?;
        let raw = fr.take(6)?;
        let v = [
            u16::from_le_bytes([raw[0], raw[1]]),
            u16::from_le_bytes([raw[2], raw[3]]),
            u16::from_le_bytes([raw[4], raw[5]]),
        ];
        let (joint, channel) = ((jid & 0x3FFF) as usize, (jid >> 14) as usize);
        if joint >= joint_count || channel > 2 {
            continue;
        }
        let value = match channel {
            0 => decompress_quat(raw),
            1 => {
                let p = dv(&v, t_min, t_max);
                [p[0], p[1], p[2], 0.0]
            }
            _ => {
                let p = dv(&v, s_min, s_max);
                [p[0], p[1], p[2], 0.0]
            }
        };
        keys[joint][channel].push((time, value));
    }
    let frames = ((duration * fps).round() as usize).max(1) + 1;
    let sample = |k: &mut Vec<(f32, [f32; 4])>, t: f32, quat: bool| -> [f32; 4] {
        if k.is_empty() {
            return if quat { [0., 0., 0., 1.] } else { [1., 1., 1., 0.] };
        }
        let i = k.partition_point(|(kt, _)| *kt <= t);
        if i == 0 {
            return k[0].1;
        }
        if i >= k.len() {
            return k[k.len() - 1].1;
        }
        let (t0, a) = k[i - 1];
        let (t1, b) = k[i];
        let f = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
        if quat {
            crate::math::nlerp(a, b, f)
        } else {
            [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f, 0.0]
        }
    };
    let mut tracks = vec![];
    for (j, ch) in keys.iter_mut().enumerate() {
        for c in ch.iter_mut() {
            c.sort_by(|a, b| a.0.total_cmp(&b.0));
        }
        let mut tr = AnmTrack { joint_hash: hashes[j], t: vec![], r: vec![], s: vec![] };
        for f in 0..frames {
            let t = f as f32 / fps;
            let rq = sample(&mut ch[0], t, true);
            let tv = sample(&mut ch[1], t, false);
            let sv = sample(&mut ch[2], t, false);
            tr.r.push(rq);
            tr.t.push([tv[0], tv[1], tv[2]]);
            tr.s.push([sv[0], sv[1], sv[2]]);
        }
        tracks.push(tr);
    }
    Ok(Anm { fps, frames: frames as u32, tracks })
}

pub fn read_anm(b: &[u8]) -> Result<Anm> {
    if &b[0..8] == b"r3d2canm" {
        return read_canm(b);
    }
    if &b[0..8] != b"r3d2anmd" {
        bail!("unsupported animation {:?}", String::from_utf8_lossy(&b[0..8]));
    }
    let mut r = Rd::at(b, 8);
    let version = r.u32()?;
    if version != 5 && version != 4 {
        bail!("anm version {version} not supported");
    }
    r.u32()?; // resource size
    r.u32()?; // format token
    r.u32()?; // version
    r.u32()?; // flags
    let track_count = r.i32()? as usize;
    let frames = r.i32()? as usize;
    let frame_duration = r.f32()?;
    if version == 5 {
        let joint_hashes_off = r.i32()? as usize;
        let _asset_name_off = r.i32()?;
        let _time_off = r.i32()?;
        let vec_off = r.i32()? as usize;
        let quat_off = r.i32()? as usize;
        let frames_off = r.i32()? as usize;
        let n_hashes = (frames_off - joint_hashes_off) / 4;
        let mut hr = Rd::at(b, joint_hashes_off + 12);
        let hashes: Vec<u32> = (0..n_hashes).map(|_| hr.u32()).collect::<Result<_>>()?;
        let mut vr = Rd::at(b, vec_off + 12);
        let vecs: Vec<[f32; 3]> = (0..(quat_off - vec_off) / 12).map(|_| vr.v3()).collect::<Result<_>>()?;
        let mut qr = Rd::at(b, quat_off + 12);
        let quats: Vec<[f32; 4]> =
            (0..(joint_hashes_off - quat_off) / 6).map(|_| Ok(decompress_quat(qr.take(6)?))).collect::<Result<_>>()?;
        let mut tracks: Vec<AnmTrack> = hashes
            .iter()
            .take(track_count)
            .map(|&h| AnmTrack { joint_hash: h, t: vec![], r: vec![], s: vec![] })
            .collect();
        let mut fr = Rd::at(b, frames_off + 12);
        for _ in 0..frames {
            for t in tracks.iter_mut() {
                let ti = fr.u16()? as usize;
                let si = fr.u16()? as usize;
                let ri = fr.u16()? as usize;
                t.t.push(vecs[ti]);
                t.s.push(vecs[si]);
                t.r.push(quats[ri]);
            }
        }
        Ok(Anm { fps: 1.0 / frame_duration, frames: frames as u32, tracks })
    } else {
        // v4: per-frame entries of (joint hash u32, translation u16, scale u16, rotation u16, pad u16)
        let vec_off = r.i32()? as usize;
        let quat_off = r.i32()? as usize;
        let frames_off = r.i32()? as usize;
        let mut vr = Rd::at(b, vec_off + 12);
        let vecs: Vec<[f32; 3]> = (0..(quat_off - vec_off) / 12).map(|_| vr.v3()).collect::<Result<_>>()?;
        let mut qr = Rd::at(b, quat_off + 12);
        let quats: Vec<[f32; 4]> =
            (0..(frames_off - quat_off) / 16).map(|_| qr.v4()).collect::<Result<_>>()?;
        let mut tracks: Vec<AnmTrack> = vec![];
        let mut fr = Rd::at(b, frames_off + 12);
        for _ in 0..frames {
            for i in 0..track_count {
                let h = fr.u32()?;
                let ti = fr.u16()? as usize;
                let si = fr.u16()? as usize;
                let ri = fr.u16()? as usize;
                fr.u16()?;
                if tracks.len() <= i {
                    tracks.push(AnmTrack { joint_hash: h, t: vec![], r: vec![], s: vec![] });
                }
                let bad = || anyhow::anyhow!("anm v4: frame index out of range");
                tracks[i].t.push(*vecs.get(ti).ok_or_else(bad)?);
                tracks[i].s.push(*vecs.get(si).ok_or_else(bad)?);
                tracks[i].r.push(*quats.get(ri).ok_or_else(bad)?);
            }
        }
        Ok(Anm { fps: 1.0 / frame_duration, frames: frames as u32, tracks })
    }
}
