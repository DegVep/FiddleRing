//! fiddlering-prep: reads Fiddlesticks from the player's own League of Legends install and writes the local
//! cache the Elden Ring DLL draws and plays from. Runs as Melty's setup step (and from the DLL if the cache is
//! missing or League updated). Nothing from League is shipped with the mod.

mod dev;
mod dump;
mod render;

use anyhow::{Context, Result, anyhow, bail};
use lolread::bin::{self, Bin, Value, fnv1a};
use lolread::cache::{self, *};
use lolread::{audio, math, mesh, sheet, texture, wad::Wad};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| matches!(a.as_str(), "--explore" | "--dumptex" | "--show" | "--dump" | "--magic" | "--has" | "--bnk")) {
        if let Err(e) = dev::run(&args) {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("--render") {
        let r = render::render(&args[2], &args[3], args[4].parse().unwrap_or(0.0), &args[5]);
        if let Err(e) = r {
            eprintln!("{e:#}");
        }
        return;
    }
    let failure = arg_value(&args, "--failure-file").map(PathBuf::from);
    match prepare(&args) {
        Ok(msg) => println!("{msg}"),
        Err(e) => {
            let text = format!("FiddleRing could not read Fiddlesticks from League of Legends: {e:#}");
            eprintln!("{text}");
            if let Some(f) = failure {
                let _ = std::fs::write(f, &text);
            }
            std::process::exit(1);
        }
    }
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn cache_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("FiddleRing").join("cache")
}

/// `{game:league-of-legends}` may be the install root, its Game folder or a parent; find the champion WADs.
fn find_champions_dir(root: &Path) -> Result<PathBuf> {
    let candidates = [
        root.join("Game/DATA/FINAL/Champions"),
        root.join("DATA/FINAL/Champions"),
        root.join("League of Legends/Game/DATA/FINAL/Champions"),
    ];
    for c in candidates {
        if c.is_dir() {
            return Ok(c);
        }
    }
    bail!("no Game/DATA/FINAL/Champions under {}", root.display())
}

fn find_file(dir: &Path, pred: impl Fn(&str) -> bool) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().map(|n| pred(&n.to_string_lossy().to_ascii_lowercase())).unwrap_or(false))
}

