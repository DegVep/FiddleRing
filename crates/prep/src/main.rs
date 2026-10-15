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
    if args.iter().any(|a| matches!(a.as_str(), "--explore" | "--dumptex" | "--show" | "--clips" | "--extract" | "--dump" | "--magic" | "--has" | "--grep" | "--bnk")) {
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
    if let Some(n) = arg_value(&args, "--skin").and_then(|n| n.parse::<u32>().ok()) {
        match prepare_skin(&args, n) {
            Ok(msg) => println!("{msg}"),
            Err(e) => {
                eprintln!("skin {n}: {e:#}");
                std::process::exit(1);
            }
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
    // skins built for the previous League or cache are rebuilt when chosen again
    let _ = std::fs::remove_dir_all(out.join("skins"));

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

    let model = build_model(&main_wad, &skin, &skin_obj_path(0), &mut found_assets)?;
    let (clips, found_clips) = build_clips(&main_wad, &anim_bin, fnv1a("Characters/FiddleSticks/Animations/Skin0"), &model)?;
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
    let skins = list_skins(&main_wad, &champions, &locale, &mut icons);
    let items = build_items(&champions, &locale, &mut icons);
    // item sounds (Zhonya) live in the map's shared wad, next to Champions
    let common_wad = champions.parent().map(|d| d.join("Maps/Shipping/Common.wad.client")).and_then(|p| Wad::open(&p).ok());
    let (sounds, audio_assets) = build_audio(&main_wad, locale_wad.as_ref(), common_wad.as_ref(), &out.join("audio"))?;
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
        skins,
        items,
        vfx: obj(&skin, &skin_obj_path(0)).map(|o| build_vfx(&main_wad, &skin, &o.as_value())).unwrap_or_default(),
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

fn build_model(wad: &Wad, skin: &Bin, obj_path: &str, found: &mut Vec<String>) -> Result<Model> {
    let o = obj(skin, obj_path).with_context(|| format!("{obj_path} missing"))?.as_value();
    let mp = o.field("skinMeshProperties").context("skinMeshProperties missing")?;
    let skl = mesh::read_skl(&wad.read(mp.field("skeleton").and_then(|v| v.as_str()).context("skeleton")?)?)?;
    found.push("skeleton".into());
    let skn = mesh::read_skn(&wad.read(mp.field("simpleSkin").and_then(|v| v.as_str()).context("simpleSkin")?)?)?;
    found.push("mesh".into());
    // the body texture, or the diffuse of the skin's material for newer skins
    let body = match mp.field("texture") {
        Some(t) => to_tex(texture::read_any(&read_link(wad, t)?)?),
        None => mp.field("material").and_then(|m| material_texture(wad, skin, m)).context("texture")?,
    };
    found.push("tex_body".into());
    let mut textures = vec![body];
    let mut submesh_tex: HashMap<String, u32> = HashMap::new();
    for ov in mp.field("materialOverride").map(|v| v.list()).unwrap_or(&[]) {
        let Some(sm) = ov.field("Submesh").and_then(|v| v.as_str()) else { continue };
        let tex = match (ov.field("texture"), ov.field("material")) {
            (Some(t), _) => read_link(wad, t).ok().and_then(|b| texture::read_any(&b).ok()).map(to_tex),
            (None, Some(m)) => material_texture(wad, skin, m),
            _ => None,
        };
        {
            if let Some(t) = tex {
                submesh_tex.insert(sm.to_ascii_lowercase(), textures.len() as u32);
                textures.push(t);
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

fn build_clips(wad: &Wad, anim_bin: &Bin, graph: u32, model: &Model) -> Result<(Vec<Clip>, Vec<String>)> {
    let graph = anim_bin.objects.iter().find(|o| o.path == graph).context("animation graph missing")?.as_value();
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

fn build_audio(main: &Wad, locale: Option<&Wad>, common: Option<&Wad>, audio_dir: &Path) -> Result<(BTreeMap<String, Vec<String>>, Vec<String>)> {
    let mut found = vec![];
    let sfx_events = audio::read_bnk(&main.read(
        sheet::ASSETS.iter().find(|a| a.id == "sfx_events").unwrap().locator,
    )?)?;
    let sfx_audio = audio::read_bnk(&main.read(sheet::ASSETS.iter().find(|a| a.id == "sfx_audio").unwrap().locator)?)?;
    found.push("sfx_events".into());
    found.push("sfx_audio".into());
    let item_bank = |id: &str| {
        let loc = sheet::ASSETS.iter().find(|a| a.id == id)?.locator;
        audio::read_bnk(&common?.read(loc).ok()?).ok()
    };
    let (items_events, items_audio) = (item_bank("items_sfx_events").unwrap_or_default(), item_bank("items_sfx_audio").unwrap_or_default());

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
        if s.bank == "items" {
            for w in audio::resolve_event(id, &[&items_events, &items_audio]) {
                if let Some(b) = items_audio.wems.get(&w) {
                    bytes.insert(w, b.clone());
                }
                wems.push(w);
            }
        } else if s.bank == "sfx" {
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

// ---------------------------------------------------------------- skins

fn skin_bin_path(n: u32) -> String {
    format!("data/characters/fiddlesticks/skins/skin{n}.bin")
}

fn skin_obj_path(n: u32) -> String {
    format!("Characters/FiddleSticks/Skins/Skin{n}")
}

/// The skin a chroma recolours, read from its mesh folder (Skins/Skin09/... -> 9, Skins/Base/... -> 0).
fn mesh_parent(skin: &Value, n: u32) -> u32 {
    let Some(p) = skin.field("skinMeshProperties").and_then(|m| m.field("simpleSkin")).and_then(|v| v.as_str()) else { return n };
    let lower = p.to_ascii_lowercase();
    let Some(rest) = lower.split("/skins/").nth(1) else { return n };
    if rest.starts_with("base/") {
        return 0;
    }
    rest.strip_prefix("skin").and_then(|r| r.split('/').next()).and_then(|d| d.parse().ok()).unwrap_or(n)
}

/// Skin names in the player's client language, from the League client's own champion data (champions/9.json).
fn client_skin_names(champions: &Path, locale: &str) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let Some(root) = champions.ancestors().nth(4) else { return out };
    let dir = root.join("Plugins/rcp-be-lol-game-data");
    let tries = [
        (dir.join(format!("{locale}-assets.wad")), format!("plugins/rcp-be-lol-game-data/global/{}/v1/champions/9.json", locale.to_ascii_lowercase())),
        (dir.join("default-assets.wad"), "plugins/rcp-be-lol-game-data/global/default/v1/champions/9.json".to_string()),
    ];
    for (wad, path) in tries {
        let Ok(text) = Wad::open(&wad).and_then(|w| w.read(&path)).map(|b| String::from_utf8_lossy(&b).into_owned()) else { continue };
        // every skin and chroma object is {"id":9NNN, ... "name":"..."}: take the first name after each id
        for (at, _) in text.match_indices("\"id\":") {
            let digits: String = text[at + 5..].chars().take_while(|c| c.is_ascii_digit()).collect();
            let Ok(id) = digits.parse::<u32>() else { continue };
            if !(9000..10000).contains(&id) || out.contains_key(&(id - 9000)) {
                continue;
            }
            let Some(np) = text[at..].find("\"name\":\"") else { continue };
            out.insert(id - 9000, json_string(&text[at + np + 8..]));
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

/// A JSON string body up to its closing quote, unescaped.
fn json_string(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => break,
            '\\' => match it.next() {
                Some('u') => {
                    let h: String = it.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some('n') => out.push(' '),
                Some(x) => out.push(x),
                None => break,
            },
            _ => out.push(c),
        }
    }
    out
}

/// Every skin and chroma in the install, with its client name and its round icon.
fn list_skins(wad: &Wad, champions: &Path, locale: &str, icons: &mut BTreeMap<String, Tex>) -> Vec<SkinInfo> {
    let names = client_skin_names(champions, locale);
    let mut out = vec![];
    for n in 0..100 {
        if wad.get(&skin_bin_path(n)).is_none() {
            continue;
        }
        let Ok(b) = wad.read(&skin_bin_path(n)).and_then(|d| bin::parse(&d)) else { continue };
        let Some(o) = obj(&b, &skin_obj_path(n)).map(|o| o.as_value()) else { continue };
        // iconCircle is the champion's for every skin; iconAvatar is the skin's own, chromas have a client picture
        let parent = mesh_parent(&o, n);
        let icon = (n != parent).then(|| chroma_picture(champions, n)).flatten().or_else(|| {
            ["iconAvatar", "iconCircle"]
                .iter()
                .find_map(|f| o.field(f).and_then(|v| read_link(wad, v).ok()).and_then(|b| texture::read_any(&b).ok()))
                .map(to_tex)
        });
        if let Some(t) = icon {
            icons.insert(format!("skin_icon_{n}"), t);
        }
        let fallback = o.field("championSkinName").and_then(|v| v.as_str()).unwrap_or("Fiddlesticks").to_string();
        out.push(SkinInfo { num: n, name: names.get(&n).cloned().unwrap_or(fallback), parent });
    }
    println!("skins: {}", out.iter().map(|s| format!("{} {}", s.num, s.name)).collect::<Vec<_>>().join(", "));
    out
}

/// `--skin N`: builds one skin's model, animations and sounds into cache/skins/skinN.bin.
fn prepare_skin(args: &[String], n: u32) -> Result<String> {
    let root = league_root(args)?;
    let champions = find_champions_dir(&root)?;
    let main_wad = Wad::open(&find_file(&champions, |f| f == "fiddlesticks.wad.client").context("FiddleSticks.wad.client not found")?)?;
    let locale_path = find_file(&champions, |f| f.starts_with("fiddlesticks.") && f.ends_with(".wad.client") && f != "fiddlesticks.wad.client");
    let locale_wad = match &locale_path {
        Some(p) => Some(Wad::open(p)?),
        None => None,
    };
    let locale = locale_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|f| f.to_string_lossy().split('.').nth(1).unwrap_or("").to_string())
        .unwrap_or_default();
    let out = arg_value(args, "--out").map(PathBuf::from).unwrap_or_else(cache_dir);
    std::fs::create_dir_all(out.join("skins"))?;
    std::fs::create_dir_all(out.join("audio"))?;

    let skin = bin::parse(&main_wad.read(&skin_bin_path(n))?)?;
    let mut found = vec![];
    let model = build_model(&main_wad, &skin, &skin_obj_path(n), &mut found)?;
    let so = obj(&skin, &skin_obj_path(n)).context("skin object missing")?.as_value();

    // its animation graph: the object the skin links to, in its own animations bin or the one it shares
    let graph = so.field("SkinAnimationProperties").and_then(|a| a.field("AnimationGraphData"));
    let graph_hash = match graph {
        Some(Value::ObjLink(h)) => *h,
        _ => fnv1a(&format!("Characters/FiddleSticks/Animations/Skin{n}")),
    };
    let parent = mesh_parent(&so, n);
    let mut clips = None;
    for m in [n, parent].into_iter().chain(0..100) {
        let Ok(b) = main_wad.read(&format!("data/characters/fiddlesticks/animations/skin{m}.bin")).and_then(|d| bin::parse(&d)) else { continue };
        if b.objects.iter().any(|o| o.path == graph_hash) {
            clips = Some(build_clips(&main_wad, &b, graph_hash, &model)?.0);
            break;
        }
    }
    let clips = clips.context("animation graph not found")?;
    let sounds = build_skin_audio(&main_wad, locale_wad.as_ref(), &locale, &so, &out.join("audio"))?;
    let name = client_skin_names(&champions, &locale).remove(&n).unwrap_or_default();
    let fx = skin_fx(&main_wad, n, parent, &name);

    let vfx = build_vfx(&main_wad, &skin, &so);
    let c = SkinCache { version: cache::CACHE_VERSION, num: n, model, clips, sounds, fx, vfx };
    let tmp = out.join(format!("skins/skin{n}.tmp"));
    std::fs::write(&tmp, cache::save_skin(&c)?)?;
    std::fs::rename(&tmp, out.join(format!("skins/skin{n}.bin")))?;
    Ok(format!(
        "skin {n} ready: {} vertices, {} textures, {} clips, {} sounds, own fx/icons: {:?}",
        c.model.vertices.len(),
        c.model.textures.len(),
        c.clips.len(),
        c.sounds.len(),
        c.fx.keys().collect::<Vec<_>>()
    ))
}

/// A skin's own sound banks (its SFX and, if it has one, its voice): each sheet sound is looked up under the
/// skin's event names (Play_vo_FiddlesticksSkin06_... for Play_vo_Fiddlesticks_...).
fn build_skin_audio(main: &Wad, locale_wad: Option<&Wad>, locale: &str, skin: &Value, audio_dir: &Path) -> Result<BTreeMap<String, Vec<String>>> {
    struct Unit {
        tag: String,
        events: audio::Bank,
        audio: Vec<audio::Bank>,
        wpk: HashMap<u32, Vec<u8>>,
    }
    let mut units: Vec<(bool, Unit)> = vec![];
    let banks = skin.field("SkinAudioProperties").and_then(|a| a.field("bankUnits")).map(|v| v.list()).unwrap_or(&[]);
    for b in banks {
        let name = b.field("name").and_then(|v| v.as_str()).unwrap_or("");
        let vo = name.ends_with("_VO");
        // Fiddlesticks_Skin06_VO -> FiddlesticksSkin06, Fiddlesticks_Base_SFX -> Fiddlesticks
        let stem = name.trim_end_matches("_VO").trim_end_matches("_SFX");
        let tag = stem.trim_end_matches("_Base").replace('_', "");
        let mut u = Unit { tag, events: audio::Bank::default(), audio: vec![], wpk: HashMap::new() };
        for p in b.field("bankPath").map(|v| v.list()).unwrap_or(&[]).iter().filter_map(|v| v.as_str()) {
            let (wad, path) = if vo {
                let Some(w) = locale_wad else { continue };
                (w, p.replace("/en_US/", &format!("/{}/", locale_case(locale))))
            } else {
                (main, p.to_string())
            };
            let Ok(data) = wad.read(&path) else { continue };
            if path.ends_with(".wpk") {
                if let Ok(w) = audio::read_wpk(&data) {
                    u.wpk.extend(w);
                }
            } else if let Ok(bank) = audio::read_bnk(&data) {
                if path.to_ascii_lowercase().ends_with("_events.bnk") {
                    u.events = bank;
                } else {
                    u.audio.push(bank);
                }
            }
        }
        units.push((vo, u));
    }

    let mut pending: Vec<(u32, PathBuf)> = vec![];
    let mut cues: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for s in sheet::SOUNDS {
        if s.bank == "items" {
            continue; // the same for every skin
        }
        let vo = s.bank != "sfx";
        let Some((_, u)) = units.iter().find(|(v, _)| *v == vo) else { continue };
        let event = s.event.replacen("_Fiddlesticks_", &format!("_{}_", u.tag), 1);
        let id = audio::wwise_id(&event);
        for bank in &u.audio {
            let wems = audio::resolve_event(id, &[&u.events, bank]);
            if wems.is_empty() {
                continue;
            }
            let mut have = vec![];
            for w in wems {
                let wav = audio_dir.join(format!("{w}.wav"));
                if !wav.exists() && !pending.iter().any(|(p, _)| *p == w) {
                    let Some(bytes) = u.wpk.get(&w).or_else(|| bank.wems.get(&w)) else { continue };
                    let p = audio_dir.join(format!("{w}.wem"));
                    std::fs::write(&p, bytes)?;
                    pending.push((w, p));
                }
                have.push(w);
            }
            if !have.is_empty() {
                cues.insert(s.id.to_string(), have);
            }
            break;
        }
    }
    decode_wems(&pending)?;
    for (_, p) in &pending {
        let _ = std::fs::remove_file(p);
    }
    Ok(cues
        .into_iter()
        .map(|(k, w)| (k, w.iter().filter(|w| audio_dir.join(format!("{w}.wav")).exists()).map(|w| format!("{w}.wav")).collect::<Vec<_>>()))
        .filter(|(_, f)| !f.is_empty())
        .collect())
}

/// es_es -> es_ES (League's sound folders)
fn locale_case(l: &str) -> String {
    match l.split_once('_') {
        Some((a, b)) => format!("{}_{}", a.to_ascii_lowercase(), b.to_ascii_uppercase()),
        None => l.to_string(),
    }
}

/// The diffuse texture of a skin material (StaticMaterialDef) in the skin's bin.
fn material_texture(wad: &Wad, skin: &Bin, link: &Value) -> Option<Tex> {
    let Value::ObjLink(h) = link else { return None };
    let m = skin.objects.iter().find(|o| o.path == *h)?;
    let samplers = m.field("samplerValues")?.list();
    // Main_Texture first: some materials (Star Nemesis' demon arms) keep a placeholder in Diffuse_Texture
    for want in ["Main_Texture", "Diffuse_Texture", "Diffuse_Color", "DiffuseTexture", "Diffuse", "MainTex"] {
        for s in samplers {
            if s.field("TextureName").and_then(|v| v.as_str()).map(|n| n.eq_ignore_ascii_case(want)).unwrap_or(false) {
                let t = s.field("texturePath")?;
                return read_link(wad, t).ok().and_then(|b| texture::read_any(&b).ok()).map(to_tex);
            }
        }
    }
    None
}

/// A chroma's picture from the League client (champion-chroma-images/9/9NNN.png) as a BGRA texture.
fn chroma_picture(champions: &Path, n: u32) -> Option<Tex> {
    let dir = champions.ancestors().nth(4)?.join("Plugins/rcp-be-lol-game-data");
    let path = format!("plugins/rcp-be-lol-game-data/global/default/v1/champion-chroma-images/9/{}.png", 9000 + n);
    let bytes = ["default-assets.wad", "default-assets2.wad"].iter().find_map(|w| Wad::open(&dir.join(w)).ok()?.read(&path).ok())?;
    png_tex(&bytes)
}

/// What a skin changes in the effects and HUD: its own versions of the base particle textures (same name under
/// its folder) and the ability icons in its colours.
fn skin_fx(wad: &Wad, n: u32, parent: u32, name: &str) -> BTreeMap<String, Tex> {
    let mut fx = BTreeMap::new();
    for a in sheet::ASSETS.iter().filter(|a| a.id.starts_with("fx_") && a.locator.contains("/Skins/Base/")) {
        for m in [n, parent] {
            let folder = format!("Skin{m:02}");
            let cand = a.locator.replace("/Skins/Base/", &format!("/Skins/{folder}/")).replace("_Base_", &format!("_{folder}_"));
            if let Some(t) = wad.read(&cand).ok().and_then(|b| texture::read_any(&b).ok()) {
                fx.insert(a.id.to_string(), to_tex(t));
                break;
            }
        }
    }
    if let Some(theme) = lolread::theme::for_skin(n, parent, name) {
        for id in ["icon_q", "icon_w", "icon_e", "icon_r"] {
            let Some(a) = sheet::ASSETS.iter().find(|a| a.id == id) else { continue };
            let Some(t) = wad.read(a.locator).ok().and_then(|b| texture::read_any(&b).ok()) else { continue };
            let Some(mut px) = texture::decode_rgba(t.format, t.width, t.height, &t.mips[0]) else { continue };
            for p in px.chunks_exact_mut(4) {
                let c = lolread::theme::recolor([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0], &theme);
                let q = c.map(|v| (v * 255.0).round() as u8);
                p[0] = q[2];
                p[1] = q[1];
                p[2] = q[0];
            }
            fx.insert(id.to_string(), Tex { format: texture::DXGI_BGRA8, width: t.width, height: t.height, mips: vec![px] });
        }
    }
    fx
}

// ---------------------------------------------------------------- League particle systems

/// A vfx_events key as League hashes it: a name (FNV-1a, lowercase) or "#hex".
fn vfx_key(k: &str) -> u32 {
    match k.strip_prefix('#') {
        Some(h) => u32::from_str_radix(h, 16).unwrap_or(0),
        None => fnv1a(k),
    }
}

/// The particle systems a skin uses for the vfx_events sheet (and their child systems), with their textures and
/// meshes, read through the skin's resource map from its bin and the bins it depends on.
fn build_vfx(wad: &Wad, skin: &Bin, skin_obj: &Value) -> lolread::vfx::VfxLib {
    use lolread::vfx::{Child, Prim};
    let mut lib = lolread::vfx::VfxLib::default();
    let mut bins: Vec<Bin> = skin.deps.iter().filter_map(|d| wad.read(&d.to_ascii_lowercase()).ok().and_then(|b| bin::parse(&b).ok())).collect();
    bins.push(Bin { deps: vec![], objects: skin.objects.iter().map(|o| bin::Object { path: o.path, class: o.class, fields: o.fields.clone() }).collect() });
    let Some(Value::ObjLink(res)) = skin_obj.field("mResourceResolver") else { return lib };
    let Some(resolver) = bins.iter().flat_map(|b| b.objects.iter()).find(|o| o.path == *res) else { return lib };
    let mut all_keys: HashMap<u32, u32> = HashMap::new();
    if let Some(Value::Map(m)) = resolver.field("resourceMap") {
        for (k, v) in m {
            if let (Value::Hash(k), Value::ObjLink(p)) = (k, v) {
                all_keys.insert(*k, *p);
            }
        }
    }
    let defs: HashMap<u32, &bin::Object> = lolread::vfx::systems_in(&bins).map(|o| (o.path, o)).collect();

    // the sheet's keys, then every child they spawn
    let mut todo: Vec<u32> = sheet::VFX_EVENTS.iter().filter_map(|e| all_keys.get(&vfx_key(e.key)).copied()).collect();
    for e in sheet::VFX_EVENTS {
        if let Some(p) = all_keys.get(&vfx_key(e.key)) {
            lib.keys.insert(vfx_key(e.key), *p);
        }
    }
    while let Some(path) = todo.pop() {
        if lib.systems.contains_key(&path) {
            continue;
        }
        let Some(o) = defs.get(&path) else { continue };
        let sys = lolread::vfx::system(o, &format!("{path:08x}"));
        for em in &sys.emitters {
            for c in &em.children {
                match c {
                    Child::Key(k) => {
                        if let Some(p) = all_keys.get(k) {
                            lib.keys.insert(*k, *p);
                            todo.push(*p);
                        }
                    }
                    Child::System(p) => todo.push(*p),
                }
            }
        }
        lib.systems.insert(path, sys);
    }

    // textures and meshes they draw with
    let mut tex_paths: Vec<String> = vec![];
    let mut mesh_names: Vec<(String, String)> = vec![];
    for sys in lib.systems.values() {
        for em in &sys.emitters {
            tex_paths.push(em.texture.clone());
            if let Some(m) = &em.mult {
                tex_paths.push(m.texture.clone());
            }
            if let Some(e) = &em.erosion {
                tex_paths.push(e.map.clone());
            }
            if let Prim::Mesh { mesh } = &em.prim {
                let dir = em.texture.rsplit_once('/').map(|d| d.0).unwrap_or("ASSETS/Characters/Fiddlesticks/Skins/Base/Particles");
                mesh_names.push((mesh.clone(), dir.to_string()));
            }
        }
    }
    for p in tex_paths {
        if p.is_empty() || lib.textures.contains_key(&p) {
            continue;
        }
        if let Some(t) = wad.read(&p.to_ascii_lowercase()).ok().and_then(|b| texture::read_any(&b).ok()) {
            lib.textures.insert(p, to_tex(t));
        }
    }
    for (name, dir) in mesh_names {
        if lib.meshes.contains_key(&name) {
            continue;
        }
        let cands = if name.contains('/') {
            vec![name.clone()]
        } else {
            vec![format!("{dir}/{name}"), format!("ASSETS/Characters/Fiddlesticks/Skins/Base/Particles/{name}")]
        };
        if let Some(m) = cands.iter().find_map(|c| wad.read(&c.to_ascii_lowercase()).ok().and_then(|b| lolread::vfx::read_scb(&b).ok())) {
            lib.meshes.insert(name, m);
        }
    }
    let emitters: usize = lib.systems.values().map(|s| s.emitters.len()).sum();
    println!("vfx: {} keys, {} systems, {emitters} emitters, {} textures, {} meshes", lib.keys.len(), lib.systems.len(), lib.textures.len(), lib.meshes.len());
    lib
}

/// A PNG as a BGRA texture.
fn png_tex(bytes: &[u8]) -> Option<Tex> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let ch = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        _ => 1,
    };
    let mut px = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let p = &buf[i * ch..i * ch + ch];
        let (r, g, b, a) = match ch {
            4 => (p[0], p[1], p[2], p[3]),
            3 => (p[0], p[1], p[2], 255),
            2 => (p[0], p[0], p[0], p[1]),
            _ => (p[0], p[0], p[0], 255),
        };
        px.extend_from_slice(&[b, g, r, a]);
    }
    Some(Tex { format: texture::DXGI_BGRA8, width: w as u32, height: h as u32, mips: vec![px] })
}

/// The items sheet's League items from the player's client (es): name, stat lines, passive text, AP and ability
/// haste, and their icons (as item_<id> in `icons`).
fn build_items(champions: &Path, locale: &str, icons: &mut BTreeMap<String, Tex>) -> Vec<ItemInfo> {
    let mut out = vec![];
    let Some(root) = champions.ancestors().nth(4) else { return out };
    let dir = root.join("Plugins/rcp-be-lol-game-data");
    let tries = [
        (dir.join(format!("{locale}-assets.wad")), format!("plugins/rcp-be-lol-game-data/global/{}/v1/items.json", locale.to_ascii_lowercase())),
        (dir.join("default-assets.wad"), "plugins/rcp-be-lol-game-data/global/default/v1/items.json".to_string()),
    ];
    let Some(text) = tries.iter().find_map(|(w, p)| Wad::open(w).ok()?.read(p).ok()).map(|b| String::from_utf8_lossy(&b).into_owned()) else { return out };
    let wads: Vec<Wad> = ["default-assets2.wad", "default-assets.wad"].iter().filter_map(|w| Wad::open(&dir.join(w)).ok()).collect();
    for row in sheet::ITEMS {
        // the object {"id":<id>,"name":"...","description":"...", ... "iconPath":"..."}
        let Some(at) = text.find(&format!("{{\"id\":{},", row.id)) else { continue };
        let obj = &text[at..];
        let field = |k: &str| obj.find(&format!("\"{k}\":\"")).map(|i| json_string(&obj[i + k.len() + 4..])).unwrap_or_default();
        let name = field("name");
        let desc = field("description");
        let stats_part = desc.split("<stats>").nth(1).and_then(|x| x.split("</stats>").next()).unwrap_or("");
        let stat_lines: Vec<String> = stats_part.split("<br>").map(strip_tags).map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        let value_of = |what: &str| -> f32 {
            stat_lines.iter().find(|l| l.contains(what)).and_then(|l| l.split_whitespace().next()).and_then(|n| n.trim_end_matches('%').parse().ok()).unwrap_or(0.0)
        };
        let rest = desc.split("</stats>").nth(1).unwrap_or(&desc);
        // passive / active names that open a line become titles (marked with § for the HUD); quoted mid-sentence
        // they stay plain words
        let marked = title_names(rest);
        let text_plain = strip_tags(&marked).lines().map(str::trim).filter(|l| !l.is_empty() && *l != "§").collect::<Vec<_>>().join("\n");
        let icon_path = field("iconPath").to_ascii_lowercase().replace("/lol-game-data/assets/", "plugins/rcp-be-lol-game-data/global/default/");
        if let Some(t) = wads.iter().find_map(|w| w.read(&icon_path).ok()).and_then(|b| png_tex(&b)) {
            icons.insert(format!("item_{}", row.id), t);
        }
        out.push(ItemInfo { id: row.id, name, ap: value_of("de poder de habilidad"), haste: value_of("de velocidad de habilidad"), stat_lines, text: text_plain });
    }
    // League's stat icons (the client's stat shard pictures) for the item cards and the stats box
    for (key, file) in [("stat_as", "statmodsattackspeedicon"), ("stat_haste", "statmodscdrscalingicon"), ("stat_hp", "statmodshealthscalingicon"), ("stat_armor", "statmodsarmoricon"), ("stat_mr", "statmodsmagicresicon"), ("stat_ms", "statmodsmovementspeedicon"), ("stat_adaptive", "statmodsadaptiveforceicon")] {
        let path = format!("plugins/rcp-be-lol-game-data/global/default/v1/perk-images/statmods/{file}.png");
        if let Some(t) = wads.iter().find_map(|w| w.read(&path).ok()).and_then(|b| png_tex(&b)) {
            icons.insert(key.to_string(), t);
        }
    }
    println!("items: {}", out.iter().map(|i| format!("{} ({} AP, {} haste)", i.name, i.ap, i.haste)).collect::<Vec<_>>().join(", "));
    out
}

/// Text without its <tags>.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in s.chars() {
        match c {
            '<' => inside = true,
            '>' => inside = false,
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

/// Item text with each <passive>/<active> name that starts a line set apart as "§name" on its own line.
fn title_names(desc: &str) -> String {
    let mut out = String::new();
    let mut rest = desc;
    while let Some(i) = rest.find(['<']) {
        let (before, tail) = rest.split_at(i);
        out.push_str(before);
        let line_start = out.trim_end_matches(' ').is_empty() || out.trim_end_matches(' ').ends_with('\n');
        let tag = ["<passive>", "<active>"].into_iter().find(|t| tail.starts_with(t));
        let close = tag.map(|t| t.replace('<', "</"));
        match (tag, close.as_deref().and_then(|c| tail.find(c).map(|e| (c, e)))) {
            (Some(t), Some((c, e))) if line_start => {
                out.push('§');
                out.push_str(&tail[t.len()..e]);
                out.push('\n');
                rest = &tail[e + c.len()..];
            }
            _ if tail.starts_with("<br>") => {
                out.push('\n');
                rest = &tail[4..];
            }
            _ => {
                out.push('<');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
