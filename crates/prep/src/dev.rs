use anyhow::Result;
use lolread::{bin, wad::Wad};
use std::path::Path;
use crate::dump;

/// Development commands used while reverse engineering League's files.
pub fn run(args: &[String]) -> Result<()> {
    if args.get(1).map(|s| s.as_str()) == Some("--dumptex") {
        // --dumptex <cache> <outdir>: every cached texture's top mip as PNG (decoded BC1/BC3/BGRA)
        let c = lolread::cache::load(&std::fs::read(&args[2])?)?;
        std::fs::create_dir_all(&args[3])?;
        for (k, t) in c.icons.iter() {
            let (w, h) = (t.width as usize, t.height as usize);
            let src = &t.mips[0];
            let mut px = vec![0u8; w * h * 4];
            match t.format {
                87 => {
                    for i in 0..w * h {
                        px[i * 4..i * 4 + 4].copy_from_slice(&[src[i * 4 + 2], src[i * 4 + 1], src[i * 4], src[i * 4 + 3]]);
                    }
                }
                71 | 77 => {
                    let bs = if t.format == 71 { 8 } else { 16 };
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
                f => {
                    println!("{k}: format {f} skipped");
                    continue;
                }
            }
            let f = std::fs::File::create(format!("{}/{k}.png", args[3]))?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&px)?;
            println!("{k}: {w}x{h} fmt {}", t.format);
        }
        return Ok(());
    }
    if args.get(1).map(|s| s.as_str()) == Some("--show") {
        let c = lolread::cache::load(&std::fs::read(&args[2])?)?;
        println!("stats {:?}", c.stats);
        for (k, s) in &c.spells {
            println!("{k}: cast_frame {} cast_time {} missile {}", s.cast_frame, s.cast_time, s.missile_speed);
            for (n, v) in &s.values { println!("   {n} {v:?}"); }
        }
        for k in &c.clips { println!("clip {} frames {} fps {} events {}", k.id, k.frames, k.fps, k.submesh_events.len()); }
        for s in &c.model.submeshes { println!("submesh {} tex {} hidden {}", s.name, s.texture, s.hidden); }
        for (i, j) in c.model.joints.iter().enumerate() { println!("joint {i} {} parent {}", j.name, j.parent); }
        for (k, t) in &c.icons { println!("tex {k} {}x{} fmt {} mips {}", t.width, t.height, t.format, t.mips.len()); }
        return Ok(());
    }
    if args.get(1).map(|s| s.as_str()) == Some("--explore") {
        let w = Wad::open(Path::new(&args[2]))?;
        println!("version {:?} chunks {}", w.version, w.chunks.len());
        let mut kinds = std::collections::BTreeMap::new();
        for c in w.chunks.values() {
            let d = w.read_chunk(c)?;
            let magic = d.get(0..8).map(|m| m.iter().map(|b| if b.is_ascii_graphic() { *b as char } else { '.' }).collect::<String>()).unwrap_or_default();
            *kinds.entry(magic.clone()).or_insert(0) += 1;
            if magic.starts_with("PROP") || magic.starts_with("PTCH") {
                if let Ok(b) = bin::parse(&d) {
                    let mut s = vec![];
                    let vals: Vec<_> = b.objects.iter().map(|o| o.as_value()).collect(); for v in &vals { v.strings(&mut s); }
                    s.sort(); s.dedup();
                    println!("BIN {:016x} objs {} deps {:?}", c.hash, b.objects.len(), b.deps);
                    for x in s.iter().filter(|x| x.contains('/') || x.contains('.')) { println!("   {x}"); }
                } else { println!("BIN {:016x} parse failed", c.hash); }
            }
        }
        println!("{kinds:#?}");
    }
    if args.get(1).map(|s| s.as_str()) == Some("--dump") {
        let w = Wad::open(Path::new(&args[2]))?;
        let names = dump::Names::load(&args[4]);
        let b = bin::parse(&w.read(&args[3])?)?;
        println!("deps {:?}", b.deps);
        print!("{}", dump::dump(&names, &b));
    }
    if args.get(1).map(|s| s.as_str()) == Some("--magic") {
        let w = Wad::open(Path::new(&args[2]))?;
        for h in &args[3..] {
            let hash: u64 = h.parse()?;
            let c = w.chunks.get(&hash).copied();
            match c { Some(c) => { let d = w.read_chunk(&c)?; println!("{h}: {:?} len {}", String::from_utf8_lossy(&d[..12.min(d.len())]), d.len()); } None => println!("{h}: missing") }
        }
    }
    if args.get(1).map(|s| s.as_str()) == Some("--has") {
        let w = Wad::open(Path::new(&args[2]))?;
        for p in &args[3..] { match w.get(p) { Some(c) => { let d = w.read_chunk(&c)?; println!("YES {p} len {} magic {:?}", d.len(), String::from_utf8_lossy(&d[..8.min(d.len())])); } None => println!("no  {p}") } }
    }
    if args.get(1).map(|s| s.as_str()) == Some("--bnk") {
        let w = Wad::open(Path::new(&args[2]))?;
        let ev = lolread::audio::read_bnk(&w.read(&args[3])?)?;
        let au = lolread::audio::read_bnk(&w.read(&args[4])?)?;
        let mut types = std::collections::BTreeMap::new();
        for (_, (t, _)) in ev.objects.iter().chain(au.objects.iter()) { *types.entry(*t).or_insert(0) += 1; }
        println!("types {types:?} wems {}", au.wems.len());
        for name in &args[5..] {
            let id = lolread::audio::wwise_id(name);
            let o = ev.objects.get(&id);
            println!("{name} id {id} -> {:?}", o.map(|(t, b)| (t, b.len(), b.iter().take(24).map(|x| format!("{x:02x}")).collect::<String>())));
            println!("   resolved {:?}", lolread::audio::resolve_event(id, &[&ev, &au]));
            if let Some((_, b)) = o { if b.len() >= 5 { let a = u32::from_le_bytes(b[1..5].try_into()?); let ao = ev.objects.get(&a).or(au.objects.get(&a)); println!("   action {a} -> {:?}", ao.map(|(t, b)| (t, b.iter().take(32).map(|x| format!("{x:02x}")).collect::<String>())));
              if let Some((_, ab)) = ao { let tgt = u32::from_le_bytes(ab[2..6].try_into()?); let to = au.objects.get(&tgt).or(ev.objects.get(&tgt)); println!("   in ev {} in au {}", ev.objects.contains_key(&tgt), au.objects.contains_key(&tgt)); println!("   target {tgt} -> {:?}", to.map(|(t, b)| (t, b.len(), b.iter().take(48).map(|x| format!("{x:02x}")).collect::<String>()))); } } }
        }
    }
    Ok(())
}