fn league_root(args: &[String]) -> Result<PathBuf> {
    if let Some(p) = arg_value(args, "--league") {
        return Ok(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("FIDDLERING_LOL") {
        return Ok(PathBuf::from(p));
    }
    for p in ["C:/Riot Games/League of Legends", "D:/Riot Games/League of Legends"] {
        if Path::new(p).is_dir() {
            return Ok(PathBuf::from(p));
        }
    }
    bail!("League of Legends folder not given (FIDDLERING_LOL)")
}

fn read_league_version(champions: &Path) -> String {
    let game = champions.parent().and_then(|p| p.parent()).and_then(|p| p.parent());
    game.and_then(|g| std::fs::read_to_string(g.join("content-metadata.json")).ok())
        .and_then(|t| t.split("\"version\"").nth(1).map(|s| s.to_string()))
        .and_then(|s| s.split('"').nth(1).map(|v| v.to_string()))
        .unwrap_or_else(|| "unknown".into())
}

fn prepare(args: &[String]) -> Result<String> {
    let root = league_root(args)?;
    let champions = find_champions_dir(&root)?;
    let main_wad_path = find_file(&champions, |n| n == "fiddlesticks.wad.client")
        .context("FiddleSticks.wad.client not found: is League of Legends fully installed?")?;
    let locale_wad_path = find_file(&champions, |n| n.starts_with("fiddlesticks.") && n.ends_with(".wad.client") && n != "fiddlesticks.wad.client");
    let league_version = read_league_version(&champions);
    let out = arg_value(args, "--out").map(PathBuf::from).unwrap_or_else(cache_dir);
    let ready = out.join("ready.txt");
    let stamp = format!("fiddlering-cache-v{} league-{}", cache::CACHE_VERSION, league_version);
    if !args.iter().any(|a| a == "--force") && std::fs::read_to_string(&ready).map(|s| s.trim() == stamp).unwrap_or(false) {
        return Ok(format!("cache up to date ({stamp})"));
    }
    std::fs::create_dir_all(out.join("audio"))?;
    let _ = std::fs::remove_file(&ready);

    let main_wad = Wad::open(&main_wad_path)?;
    let locale_wad = match &locale_wad_path {
        Some(p) => Some(Wad::open(p)?),
        None => None,
    };
    let locale = locale_wad_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().split('.').nth(1).unwrap_or("").to_string())
        .unwrap_or_default();

    let mut found_assets: Vec<String> = vec![];
    let skin = bin::parse(&main_wad.read("data/characters/fiddlesticks/skins/skin0.bin")?)?;
    found_assets.push("skin_bin".into());
    let anim_bin = bin::parse(&main_wad.read("data/characters/fiddlesticks/animations/skin0.bin")?)?;
    found_assets.push("anim_bin".into());
    let champ = bin::parse(&main_wad.read("data/characters/fiddlesticks/fiddlesticks.bin")?)?;
    found_assets.push("champ_bin".into());

    let model = build_model(&main_wad, &skin, &mut found_assets)?;
    let (clips, found_clips) = build_clips(&main_wad, &anim_bin, &model)?;
    let (spells, stats) = build_spells(&champ)?;
    let mut icons = BTreeMap::new();
    for a in sheet::ASSETS.iter().filter(|a| a.kind == "tex" || a.kind == "dds") {
        if a.locator.contains(':') {
            continue;
        }
        if let Ok(b) = main_wad.read(a.locator) {
            if let Ok(t) = texture::read_any(&b) {
                icons.insert(a.id.to_string(), to_tex(t));
                found_assets.push(a.id.to_string());
            }
        }
    }
    let (sounds, audio_assets) = build_audio(&main_wad, locale_wad.as_ref(), &out.join("audio"))?;
    found_assets.extend(audio_assets);

    let c = Cache {
        version: cache::CACHE_VERSION,
        league_version: league_version.clone(),
        locale: locale.clone(),
        model,
        clips,
        stats,
        spells: spells.clone(),
        icons,
        sounds: sounds.clone(),
        raven: build_raven(&main_wad, &mut found_assets),
    };
    std::fs::write(out.join("fiddlesticks.bin"), cache::save(&c)?)?;

    let mut v = String::from("{\n");
    v += &format!("  \"league_version\": {:?},\n  \"locale\": {:?},\n", league_version, locale);
    v += &format!("  \"assets\": {:?},\n", found_assets);
    v += &format!("  \"clips\": {:?},\n", found_clips);
    v += &format!("  \"sounds\": {:?},\n", sounds.keys().collect::<Vec<_>>());
    v += "  \"spells\": {";
    v += &spells
        .iter()
        .map(|(k, s)| format!("{:?}: {:?}", k, s.values.keys().collect::<Vec<_>>()))
        .collect::<Vec<_>>()
        .join(", ");
    v += "}\n}\n";
    std::fs::write(out.join("verify.json"), v)?;
    std::fs::write(&ready, &stamp)?;
    Ok(format!(
        "Fiddlesticks ready: {} vertices, {} joints, {}/{} animations, {} abilities, {} sounds, locale {} ({})",
        c.model.vertices.len(),
        c.model.joints.len(),
        found_clips.len(),
        sheet::ANIMS.len(),
        spells.len(),
        sounds.len(),
        locale,
        stamp
    ))
}

fn obj<'a>(b: &'a Bin, path: &str) -> Option<&'a bin::Object> {
    let h = fnv1a(path);
    b.objects.iter().find(|o| o.path == h)
}

fn field_h(v: &Value, h: u32) -> Option<&Value> {
    match v {
        Value::Struct(_, f) => f.iter().find(|(n, _)| *n == h).map(|(_, v)| v),
        Value::Opt(Some(v)) => field_h(v, h),
        _ => None,
    }
}

fn read_link(wad: &Wad, v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Link(h) => wad.read_chunk(&wad.chunks.get(h).copied().ok_or_else(|| anyhow!("link {h:x} missing"))?),
        Value::Str(s) => wad.read(s),
        _ => bail!("not a file reference"),
    }
}

fn to_tex(t: texture::Texture) -> Tex {
    Tex { format: t.format, width: t.width, height: t.height, mips: t.mips }
}

