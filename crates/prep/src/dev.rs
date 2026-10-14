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
            let Some(px) = lolread::texture::decode_rgba(t.format, t.width, t.height, &t.mips[0]) else {
                println!("{k}: format {} skipped", t.format);
                continue;
            };
            let f = std::fs::File::create(format!("{}/{k}.png", args[3]))?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&px)?;
            println!("{k}: {w}x{h} fmt {}", t.format);
        }
        return Ok(());
    }
    if args.get(1).map(|s| s.as_str()) == Some("--clips") {
        // clips of a base cache or a skin cache: id, frames, first rotation of the first track
        let b = std::fs::read(&args[2])?;
        let clips = match lolread::cache::load(&b) { Ok(c) => c.clips, Err(_) => lolread::cache::load_skin(&b)?.clips };
        for k in &clips { println!("{} {} frames {} r0 {:?}", k.id, k.league_name, k.frames, k.tracks.first().and_then(|t| t.r.first())); }
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
    if args.get(1).map(|s| s.as_str()) == Some("--extract") {
        let w = Wad::open(Path::new(&args[2]))?;
        std::fs::write(&args[4], w.read(&args[3])?)?;
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
