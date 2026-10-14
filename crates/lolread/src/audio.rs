//! Wwise sound banks (`.bnk`) and League wem packs (`.wpk`): event name -> wem ids -> wem bytes.

use crate::mesh::Rd;
use anyhow::{Result, bail};
use std::collections::{HashMap, HashSet};

/// Wwise short ids are FNV-1 (not 1a) of the lowercase name.
pub fn wwise_id(name: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for b in name.to_ascii_lowercase().bytes() {
        h = h.wrapping_mul(16777619);
        h ^= b as u32;
    }
    h
}

#[derive(Default)]
pub struct Bank {
    /// HIRC objects: id -> (type, body after the id)
    pub objects: HashMap<u32, (u8, Vec<u8>)>,
    /// embedded wems: id -> bytes
    pub wems: HashMap<u32, Vec<u8>>,
}

pub fn read_bnk(b: &[u8]) -> Result<Bank> {
    let mut bank = Bank::default();
    let mut didx: Vec<(u32, u32, u32)> = vec![];
    let mut data: &[u8] = &[];
    let mut r = Rd::new(b);
    while r.o + 8 <= b.len() {
        let tag = r.take(4)?;
        let size = r.u32()? as usize;
        let body = r.take(size)?;
        match tag {
            b"DIDX" => {
                for e in body.chunks_exact(12) {
                    didx.push((
                        u32::from_le_bytes(e[0..4].try_into()?),
                        u32::from_le_bytes(e[4..8].try_into()?),
                        u32::from_le_bytes(e[8..12].try_into()?),
                    ));
                }
            }
            b"DATA" => data = body,
            b"HIRC" => {
                let mut h = Rd::new(body);
                let n = h.u32()?;
                for _ in 0..n {
                    let t = h.u8()?;
                    let s = h.u32()? as usize;
                    let o = h.take(s)?;
                    if s >= 4 {
                        let id = u32::from_le_bytes(o[0..4].try_into()?);
                        bank.objects.insert(id, (t, o[4..].to_vec()));
                    }
                }
            }
            _ => {}
        }
    }
    for (id, off, size) in didx {
        let (off, size) = (off as usize, size as usize);
        if off + size <= data.len() {
            bank.wems.insert(id, data[off..off + size].to_vec());
        }
    }
    Ok(bank)
}

pub fn read_wpk(b: &[u8]) -> Result<HashMap<u32, Vec<u8>>> {
    if &b[0..4] != b"r3d2" {
        bail!("not a wpk");
    }
    let mut r = Rd::at(b, 4);
    r.u32()?; // version
    let n = r.u32()?;
    let offs: Vec<u32> = (0..n).map(|_| r.u32()).collect::<Result<_>>()?;
    let mut out = HashMap::new();
    for off in offs {
        if off == 0 {
            continue;
        }
        let mut e = Rd::at(b, off as usize);
        let data_off = e.u32()? as usize;
        let size = e.u32()? as usize;
        let name_len = e.u32()? as usize;
        let name: String = e.take(name_len * 2)?.chunks_exact(2).map(|c| c[0] as char).collect();
        let id: u32 = name.trim_end_matches(".wem").parse().unwrap_or(0);
        if id != 0 && data_off + size <= b.len() {
            out.insert(id, b[data_off..data_off + size].to_vec());
        }
    }
    Ok(out)
}

/// Event ids defined by an events bank.
pub fn event_ids(events: &Bank) -> HashSet<u32> {
    events.objects.iter().filter(|(_, (t, _))| *t == 4).map(|(id, _)| *id).collect()
}

fn find<'a>(banks: &[&'a Bank], id: u32) -> Option<&'a (u8, Vec<u8>)> {
    banks.iter().find_map(|b| b.objects.get(&id))
}

/// Resolves an event to the wem ids it can play. League keeps events, actions and the sound hierarchy in the
/// events bank and the wem data in the audio bank / wpk, so lookups go through every bank given.
pub fn resolve_event(event: u32, banks: &[&Bank]) -> Vec<u32> {
    let Some((4, body)) = find(banks, event).map(|(t, b)| (*t, b)) else { return vec![] };
    let actions: Vec<u32> = {
        let n8 = body.first().copied().unwrap_or(0) as usize;
        if body.len() == 1 + 4 * n8 {
            body[1..].chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
        } else if body.len() >= 4 {
            let n = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
            body[4..].chunks_exact(4).take(n).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
        } else {
            vec![]
        }
    };
    let mut out = vec![];
    for a in actions {
        let Some((3, ab)) = find(banks, a).map(|(t, b)| (*t, b)) else {
            continue;
        };
        if ab.len() < 6 {
            continue;
        }
        let action_type = u16::from_le_bytes([ab[0], ab[1]]);
        if action_type >> 8 != 0x04 {
            continue; // only Play actions
        }
        let target = u32::from_le_bytes(ab[2..6].try_into().unwrap());
        collect(target, banks, &mut out, 0);
    }
    out.sort();
    out.dedup();
    out
}

fn collect(id: u32, banks: &[&Bank], out: &mut Vec<u32>, depth: u32) {
    if depth > 8 {
        return;
    }
    let Some((t, body)) = find(banks, id) else { return };
    match t {
        2 => {
            // plugin id u32, stream type u8, source id u32
            if body.len() >= 9 {
                out.push(u32::from_le_bytes(body[5..9].try_into().unwrap()));
            }
        }
        5 | 6 | 7 | 9 => {
            for child in child_list(body, banks) {
                collect(child, banks, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Containers end their node params with `count u32, ids[count]`; find the longest run of known object ids
/// preceded by its count instead of parsing every version's node layout.
fn child_list(body: &[u8], banks: &[&Bank]) -> Vec<u32> {
    let mut best: Vec<u32> = vec![];
    for p in 0..body.len().saturating_sub(8) {
        let n = u32::from_le_bytes(body[p..p + 4].try_into().unwrap()) as usize;
        if n == 0 || n > 256 || p + 4 + n * 4 > body.len() {
            continue;
        }
        let ids: Vec<u32> = body[p + 4..p + 4 + n * 4]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        if ids.iter().all(|i| find(banks, *i).is_some()) && ids.len() > best.len() {
            best = ids;
        }
    }
    best
}