fn build_model(wad: &Wad, skin: &Bin, found: &mut Vec<String>) -> Result<Model> {
    let o = obj(skin, "Characters/FiddleSticks/Skins/Skin0").context("skin0 object missing")?.as_value();
    let mp = o.field("skinMeshProperties").context("skinMeshProperties missing")?;
    let skl = mesh::read_skl(&wad.read(mp.field("skeleton").and_then(|v| v.as_str()).context("skeleton")?)?)?;
    found.push("skeleton".into());
    let skn = mesh::read_skn(&wad.read(mp.field("simpleSkin").and_then(|v| v.as_str()).context("simpleSkin")?)?)?;
    found.push("mesh".into());
    let body = texture::read_tex(&read_link(wad, mp.field("texture").context("texture")?)?)?;
    found.push("tex_body".into());
    let mut textures = vec![to_tex(body)];
    let mut submesh_tex: HashMap<String, u32> = HashMap::new();
    for ov in mp.field("materialOverride").map(|v| v.list()).unwrap_or(&[]) {
        if let (Some(t), Some(sm)) = (ov.field("texture"), ov.field("Submesh").and_then(|v| v.as_str())) {
            if let Ok(t) = read_link(wad, t).and_then(|b| texture::read_tex(&b)) {
                submesh_tex.insert(sm.to_ascii_lowercase(), textures.len() as u32);
                textures.push(to_tex(t));
                if sm.eq_ignore_ascii_case("weapon") {
                    found.push("tex_weapon".into());
                }
            }
        }
    }
    let hidden: HashSet<String> = mp
        .field("initialSubmeshToHide")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .split_whitespace()
        .map(|s| s.to_ascii_lowercase())
        .collect();

    let mut joints = vec![];
    for j in &skl.joints {
        let ib = math::trs(j.ibind_t, j.ibind_r, j.ibind_s);
        joints.push(Joint { name: j.name.clone(), parent: j.parent, t: j.local_t, r: j.local_r, s: j.local_s, ibind: ib });
    }
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    let vertices: Vec<Vertex> = skn
        .vertices
        .iter()
        .map(|v| {
            lo = lo.min(v.pos[1]);
            hi = hi.max(v.pos[1]);
            let map = |b: u8| skl.influences.get(b as usize).copied().unwrap_or(0).max(0) as u16;
            Vertex {
                pos: v.pos,
                normal: v.normal,
                uv: v.uv,
                joints: [map(v.bones[0]), map(v.bones[1]), map(v.bones[2]), map(v.bones[3])],
                weights: v.weights,
            }
        })
        .collect();
    let submeshes = skn
        .ranges
        .iter()
        .map(|r| Submesh {
            name: r.material.clone(),
            first_index: r.first_index,
            index_count: r.index_count,
            texture: submesh_tex.get(&r.material.to_ascii_lowercase()).copied().unwrap_or(0),
            hidden: hidden.contains(&r.material.to_ascii_lowercase()),
        })
        .collect();
    Ok(Model { joints, vertices, indices: skn.indices, submeshes, textures, height: hi - lo })
}

/// The crow of Terrify / Crowstorm: its own skeleton, mesh and flight loop, textured with the Q crow atlas.
fn build_raven(wad: &Wad, found: &mut Vec<String>) -> Option<(Model, Clip)> {
    let path = |id: &str| sheet::ASSETS.iter().find(|a| a.id == id).map(|a| a.locator).unwrap_or("");
    let skl = mesh::read_skl(&wad.read(path("raven_skl")).ok()?).ok()?;
    found.push("raven_skl".into());
    let skn = mesh::read_skn(&wad.read(path("raven_skn")).ok()?).ok()?;
    found.push("raven_skn".into());
    let tex = wad.read(path("fx_q_crow")).ok().and_then(|b| texture::read_any(&b).ok())?;
    let joints: Vec<Joint> = skl
        .joints
        .iter()
        .map(|j| Joint { name: j.name.clone(), parent: j.parent, t: j.local_t, r: j.local_r, s: j.local_s, ibind: math::trs(j.ibind_t, j.ibind_r, j.ibind_s) })
        .collect();
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    let vertices = skn
        .vertices
        .iter()
        .map(|v| {
            lo = lo.min(v.pos[1]);
            hi = hi.max(v.pos[1]);
            let map = |b: u8| skl.influences.get(b as usize).copied().unwrap_or(0).max(0) as u16;
            Vertex { pos: v.pos, normal: v.normal, uv: v.uv, joints: [map(v.bones[0]), map(v.bones[1]), map(v.bones[2]), map(v.bones[3])], weights: v.weights }
        })
        .collect();
    let submeshes = skn
        .ranges
        .iter()
        .map(|r| Submesh { name: r.material.clone(), first_index: r.first_index, index_count: r.index_count, texture: 0, hidden: false })
        .collect();
    let model = Model { joints, vertices, indices: skn.indices, submeshes, textures: vec![to_tex(tex)], height: hi - lo };
    let anm = mesh::read_anm(&wad.read(path("raven_anm")).ok()?).ok()?;
    found.push("raven_anm".into());
    let by_hash: HashMap<u32, u16> = model.joints.iter().enumerate().map(|(i, j)| (mesh::elf_hash(&j.name), i as u16)).collect();
    let tracks = anm.tracks.into_iter().filter_map(|t| by_hash.get(&t.joint_hash).map(|&j| Track { joint: j, t: t.t, r: t.r, s: t.s })).collect();
    let clip = Clip { id: "raven_fly".into(), league_name: "Q_RavenFly".into(), fps: anm.fps, frames: anm.frames, tracks, submesh_events: vec![] };
    println!("raven: {} vertices, {} joints, {} frames, height {:.0}", model.vertices.len(), model.joints.len(), clip.frames, model.height);
    Some((model, clip))
}

