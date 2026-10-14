//! League `.tex` and `.dds` textures -> raw mip chain (largest first) with a DXGI format.

use crate::mesh::Rd;
use anyhow::{Result, bail};

pub const DXGI_BC1: u32 = 71;
pub const DXGI_BC3: u32 = 77;
pub const DXGI_BC5: u32 = 83;
pub const DXGI_BGRA8: u32 = 87;
pub const DXGI_BC7: u32 = 98;

#[derive(Clone)]
pub struct Texture {
    pub format: u32,
    pub width: u32,
    pub height: u32,
    pub mips: Vec<Vec<u8>>,
}

pub fn mip_size(format: u32, w: u32, h: u32) -> usize {
    let (bw, bh) = (w.div_ceil(4).max(1) as usize, h.div_ceil(4).max(1) as usize);
    match format {
        DXGI_BC1 => bw * bh * 8,
        DXGI_BC3 | DXGI_BC5 | DXGI_BC7 => bw * bh * 16,
        _ => (w * h * 4) as usize,
    }
}

pub fn read_tex(b: &[u8]) -> Result<Texture> {
    if &b[0..4] != b"TEX\0" {
        bail!("not a TEX");
    }
    let mut r = Rd::at(b, 4);
    let width = r.u16()? as u32;
    let height = r.u16()? as u32;
    r.u8()?;
    let fmt = r.u8()?;
    r.u8()?;
    let flags = r.u8()?;
    let format = match fmt {
        10 | 11 => DXGI_BC1,
        12 => DXGI_BC3,
        13 => DXGI_BC7,
        14 => DXGI_BC5,
        20 => DXGI_BGRA8,
        f => bail!("TEX format {f} not supported"),
    };
    let count = if flags & 1 != 0 { 32 - width.max(height).leading_zeros() } else { 1 };
    // Mips are stored smallest first; the largest ends at the end of the file.
    let mut end = b.len();
    let mut mips = Vec::new();
    for i in 0..count {
        let s = mip_size(format, (width >> i).max(1), (height >> i).max(1));
        if s > end - 12 {
            bail!("TEX truncated");
        }
        mips.push(b[end - s..end].to_vec());
        end -= s;
    }
    Ok(Texture { format, width, height, mips })
}

pub fn read_dds(b: &[u8]) -> Result<Texture> {
    if &b[0..4] != b"DDS " {
        bail!("not a DDS");
    }
    let mut r = Rd::at(b, 4);
    r.u32()?; // header size
    r.u32()?; // flags
    let height = r.u32()?;
    let width = r.u32()?;
    r.u32()?;
    r.u32()?;
    let mip_count = r.u32()?.max(1);
    r.take(44)?;
    r.u32()?; // pf size
    let pf_flags = r.u32()?;
    let fourcc = r.take(4)?;
    let bits = r.u32()?;
    let mut data_off = 128;
    let format = if pf_flags & 4 != 0 {
        match fourcc {
            b"DXT1" => DXGI_BC1,
            b"DXT5" | b"DXT4" => DXGI_BC3,
            b"DX10" => {
                let f = u32::from_le_bytes(b[128..132].try_into()?);
                data_off = 148;
                f
            }
            f => bail!("DDS fourcc {:?} not supported", String::from_utf8_lossy(f)),
        }
    } else if bits == 32 {
        DXGI_BGRA8
    } else {
        bail!("DDS pixel format not supported");
    };
    let mut at = data_off;
    let mut mips = vec![];
    for i in 0..mip_count {
        let s = mip_size(format, (width >> i).max(1), (height >> i).max(1));
        if at + s > b.len() {
            break;
        }
        mips.push(b[at..at + s].to_vec());
        at += s;
    }
    Ok(Texture { format, width, height, mips })
}

pub fn read_any(b: &[u8]) -> Result<Texture> {
    if b.starts_with(b"DDS ") { read_dds(b) } else { read_tex(b) }
}

/// Top-mip pixels as RGBA8 (BGRA8, BC1 and BC3; other formats give None).
pub fn decode_rgba(format: u32, w: u32, h: u32, src: &[u8]) -> Option<Vec<u8>> {
    let (w, h) = (w as usize, h as usize);
    let mut px = vec![0u8; w * h * 4];
    match format {
        87 => {
            for i in 0..w * h {
                px[i * 4..i * 4 + 4].copy_from_slice(&[src[i * 4 + 2], src[i * 4 + 1], src[i * 4], src[i * 4 + 3]]);
            }
        }
        71 | 77 => {
            let bs = if format == 71 { 8 } else { 16 };
            let bw = (w + 3) / 4;
            for by in 0..(h + 3) / 4 {
                for bx in 0..bw {
                    let b = &src[(by * bw + bx) * bs..(by * bw + bx + 1) * bs];
                    let (ab, cb) = if bs == 16 { (Some(&b[..8]), &b[8..]) } else { (None, b) };
                    let c0 = u16::from_le_bytes([cb[0], cb[1]]);
                    let c1 = u16::from_le_bytes([cb[2], cb[3]]);
                    let rgb = |c: u16| [((c >> 11) & 31) as f32 * 255.0 / 31.0, ((c >> 5) & 63) as f32 * 255.0 / 63.0, (c & 31) as f32 * 255.0 / 31.0];
                    let (p0, p1) = (rgb(c0), rgb(c1));
                    let mix = |a: f32| [0, 1, 2].map(|k| p0[k] + (p1[k] - p0[k]) * a);
                    let pal = if c0 > c1 || bs == 16 { [p0, p1, mix(1.0 / 3.0), mix(2.0 / 3.0)] } else { [p0, p1, mix(0.5), [0.0; 3]] };
                    let idx = u32::from_le_bytes([cb[4], cb[5], cb[6], cb[7]]);
                    let alphas: [u8; 16] = match ab {
                        Some(a) => {
                            let (a0, a1) = (a[0] as f32, a[1] as f32);
                            let bits = u64::from_le_bytes([a[2], a[3], a[4], a[5], a[6], a[7], 0, 0]);
                            std::array::from_fn(|i| {
                                let k = ((bits >> (3 * i)) & 7) as f32;
                                (match k as u32 {
                                    0 => a0,
                                    1 => a1,
                                    _ if a0 > a1 => (a0 * (8.0 - k) + a1 * (k - 1.0)) / 7.0,
                                    6 => 0.0,
                                    7 => 255.0,
                                    _ => (a0 * (6.0 - k) + a1 * (k - 1.0)) / 5.0,
                                }) as u8
                            })
                        }
                        None => std::array::from_fn(|i| if c0 <= c1 && (idx >> (2 * i)) & 3 == 3 { 0 } else { 255 }),
                    };
                    for i in 0..16 {
                        let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                        if x < w && y < h {
                            let c = pal[((idx >> (2 * i)) & 3) as usize];
                            px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c[0] as u8, c[1] as u8, c[2] as u8, alphas[i]]);
                        }
                    }
                }
            }
        }
        _ => return None,
    }
    Some(px)
}
