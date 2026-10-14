//! League of Legends WAD archives (`*.wad.client`), versions 3.x.
//! Chunks are addressed by the xxhash64 of their lowercase path.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct Chunk {
    pub hash: u64,
    pub offset: u64,
    pub compressed: u32,
    pub size: u32,
    pub kind: u8,
    pub subchunk_count: u8,
    pub subchunk_start: u32,
}

pub struct Wad {
    pub path: PathBuf,
    pub version: (u8, u8),
    pub chunks: HashMap<u64, Chunk>,
    subchunks: Vec<(u32, u32)>,
}

pub fn hash_path(path: &str) -> u64 {
    xxhash_rust::xxh64::xxh64(path.to_ascii_lowercase().as_bytes(), 0)
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

impl Wad {
    pub fn open(path: &Path) -> Result<Wad> {
        let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let mut head = [0u8; 272];
        f.read_exact(&mut head)?;
        if &head[0..2] != b"RW" {
            bail!("{} is not a WAD", path.display());
        }
        let (major, minor) = (head[2], head[3]);
        if major != 3 {
            bail!("unsupported WAD version {major}.{minor}");
        }
        let count = u32le(&head, 268) as usize;
        let mut toc = vec![0u8; count * 32];
        f.read_exact(&mut toc)?;
        let mut chunks = HashMap::with_capacity(count);
        for i in 0..count {
            let e = &toc[i * 32..i * 32 + 32];
            let hash = u64::from_le_bytes(e[0..8].try_into().unwrap());
            let kind_byte = e[20];
            // 3.4+ packs the first subchunk index into 24 bits (bytes 21..24).
            // 3.4 widened the first subchunk index to 24 bits; byte 21 is its high byte.
            let subchunk_start = u16::from_le_bytes([e[22], e[23]]) as u32
                | if minor >= 4 { (e[21] as u32) << 16 } else { 0 };
            chunks.insert(
                hash,
                Chunk {
                    hash,
                    offset: u32le(e, 8) as u64,
                    compressed: u32le(e, 12),
                    size: u32le(e, 16),
                    kind: kind_byte & 0xF,
                    subchunk_count: kind_byte >> 4,
                    subchunk_start,
                },
            );
        }
        let mut wad = Wad { path: path.to_path_buf(), version: (major, minor), chunks, subchunks: Vec::new() };
        // The subchunk table is a chunk named after the wad's path under Game/.
        if let Some(rel) = game_relative(path) {
            let toc_path = rel.trim_end_matches(".client").trim_end_matches(".wad").to_string() + ".wad.subchunktoc";
            if let Some(c) = wad.chunks.get(&hash_path(&toc_path)).copied() {
                let data = wad.read_chunk(&c)?;
                wad.subchunks = data
                    .chunks_exact(16)
                    .map(|s| (u32le(s, 0), u32le(s, 4)))
                    .collect();
            }
        }
        Ok(wad)
    }

    pub fn get(&self, path: &str) -> Option<Chunk> {
        self.chunks.get(&hash_path(path)).copied()
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let c = self.get(path).with_context(|| format!("{path} not in {}", self.path.display()))?;
        self.read_chunk(&c)
    }

    pub fn read_chunk(&self, c: &Chunk) -> Result<Vec<u8>> {
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(c.offset))?;
        let mut raw = vec![0u8; c.compressed as usize];
        f.read_exact(&mut raw)?;
        match c.kind {
            0 => Ok(raw),
            1 => bail!("gzip chunks are not used by current League"),
            2 => bail!("satellite chunk"),
            3 => zstd(&raw, c.size as usize),
            4 => {
                let mut out = Vec::with_capacity(c.size as usize);
                let mut at = 0usize;
                for i in 0..c.subchunk_count as usize {
                    let (cs, us) = *self
                        .subchunks
                        .get(c.subchunk_start as usize + i)
                        .context("subchunk table missing")?;
                    let part = &raw[at..at + cs as usize];
                    if cs == us {
                        out.extend_from_slice(part);
                    } else {
                        out.extend(zstd(part, us as usize)?);
                    }
                    at += cs as usize;
                }
                Ok(out)
            }
            k => bail!("unknown chunk kind {k}"),
        }
    }
}

fn zstd(data: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    let mut dec = ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| anyhow::anyhow!("zstd: {e}"))?;
    dec.read_to_end(&mut out)?;
    Ok(out)
}

/// `.../Game/DATA/FINAL/Champions/X.wad.client` -> `data/final/champions/x.wad.client`
fn game_relative(path: &Path) -> Option<String> {
    let s = path.to_string_lossy().replace('\\', "/");
    let i = s.rfind("/Game/")?;
    Some(s[i + 6..].to_ascii_lowercase())
}