fn build_clips(wad: &Wad, anim_bin: &Bin, model: &Model) -> Result<(Vec<Clip>, Vec<String>)> {
    let graph = obj(anim_bin, "Characters/FiddleSticks/Animations/Skin0").context("animation graph missing")?.as_value();
    let Some(Value::Map(clips)) = graph.field("mClipDataMap") else { bail!("mClipDataMap missing") };
    let joint_by_hash: HashMap<u32, u16> =
        model.joints.iter().enumerate().map(|(i, j)| (mesh::elf_hash(&j.name), i as u16)).collect();
    let submesh_by_hash: HashMap<u32, String> =
        model.submeshes.iter().map(|s| (fnv1a(&s.name), s.name.clone())).collect();
    let mut out = vec![];
    let mut found = vec![];
    for a in sheet::ANIMS {
        let key = fnv1a(a.league_clip);
        let Some(entry) = clips.iter().find(|(k, _)| matches!(k, Value::Hash(h) if *h == key)) else { continue };
        let mut clip: &Value = &entry.1;
        // sequencers (e.g. Melee_Attack1) list their clips: play the first one
        if clip.field("mAnimationResourceData").is_none() {
            let first = clip.field("mClipNameList").and_then(|l| l.list().first()).and_then(|h| if let Value::Hash(h) = h { Some(*h) } else { None });
            let Some(inner) = first.and_then(|f| clips.iter().find(|(k, _)| matches!(k, Value::Hash(h) if *h == f))) else { continue };
            clip = &inner.1;
        }
        let Some(path) = clip.field("mAnimationResourceData").and_then(|r| r.field("mAnimationFilePath")) else { continue };
        let Ok(anm) = read_link(wad, path).and_then(|b| mesh::read_anm(&b)) else { continue };
        let tracks = anm
            .tracks
            .into_iter()
            .filter_map(|t| joint_by_hash.get(&t.joint_hash).map(|&j| Track { joint: j, t: t.t, r: t.r, s: t.s }))
            .collect();
        let mut events = vec![];
        if let Some(Value::Map(ev)) = clip.field("mEventDataMap") {
            for (_, e) in ev {
                let names = |f: &str| -> Vec<String> {
                    e.field(f)
                        .map(|l| l.list().iter().filter_map(|h| if let Value::Hash(h) = h { submesh_by_hash.get(h).cloned() } else { None }).collect())
                        .unwrap_or_default()
                };
                let (show, hide) = (names("mShowSubmeshList"), names("mHideSubmeshList"));
                if show.is_empty() && hide.is_empty() {
                    continue;
                }
                events.push(SubmeshEvent {
                    start: e.field("mStartFrame").and_then(|v| v.as_f32()).unwrap_or(0.0),
                    end: e.field("mEndFrame").and_then(|v| v.as_f32()).unwrap_or(-1.0),
                    show,
                    hide,
                });
            }
        }
        out.push(Clip {
            id: a.id.to_string(),
            league_name: a.league_clip.to_string(),
            fps: anm.fps,
            frames: anm.frames,
            tracks,
            submesh_events: events,
        });
        found.push(a.id.to_string());
    }
    Ok((out, found))
}

