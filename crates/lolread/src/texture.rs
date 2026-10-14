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