fn floats(v: &Value) -> Vec<f32> {
    v.list().iter().filter_map(|x| x.as_f32()).collect()
}

fn build_spells(champ: &Bin) -> Result<(BTreeMap<String, Spell>, BTreeMap<String, f32>)> {
    let mut spells = BTreeMap::new();
    for a in sheet::ABILITIES {
        let Some(o) = obj(champ, a.league_spell) else { continue };
        let v = o.as_value();
        let Some(s) = v.field("mSpell") else { continue };
        let mut sp = Spell::default();
        for dv in s.field("DataValues").map(|l| l.list()).unwrap_or(&[]) {
            if let (Some(n), Some(vals)) = (dv.field("name").and_then(|n| n.as_str()), dv.field("values")) {
                // Some spells list a name twice (champion vs monster variant); keep the first.
                sp.values.entry(n.to_string()).or_insert_with(|| floats(vals));
            }
        }
        for f in ["CooldownTime", "Mana", "CastRange", "castRadius", "mChannelDuration"] {
            if let Some(x) = s.field(f) {
                sp.values.insert(f.to_string(), floats(x));
            }
        }
        sp.cast_frame = s.field("castFrame").and_then(|x| x.as_f32()).unwrap_or(0.0);
        sp.cast_time = s.field("mCastTime").and_then(|x| x.as_f32()).unwrap_or(0.0);
        sp.missile_speed = s.field("missileSpeed").and_then(|x| x.as_f32()).unwrap_or(0.0);
        spells.insert(a.id.to_string(), sp);
    }
    let rec = obj(champ, "Characters/FiddleSticks/CharacterRecords/Root").context("character record missing")?.as_value();
    let mut stats = BTreeMap::new();
    let base = |f: &str| rec.field(f).and_then(|m| m.field("BaseValue")).and_then(|x| x.as_f32());
    for (k, f) in [
        ("hp", "baseHPModifiable"),
        ("hp_per_level", "hpPerLevelModifiable"),
        ("ad", "baseDamageModifiable"),
        ("ad_per_level", "damagePerLevelModifiable"),
        ("attack_range", "attackRangeModifiable"),
        ("attack_speed", "attackSpeedModifiable"),
        ("attack_speed_ratio", "attackSpeedRatioModifiable"),
        ("attack_speed_per_level", "attackSpeedPerLevelModifiable"),
        ("move_speed", "baseMoveSpeedModifiable"),
    ] {
        if let Some(v) = base(f) {
            stats.insert(k.to_string(), v);
        }
    }
    if let Some(res) = rec.field("primaryAbilityResource") {
        for (k, h) in [("mana", 0x726ee5cdu32), ("mana_per_level", 0x6216bf7b), ("mana_regen", 0xc4ab3550), ("mana_regen_per_level", 0x3a509002)] {
            if let Some(v) = field_h(res, h).and_then(|m| m.field("BaseValue")).and_then(|x| x.as_f32()) {
                stats.insert(k.to_string(), v);
            }
        }
    }
    for k in ["hp", "ad", "attack_range", "attack_speed", "mana", "mana_regen"] {
        if !stats.contains_key(k) {
            bail!("League stat {k} not found for Fiddlesticks");
        }
    }
    Ok((spells, stats))
}

fn all_chunks(w: &Wad) -> Vec<Vec<u8>> {
    w.chunks.values().filter_map(|c| w.read_chunk(c).ok()).collect()
}

fn build_audio(main: &Wad, locale: Option<&Wad>, audio_dir: &Path) -> Result<(BTreeMap<String, Vec<String>>, Vec<String>)> {
    let mut found = vec![];
    let sfx_events = audio::read_bnk(&main.read(
        sheet::ASSETS.iter().find(|a| a.id == "sfx_events").unwrap().locator,
    )?)?;
    let sfx_audio = audio::read_bnk(&main.read(sheet::ASSETS.iter().find(|a| a.id == "sfx_audio").unwrap().locator)?)?;
    found.push("sfx_events".into());
    found.push("sfx_audio".into());

    // Localised VO: find the events bank by the ids it holds, then whichever bank/wpk resolves them.
    let probe = audio::wwise_id(sheet::ASSETS.iter().find(|a| a.id == "vo_events").unwrap().locator.trim_start_matches("bankevents:"));
    let mut vo_events = audio::Bank::default();
    let mut vo_audio_banks: Vec<audio::Bank> = vec![];
    let mut wpk_wems: HashMap<u32, Vec<u8>> = HashMap::new();
    if let Some(lw) = locale {
        for data in all_chunks(lw) {
            if data.starts_with(b"BKHD") {
                if let Ok(b) = audio::read_bnk(&data) {
                    if b.objects.get(&probe).map(|(t, _)| *t == 4).unwrap_or(false) {
                        vo_events = b;
                    } else {
                        vo_audio_banks.push(b);
                    }
                }
            } else if data.starts_with(b"r3d2") {
                if let Ok(w) = audio::read_wpk(&data) {
                    wpk_wems.extend(w);
                }
            }
        }
        if !vo_events.objects.is_empty() {
            found.push("vo_events".into());
        }
        if !wpk_wems.is_empty() {
            found.push("vo_audio".into());
        }
    }

    let mut wav_by_wem: HashMap<u32, String> = HashMap::new();
    let mut pending: Vec<(u32, PathBuf)> = vec![];
    let mut cue_wems: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for s in sheet::SOUNDS {
        let id = audio::wwise_id(s.event);
        let mut wems = vec![];
        let mut bytes: HashMap<u32, Vec<u8>> = HashMap::new();
        if s.bank == "sfx" {
            for w in audio::resolve_event(id, &[&sfx_events, &sfx_audio]) {
                if let Some(b) = sfx_audio.wems.get(&w) {
                    bytes.insert(w, b.clone());
                }
                wems.push(w);
            }
        } else {
            for bank in &vo_audio_banks {
                let r = audio::resolve_event(id, &[&vo_events, bank]);
                if !r.is_empty() {
                    for w in &r {
                        if let Some(b) = wpk_wems.get(w).or_else(|| bank.wems.get(w)) {
                            bytes.insert(*w, b.clone());
                        }
                    }
                    wems = r;
                    break;
                }
            }
        }
        let mut have = vec![];
        for w in wems {
            if let Some(b) = bytes.get(&w) {
                if !wav_by_wem.contains_key(&w) {
                    let p = audio_dir.join(format!("{w}.wem"));
                    std::fs::write(&p, b)?;
                    pending.push((w, p));
                    wav_by_wem.insert(w, format!("{w}.wav"));
                }
                have.push(w);
            }
        }
        if !have.is_empty() {
            cue_wems.insert(s.id.to_string(), have);
        }
    }
    decode_wems(&pending)?;
    let mut sounds = BTreeMap::new();
    for (cue, wems) in cue_wems {
        let files: Vec<String> = wems
            .iter()
            .filter(|w| audio_dir.join(format!("{w}.wav")).exists())
            .map(|w| format!("{w}.wav"))
            .collect();
        if !files.is_empty() {
            sounds.insert(cue, files);
        }
    }
    for (_, p) in &pending {
        let _ = std::fs::remove_file(p);
    }
    Ok((sounds, found))
}

/// Wwise Vorbis -> PCM wav with vgmstream-cli (bundled next to this exe).
fn decode_wems(pending: &[(u32, PathBuf)]) -> Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let exe_dir = std::env::current_exe()?.parent().map(Path::to_path_buf).unwrap_or_default();
    let cli = [exe_dir.join("vgmstream/vgmstream-cli.exe"), exe_dir.join("vgmstream-cli.exe")]
        .into_iter()
        .find(|p| p.exists())
        .context("vgmstream-cli.exe missing next to fiddlering-prep.exe")?;
    for batch in pending.chunks(40) {
        let mut cmd = std::process::Command::new(&cli);
        cmd.arg("-o").arg("?f.wav");
        for (_, p) in batch {
            cmd.arg(p);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output()?;
        if !out.status.success() {
            eprintln!("vgmstream: {}", String::from_utf8_lossy(&out.stderr));
        }
        for (w, p) in batch {
            // "?f" keeps the full input name: 123.wem -> 123.wem.wav
            let produced = PathBuf::from(format!("{}.wav", p.display()));
            let want = p.with_file_name(format!("{w}.wav"));
            if produced.exists() {
                std::fs::rename(&produced, &want)?;
            }
        }
    }
    Ok(())
}
