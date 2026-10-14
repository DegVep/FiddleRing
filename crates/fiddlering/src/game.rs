//! Fiddlesticks in the Lands Between: reads the Tarnished, the camera and nearby characters each frame, runs his
//! kit (abilities sheet, numbers from the player's League data), League levelling, and hands the frame to
//! the renderer. Runs on the game thread from Elden Ring's own task loop.

use crate::anim::{self, Anim};
use crate::audio::Audio;
use crate::font;
use crate::log;
use crate::scene::{ModelDraw, SCENE, Scene, Sprite, quad_uv};
use crate::vfx::{Follow, Frame, Vfx};
use eldenring::cs::{CSCamera, ChrIns, WorldChrMan};
use fromsoftware_shared::FromStatic;
use lolread::cache::{Cache, Spell};
use lolread::math::M4;
use lolread::sheet::{self, tuning};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_DOWN, VK_F2, VK_F3, VK_LEFT, VK_MENU, VK_RETURN, VK_RIGHT, VK_UP};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

type V3 = [f32; 3];

/// Fiddlesticks' ability buttons captured this frame (ChrIns_PreBehaviorSafe), consumed by the game tick.
static ABILITY_ACTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

const fn bits(list: &[u32]) -> u64 {
    let mut m = 0u64;
    let mut i = 0;
    while i < list.len() {
        m |= 1 << list[i];
        i += 1;
    }
    m
}
const ABILITY_BITS: u64 = bits(sheet::ABILITY_ACTION_BITS);


const SLOTS: [&str; 5] = ["aa", "q", "w", "e", "r"];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn len(a: V3) -> f32 {
    (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
}
fn flat(a: V3) -> f32 {
    (a[0] * a[0] + a[2] * a[2]).sqrt()
}
fn norm(a: V3) -> V3 {
    let l = len(a).max(1e-5);
    mul(a, 1.0 / l)
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// A character this frame (pointer valid only during the tick it was read in).
#[derive(Clone, Copy)]
struct Chr {
    ptr: usize,
    pos: V3,
    height: f32,
    hp: i32,
    max_hp: i32,
}

fn chr_mut(p: usize) -> &'static mut ChrIns {
    unsafe { &mut *(p as *mut ChrIns) }
}

fn read_chr(c: &ChrIns) -> Chr {
    let ph = &c.modules.physics;
    let d = &c.modules.data;
    Chr {
        ptr: c as *const ChrIns as usize,
        pos: [ph.position.0, ph.position.1, ph.position.2],
        height: ph.hit_height.clamp(0.5, 12.0),
        hp: d.hp,
        max_hp: d.max_hp,
    }
}

struct Missile {
    id: usize,
    group: &'static str,
    slot: usize,
    pos: V3,
    dir: V3,
    target: usize,
    rank: u32,
}

struct Cc {
    ptr: usize,
    until: f32,
    fear: bool,
    silence: bool,
    slow: f32,
}

struct Drain {
    targets: Vec<usize>,
    t: f32,
    tick: f32,
    rank: u32,
}

enum Ult {
    Channel { t: f32, rank: u32, target: Option<usize> },
    /// the jump after the channel
    Leap { t: f32, rank: u32, from: V3, to: V3 },
    Storm { t: f32, tick: f32, rank: u32 },
}

struct Pending {
    slot: usize,
    t: f32,
    target: Option<usize>,
    point: V3,
    rank: u32,
    /// basic attack at melee range: lands at the end of the windup, no missile
    melee: bool,
}

pub struct Game {
    c: Arc<Cache>,
    /// the base skin's cache (skins replace model, clips and some sounds of a copy of it)
    base: Arc<Cache>,
    /// skin worn (League skin number)
    skin: u32,
    /// skin picker open: (row in the skin list, chroma: 0 = the skin itself)
    skin_menu: Option<(usize, usize)>,
    skin_job: Option<(u32, std::sync::mpsc::Receiver<bool>)>,
    skin_note: Option<(String, f32)>,
    menu_keys: [bool; 6],
    /// the Elden Ring character whose progress is loaded (save slot + name)
    save_key: Option<String>,
    /// hostiles near him this frame, for their health bars
    hostile_ptrs: Vec<usize>,
    /// health bar damage trails: (shown share, last hit clock) per character
    bar_trails: HashMap<usize, (f32, f32)>,
    /// ability power this frame (runes + items), for the stats box
    ap_total: f32,
    /// League items owned (ids), offered three at a time when a boss dies
    items: Vec<u32>,
    offer: Option<([u32; 3], usize)>,
    /// F3: the items window, on the slot shown
    inv_open: Option<usize>,
    item_keys: [bool; 4],
    /// bosses whose health bar showed, a boss died and when the bars cleared
    boss_ptrs: HashSet<usize>,
    boss_died: bool,
    /// last strong (team 7) enemy we killed, last full-screen banner the game asked for, and its id
    strong_kill_at: f32,
    banner_at: f32,
    banner_id: i32,
    bars_clear_at: f32,
    /// item burns: (character, until, share of max HP per second, carried damage)
    burns: Vec<(usize, f32, f32, f32)>,
    /// the model's yaw, easing towards where he faces
    model_yaw: f32,
    /// League particle systems playing, and the lasting ones by (vfx_events id, missile id / character)
    vfx: Vfx,
    vfx_live: HashMap<(&'static str, usize), u64>,
    /// a chroma's colours over its skin's League effects
    vfx_theme: Option<lolread::theme::Theme>,
    /// characters that exist this frame (pointers safe to read)
    alive_ptrs: HashSet<usize>,
    snap_pos: V3,
    snap_fwd: V3,
    /// the worn skin's colours for the effects
    theme: Option<lolread::theme::Theme>,
    /// CC immunity: poise written last frame, the poise he would really have left, last time it was hit
    poise_written: f32,
    real_poise: f32,
    poise_hit_at: f32,
    poise_clamp_logged: bool,
    audio: Option<Audio>,
    anim: Anim,
    level: u32,
    xp: f32,
    /// skill points spent in Q W E R (index 1..4), chosen by the player like in League
    ranks: [u32; 5],
    cd: [f32; 5],
    prev_actions: u64,
    prev_keys: [bool; 12],
    last_pos: Option<V3>,
    speed: f32,
    dead: bool,
    damaged: HashMap<usize, i32>,
    cc: Vec<Cc>,
    missiles: Vec<Missile>,
    fx: crate::fx::Fx,
    next_missile: usize,
    drain: Option<Drain>,
    ult: Option<Ult>,
    pending: Option<Pending>,
    face: Option<(V3, f32)>,
    clock: f32,
    last: Instant,
    moved_once: bool,
    test: bool,
    test_t: f32,
    test_spawn_t: f32,
    test_spawns: u32,
    /// test runs: a screenshot taken at this clock time
    shot_at: Option<(String, f32)>,
    test_next: usize,
    status_t: f32,
    save_t: f32,
    dirty: bool,
    kills: u32,
    seen_teams: HashSet<u8>,
    /// the closest non-player characters this frame (chr id, team, hp, 3D and flat distance), logged when a cast finds no target
    near: Vec<(u32, u8, i32, f32, f32)>,
    no_target_logged: f32,
    /// the Tarnished is slowed by our Bountiful Harvest channel (restored once when it ends)
    w_slowed: bool,
    /// an enemy is inside Q's / W's range (their slots are dark and they cannot be cast otherwise)
    in_range: [bool; 5],
    frame: u64,
    hidden_logged: bool,
    /// the game draws the Tarnished (false during cutscenes)
    player_drawn: bool,
    /// the HUD stays hidden until then (dialogue)
    hud_hidden_until: f32,
    dt: f32,
    ui_seen: u128,
    hud_seen: i32,
    last_close: f32,
    dbg_last: u64,
}

/// Fiddlesticks' level, xp and ranks for one Elden Ring character (save slot + name).
fn progress_path(key: &str) -> std::path::PathBuf {
    crate::log::data_dir().join(format!("progress_{key}.txt"))
}

/// `level xp rQ rW rE rR [items...]`
fn read_progress(path: &std::path::Path) -> Option<(u32, f32, [u32; 5], Vec<u32>)> {
    let t = std::fs::read_to_string(path).ok()?;
    let mut it = t.split_whitespace();
    let level = it.next()?.parse::<u32>().ok()?.clamp(1, 18);
    let xp = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let mut ranks = [1, 0, 0, 0, 0];
    for r in ranks.iter_mut().skip(1) {
        *r = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    }
    let items = it.filter_map(|v| v.parse().ok()).take(6).collect();
    Some((level, xp, ranks, items))
}

fn spell<'a>(c: &'a Cache, slot: usize) -> Option<&'a Spell> {
    c.spells.get(SLOTS[slot])
}

/// League stores Mana per rank from index 0; DataValues and cooldowns are indexed by rank.
fn val(c: &Cache, slot: usize, name: &str, rank: u32) -> f32 {
    let Some(s) = spell(c, slot) else { return 0.0 };
    let r = if name == "Mana" { rank.saturating_sub(1) } else { rank };
    s.at(name, r)
}

fn growth(level: u32) -> f32 {
    let l = level.saturating_sub(1) as f32;
    l * (0.7025 + 0.0175 * l)
}

fn ability(slot: usize) -> &'static sheet::Ability {
    &sheet::ABILITIES[sheet::ABILITIES.iter().position(|a| a.id == SLOTS[slot]).unwrap_or(0)]
}

/// CC immunity: the game's own super armor (the flag its heavy attacks set from their animations) and full poise,
/// written before the game updates the character and again each frame, so hits never stagger him.
fn hold_super_armor(c: &mut ChrIns) {
    c.modules.action_flag.action_modifiers_flags.set_super_armor(true);
    // the last hit's stagger size: none (boss hits like Morgott's go through super armor otherwise)
    c.modules.action_flag.damage_level = 0;
    let sa = &mut c.modules.super_armor;
    if sa.sa_durability < sa.sa_durability_max {
        sa.sa_durability = sa.sa_durability_max;
    }
}

fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// Esc as the game's own "back" (works for pad players too).
fn press_escape() {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: 0x01,
                dwFlags: KEYEVENTF_SCANCODE | if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe {
        SendInput(&[key(false)], std::mem::size_of::<INPUT>() as i32);
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(60));
        unsafe {
            SendInput(&[key(true)], std::mem::size_of::<INPUT>() as i32);
        }
    });
    log!("closed a blocked Elden Ring screen");
}

fn game_focused() -> bool {
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        pid == std::process::id()
    }
}

/// Elden Ring actions Fiddlesticks takes away (er_actions sheet): attacks and spells (his kit uses those buttons),
/// dodging, items and flasks, weapon/item switching, guarding, gestures. Jumping, sprinting and Torrent stay.
const BLOCKED: u64 = bits(sheet::BLOCKED_ACTION_BITS);
const ACTION_BIT: [u32; 5] = [0, 1, 2, 3, 28];

impl Game {
    pub fn new(c: Arc<Cache>) -> Self {
        let audio = match Audio::new(&c, crate::log::data_dir().join("cache").join("audio")) {
            Ok(a) => Some(a),
            Err(e) => {
                log!("audio: XAudio2 unavailable: {e:?}");
                None
            }
        };
        let (mut level, xp, mut ranks) = (1u32, 0f32, [1u32, 0, 0, 0, 0]);
        // progress belongs to an Elden Ring character: it is loaded once the game says which one (switch_save)
        let test = std::env::var_os("FIDDLERING_TEST").is_some() || crate::log::data_dir().join("test.txt").exists();
        if test {
            level = 18;
            ranks = [1, 0, 0, 0, 0];
            for l in sheet::LEVELS {
                if let Some(s) = SLOTS.iter().position(|s| *s == l.rank_up) {
                    ranks[s] += 1;
                }
            }
            log!("TEST mode: level 18, abilities cast by themselves at the nearest enemy");
        }
        log!("champion level {level}, xp {xp:.0}");
        let mut g = Game {
            base: c.clone(),
            skin: 0,
            skin_menu: None,
            skin_job: None,
            skin_note: None,
            menu_keys: [false; 6],
            save_key: None,
            hostile_ptrs: vec![],
            bar_trails: HashMap::new(),
            ap_total: 0.0,
            items: vec![],
            offer: None,
            inv_open: None,
            item_keys: [false; 4],
            boss_ptrs: HashSet::new(),
            boss_died: false,
            strong_kill_at: -100.0,
            banner_at: -100.0,
            banner_id: 0,
            bars_clear_at: 0.0,
            burns: vec![],
            model_yaw: 0.0,
            vfx: Vfx::new(),
            vfx_live: HashMap::new(),
            vfx_theme: None,
            alive_ptrs: HashSet::new(),
            snap_pos: [0.0; 3],
            snap_fwd: [0.0, 0.0, 1.0],
            theme: None,
            poise_written: -1.0,
            real_poise: 0.0,
            poise_hit_at: -100.0,
            poise_clamp_logged: false,
            c,
            audio,
            anim: Anim::new(),
            level,
            xp,
            ranks,
            cd: [0.0; 5],
            prev_actions: 0,
            prev_keys: [false; 12],
            last_pos: None,
            speed: 0.0,
            dead: false,
            damaged: HashMap::new(),
            cc: vec![],
            missiles: vec![],
            fx: crate::fx::Fx::new(),
            next_missile: 0,
            drain: None,
            ult: None,
            pending: None,
            face: None,
            clock: 0.0,
            last: Instant::now(),
            moved_once: false,
            test,
            test_t: 0.0,
            test_spawn_t: 0.0,
            test_spawns: 0,
            shot_at: None,
            test_next: 1,
            status_t: 0.0,
            save_t: 0.0,
            dirty: false,
            kills: 0,
            seen_teams: HashSet::new(),
            near: Vec::new(),
            no_target_logged: -10.0,
            w_slowed: false,
            in_range: [true; 5],
            frame: 0,
            player_drawn: true,
            hud_hidden_until: 0.0,
            hidden_logged: false,
            dt: 0.0,
            ui_seen: 0,
            hud_seen: -2,
            last_close: 0.0,
            dbg_last: 0,
        };
        if let Some(n) = std::fs::read_to_string(skin_choice_path()).ok().and_then(|t| t.trim().parse::<u32>().ok()) {
            g.choose_skin(n);
        }
        g
    }

    fn stat(&self, k: &str) -> f32 {
        self.c.stats.get(k).copied().unwrap_or(0.0)
    }

    fn rank(&self, slot: usize) -> u32 {
        if slot == 0 { 1 } else { self.ranks[slot] }
    }

    fn skill_points(&self) -> u32 {
        self.level.saturating_sub(self.ranks[1..].iter().sum())
    }

    /// League's rules: a free point, below max rank, basic abilities at most ceil(level/2), R at 6/11/16.
    fn can_rank(&self, slot: usize) -> bool {
        let ab = ability(slot);
        let r = self.ranks[slot];
        if slot == 0 || self.skill_points() == 0 || r >= ab.max_rank || self.level < ab.unlock_level {
            return false;
        }
        match ab.rank_rule.strip_prefix("levels:") {
            Some(list) => list.split(',').filter_map(|v| v.parse::<u32>().ok()).filter(|&l| l <= self.level).count() as u32 > r,
            None => ab.rank_rule == "half_level" && r < self.level.div_ceil(2),
        }
    }

    fn rank_up(&mut self, slot: usize) -> bool {
        if !self.can_rank(slot) {
            return false;
        }
        self.ranks[slot] += 1;
        self.dirty = true;
        log!("RANK UP {} -> {} ({} point(s) left)", SLOTS[slot], self.ranks[slot], self.skill_points());
        if let Some(p) = self.last_pos {
            self.fx.burst("aa_missile", "hit", [p[0], p[1] + 1.2, p[2]], [0.0, 1.0, 0.0], 0.0);
        }
        true
    }

    fn sound(&mut self, id: &str) {
        if id == "none" {
            return;
        }
        if let Some(a) = self.audio.as_mut() {
            a.play(id);
        }
    }

    fn save(&mut self) {
        if self.test || !self.dirty {
            return;
        }
        let Some(key) = &self.save_key else { return };
        let r = &self.ranks;
        let items: String = self.items.iter().map(|i| format!(" {i}")).collect();
        let _ = std::fs::write(progress_path(key), format!("{} {:.1} {} {} {} {}{items}\n", self.level, self.xp, r[1], r[2], r[3], r[4]));
        self.dirty = false;
    }

    /// Another Elden Ring character was loaded: keep the last one's progress and take this one's (level 1 if new).
    fn switch_save(&mut self, key: String, er_level: u32) {
        if self.save_key.is_some() {
            self.dirty = true;
            self.save();
        }
        let path = progress_path(&key);
        let mut loaded = read_progress(&path);
        // progress kept before it was per character: it goes to the first well-advanced character loaded
        let old = crate::log::data_dir().join("progress.txt");
        if loaded.is_none() && er_level > 15 && old.exists() {
            loaded = read_progress(&old);
            let _ = std::fs::rename(&old, crate::log::data_dir().join("progress_migrated.txt"));
            log!("progress.txt moved to character {key}");
        }
        if !self.test {
            let (level, xp, ranks, items) = loaded.unwrap_or((1, 0.0, [1, 0, 0, 0, 0], vec![]));
            self.items = items;
            self.level = level;
            self.xp = xp;
            self.ranks = ranks;
            self.cd = [0.0; 5];
        }
        log!("SAVE {key}: champion level {}, xp {:.0}", self.level, self.xp);
        self.save_key = Some(key);
        self.dirty = true;
        self.save();
    }

    fn gain_xp(&mut self, amount: f32) {
        if self.level >= 18 {
            return;
        }
        self.xp += amount;
        self.dirty = true;
        while self.level < 18 {
            let next = &sheet::LEVELS[self.level as usize];
            if self.xp < next.xp_total {
                break;
            }
            self.level = next.level;
            log!("LEVEL UP {} ({} skill point(s) to spend)", self.level, self.skill_points());
        }
    }

    /// Damages an Elden Ring character; HP 0 lets the game run its own death (runes, drops, flags).
    fn damage(&mut self, ch: Chr, amount: f32, what: &str) -> f32 {
        let c = chr_mut(ch.ptr);
        let hp = c.modules.data.hp;
        if hp <= 0 {
            return 0.0;
        }
        // League items on ability hits: more damage, burns, slows
        let mut amount = amount;
        if matches!(what, "Q" | "W" | "E" | "R") && !self.items.is_empty() {
            amount *= 1.0 + self.item_effect("dmg_mult");
            if (hp as f32) < 0.4 * c.modules.data.max_hp.max(1) as f32 {
                amount *= 1.0 + self.item_effect("low_hp_mult");
            }
            let burn = self.item_effect("burn");
            if burn > 0.0 {
                let until = self.clock + 3.0;
                match self.burns.iter_mut().find(|b| b.0 == ch.ptr) {
                    Some(b) => b.1 = until,
                    None => self.burns.push((ch.ptr, until, burn, 0.0)),
                }
            }
            let slow = self.item_effect("slow");
            if slow > 0.0 {
                self.apply_cc(ch.ptr, 1.0, false, false, slow);
            }
        }
        let c = chr_mut(ch.ptr);
        let dmg = (amount * tuning::DAMAGE_SCALE).max(1.0).round() as i32;
        let new = (hp - dmg).max(0);
        c.modules.data.hp = new;
        self.damaged.insert(ch.ptr, c.modules.data.max_hp);
        log!("HIT {what} chr={} team={} hp {hp}->{new} ({dmg})", c.character_id, c.team_type);
        (hp - new) as f32
    }

    fn apply_cc(&mut self, ptr: usize, dur: f32, fear: bool, silence: bool, slow: f32) {
        self.cc.push(Cc { ptr, until: self.clock + dur, fear, silence, slow });
    }

    /// Runs right after Elden Ring turned the pad/keys into action requests: keep Fiddlesticks' buttons for his
    /// kit and erase them everywhere the Tarnished's behaviour would read them.
    pub fn capture_ability_inputs() {
        unsafe {
            if let Ok(w) = WorldChrMan::instance_mut() {
                if let Some(p) = w.main_player.as_deref_mut() {
                    hold_super_armor(&mut p.chr_ins);
                    let ar: &mut eldenring::cs::CSChrActionRequestModule = &mut p.chr_ins.modules.action_request;
                    ABILITY_ACTIONS.fetch_or((ar.action_requests.0 | ar.new_action_presses.0) & ABILITY_BITS, std::sync::atomic::Ordering::Relaxed);
                    for a in [
                        &mut ar.action_requests,
                        &mut ar.new_action_presses,
                        &mut ar.queued_action_inputs,
                        &mut ar.cancel_ready_actions,
                        &mut ar.readback_new_presses,
                        &mut ar.readback_queued_inputs,
                        &mut ar.readback_cancel_ready,
                    ] {
                        a.0 &= !ABILITY_BITS;
                    }
                }
            }
        }
    }

    /// Elden Ring's HUD is hidden (it would also hide enemies' health), so the mod draws League-style health bars:
    /// over each wounded enemy near him (screen space, constant size, fading with distance) and a wide one at the
    /// top for a boss. Lost health lingers in a light damage trail that drains after a moment, like League.
    fn draw_health_bars(&mut self, me: &Chr, cam: &M4, fov: f32, near: f32, far: f32, scene: &mut Scene) {
        if tuning::HIDE_ER_BARS <= 0.5 {
            return;
        }
        let aspect = f32::from_bits(crate::scene::ASPECT.load(std::sync::atomic::Ordering::Relaxed)).clamp(0.5, 4.0);
        let view_proj = lolread::math::mul(&lolread::math::inverse(cam), &crate::gfx::perspective(fov, aspect, near.max(0.05), far.max(100.0)));
        // world -> HUD reference frame (1080 high, centred)
        let to_screen = |p: V3| -> Option<(f32, f32)> {
            let v = [p[0], p[1], p[2], 1.0];
            let c: Vec<f32> = (0..4).map(|k| (0..4).map(|r| v[r] * view_proj[r * 4 + k]).sum()).collect();
            if c[3] <= 0.1 {
                return None;
            }
            let (nx, ny) = (c[0] / c[3], c[1] / c[3]);
            if nx.abs() > 1.1 || ny.abs() > 1.1 {
                return None;
            }
            Some((960.0 + nx * 540.0 * aspect, 540.0 - ny * 540.0))
        };
        let rect = |x: f32, y: f32, w: f32, h: f32, color: [f32; 4]| Sprite {
            tex: "white",
            additive: false,
            pos: [[x, y, 0.], [x + w, y, 0.], [x + w, y + h, 0.], [x, y + h, 0.]],
            uv: quad_uv(),
            color,
        };
        let dt = self.dt;
        let clock = self.clock;
        // one bar: rim, dark well, damage trail, gradient fill, top sheen, 10% ticks
        let draw_bar = |out: &mut Vec<Sprite>, trails: &mut HashMap<usize, (f32, f32)>, key: usize, x: f32, y: f32, w: f32, h: f32, frac: f32, alpha: f32| {
            let tr = trails.entry(key).or_insert((frac, clock));
            if frac >= tr.0 {
                *tr = (frac, clock);
            } else if clock - tr.1 > 0.45 {
                // the trail waits a moment after a hit, then drains
                tr.0 = (tr.0 - dt * 0.6).max(frac);
            }
            let trail = tr.0;
            let a = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * alpha];
            out.push(rect(x - 2.0, y - 2.0, w + 4.0, h + 4.0, a([0.0, 0.0, 0.0, 0.9])));
            out.push(rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, a([0.36, 0.30, 0.20, 1.0])));
            out.push(rect(x, y, w, h, a([0.06, 0.03, 0.03, 0.95])));
            out.push(rect(x, y, w * trail, h, a([0.95, 0.80, 0.55, 0.85])));
            let n = 5;
            for i in 0..n {
                let t = i as f32 / (n - 1) as f32;
                out.push(rect(x, y + h * i as f32 / n as f32, w * frac, h / n as f32 + 0.5, a([0.95 - 0.40 * t, 0.22 - 0.12 * t, 0.18 - 0.10 * t, 1.0])));
            }
            out.push(rect(x, y, w * frac, (h * 0.18).max(1.0), a([1.0, 0.75, 0.70, 0.55])));
            for i in 1..10 {
                let tx = x + w * i as f32 / 10.0;
                if tx < x + w * frac {
                    out.push(rect(tx, y, 1.0, h * 0.55, a([0.0, 0.0, 0.0, 0.55])));
                }
            }
        };
        let mut trails = std::mem::take(&mut self.bar_trails);
        let mut boss: Option<(usize, Chr)> = None;
        for &p in &self.hostile_ptrs {
            if !self.alive_ptrs.contains(&p) {
                continue;
            }
            let ch = read_chr(chr_mut(p));
            if ch.hp <= 0 || ch.max_hp <= 0 {
                continue;
            }
            if self.boss_ptrs.contains(&p) {
                boss = Some((p, ch));
                continue;
            }
            let dist = len(sub(ch.pos, me.pos));
            if ch.hp >= ch.max_hp || dist > 40.0 {
                continue;
            }
            let Some((sx, sy)) = to_screen(add(ch.pos, [0.0, ch.height + 0.3, 0.0])) else { continue };
            let alpha = (1.0 - (dist - 25.0) / 15.0).clamp(0.0, 1.0);
            let frac = (ch.hp as f32 / ch.max_hp as f32).clamp(0.0, 1.0);
            let (w, h) = (118.0, 11.0);
            draw_bar(&mut scene.screen, &mut trails, p, sx - w * 0.5, sy - h, w, h, frac, alpha);
        }
        if let Some((p, b)) = boss {
            let frac = (b.hp as f32 / b.max_hp as f32).clamp(0.0, 1.0);
            let (x, y, w, h) = (510.0, 70.0, 900.0, 18.0);
            // gold double frame and diamond end caps
            scene.screen.push(rect(x - 7.0, y - 7.0, w + 14.0, h + 14.0, [0.0, 0.0, 0.0, 0.75]));
            scene.screen.push(rect(x - 5.0, y - 5.0, w + 10.0, h + 10.0, [0.78, 0.67, 0.43, 1.0]));
            scene.screen.push(rect(x - 3.5, y - 3.5, w + 7.0, h + 7.0, [0.05, 0.05, 0.06, 1.0]));
            for cx in [x - 13.0, x + w + 13.0] {
                let s = 9.0;
                let cy = y + h * 0.5;
                scene.screen.push(Sprite {
                    tex: "white",
                    additive: false,
                    pos: [[cx, cy - s, 0.], [cx + s, cy, 0.], [cx, cy + s, 0.], [cx - s, cy, 0.]],
                    uv: quad_uv(),
                    color: [0.85, 0.72, 0.45, 1.0],
                });
            }
            draw_bar(&mut scene.screen, &mut trails, p, x, y, w, h, frac, 1.0);
            let txt = format!("{} / {}", b.hp, b.max_hp);
            scene.screen.extend(font::text(&txt, 961.5, y + 2.5, h - 3.0, [0.0, 0.0, 0.0, 0.9]));
            scene.screen.extend(font::text(&txt, 960.0, y + 1.0, h - 3.0, [1.0, 0.97, 0.92, 1.0]));
        }
        let alive = &self.alive_ptrs;
        trails.retain(|k, _| alive.contains(k));
        self.bar_trails = trails;
    }

    /// Runs after Elden Ring's MenuMan and before Scaleform draws: its HUD is hidden (HideAll; writing at DrawBegin
    /// came too late, and zeroing its bar values did nothing). The mod draws enemy and boss health itself.
    pub fn hide_vanilla_hud() {
        let Ok(fe) = (unsafe { eldenring::cs::CSFeManImp::instance_mut() }) else { return };
        // ShowAll is the Esc menu: leave it be
        if tuning::HIDE_ER_BARS > 0.5 && fe.hud_state != eldenring::cs::CSFeManHudState::ShowAll {
            fe.hud_state = eldenring::cs::CSFeManHudState::HideAll;
        }
    }

    pub fn block_inputs() {
        unsafe {
            if let Ok(w) = WorldChrMan::instance_mut() {
                if let Some(p) = w.main_player.as_deref_mut() {
                    hold_super_armor(&mut p.chr_ins);
                    let ar = &mut p.chr_ins.modules.action_request;
                    ar.disabled_action_inputs.0 |= BLOCKED;
                    ar.disabled_action_inputs.0 &= !ABILITY_BITS;
                }
            }
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().clamp(0.0, 0.1);
        self.last = now;
        self.clock += dt;
        self.dt = dt;
        self.frame += 1;
        if let Some(a) = self.audio.as_mut() {
            a.update();
        }

        self.watch_menus();
        self.update_skins();
        self.update_items();
        // loading screens (er_menus sheet): nothing of Fiddlesticks on them
        if sheet::HIDE_ON_UI.iter().any(|&i| self.ui_seen & (1 << i) != 0) {
            self.publish(None);
            return;
        }
        let Ok(wcm) = (unsafe { WorldChrMan::instance_mut() }) else {
            self.publish(None);
            return;
        };
        let Some(player) = wcm.main_player.as_deref_mut() else {
            self.publish(None);
            return;
        };
        // cutscenes: the game stops drawing the real Tarnished (an actor plays the scene), so does Fiddlesticks
        let drawn = player.chr_ins.chr_flags1c5.enable_render();
        if drawn != self.player_drawn {
            self.player_drawn = drawn;
            log!("player drawn by the game: {drawn}");
        }
        if !drawn {
            self.publish(None);
            return;
        }
        let player_ptr = &player.chr_ins as *const ChrIns as usize;
        let me = read_chr(&player.chr_ins);
        let rune_ap = unsafe { player.player_game_data.as_ref().level.saturating_sub(1) } as f32 * tuning::AP_PER_RUNE_LEVEL;
        let item_ap: f32 = self.items.iter().filter_map(|&i| self.item_info(i)).map(|i| i.ap).sum();
        let ap = (rune_ap + item_ap) * (1.0 + self.item_effect("ap_mult"));
        self.ap_total = ap;

        // hide the Tarnished under Fiddlesticks
        player.chr_ins.base_transparency = 0.0;
        player.chr_ins.opacity_keyframes_multiplier = 0.0;
        let interrupted = self.immune_to_cc(&mut player.chr_ins.modules.super_armor);
        hold_super_armor(&mut player.chr_ins);
        // whose progress: the loaded save slot and the character's name
        {
            let pgd = unsafe { player.player_game_data.as_ref() };
            let name: String = String::from_utf16_lossy(&pgd.character_name).trim_end_matches('\0').chars().filter(|c| c.is_alphanumeric()).collect();
            let slot = unsafe { eldenring::cs::GameMan::instance() }.map(|g| g.save_slot).unwrap_or(-1);
            let key = format!("{slot}_{name}");
            if self.save_key.as_deref() != Some(key.as_str()) {
                self.switch_save(key, pgd.level);
            }
        }
        if !self.hidden_logged {
            self.hidden_logged = true;
            log!("player found at {:?}, hp {}, ER level {}, ap {ap}", me.pos, me.hp, ap / tuning::AP_PER_RUNE_LEVEL + 1.0);
        }

        // input: Elden Ring's own action bits (pad and bound keys) plus 1-4 / Ctrl+1-4 on the keyboard
        let stamina = player.chr_ins.modules.data.stamina;
        let ar = &mut player.chr_ins.modules.action_request;
        ar.disabled_action_inputs.0 |= BLOCKED;
        ar.disabled_action_inputs.0 &= !ABILITY_BITS;
        let actions = ABILITY_ACTIONS.swap(0, std::sync::atomic::Ordering::Relaxed);
        if actions != self.dbg_last {
            self.dbg_last = actions;
            log!("INPUT ability buttons {actions:#x} (Tarnished stamina {stamina})");
        }
        let pressed_actions = actions & !self.prev_actions;
        self.prev_actions = actions;
        let focused = game_focused();
        let ctrl = focused && key_down(VK_CONTROL.0 as i32);
        let alt = focused && key_down(VK_MENU.0 as i32);
        let mut keys = [false; 12];
        for i in 0..4 {
            let down = focused && key_down(b'1' as i32 + i as i32);
            keys[i] = down && !ctrl && !alt;
            keys[4 + i] = down && alt;
            keys[8 + i] = down && ctrl;
        }
        let key_pressed: Vec<bool> = (0..12).map(|i| keys[i] && !self.prev_keys[i]).collect();
        self.prev_keys = keys;
        let player_level_ap = ap;
        let player_orient = player.chr_ins.modules.physics.orientation;
        let locked = player.locked_on_enemy.clone();
        let _ = player;

        // characters near the player this frame
        let mut alive = HashSet::new();
        let mut hostiles: Vec<Chr> = vec![];
        self.near.clear();
        for e in wcm.chr_inses_by_distance.iter() {
            let c = unsafe { e.chr_ins.as_ref() };
            let p = c as *const ChrIns as usize;
            if p == player_ptr {
                continue;
            }
            alive.insert(p);
            let ch = read_chr(c);
            if self.near.len() < 6 {
                let d = sub(ch.pos, me.pos);
                self.near.push((c.character_id, c.team_type, ch.hp, len(d), flat(d)));
            }
            if self.test && self.status_t == 0.0 && len(sub(ch.pos, me.pos)) < 30.0 {
                log!("near chr {} team {} hp {} at {:.1}m", c.character_id, c.team_type, ch.hp, len(sub(ch.pos, me.pos)));
            }
            if ch.hp > 0 && len(sub(ch.pos, me.pos)) < 60.0 {
                if !sheet::KNOWN_TEAMS.contains(&c.team_type) && self.seen_teams.insert(c.team_type) {
                    log!("unknown team type {} (chr {}) near the player", c.team_type, c.character_id);
                }
                // test runs also target the characters they spawn themselves (any non-player team)
                if sheet::HOSTILE_TEAMS.contains(&c.team_type) || (self.test && c.team_type != 1) {
                    hostiles.push(ch);
                }
            }
        }
        for s in [1, 2] {
            let range = val(&self.c, s, "CastRange", self.rank(s).max(1)) * tuning::RANGE_SCALE;
            self.in_range[s] = hostiles.iter().any(|h| len(sub(h.pos, me.pos)) <= range);
        }
        self.alive_ptrs = alive.clone();
        self.hostile_ptrs = hostiles.iter().map(|h| h.ptr).collect();
        self.track_bosses(wcm);
        self.update_burns(dt);
        self.snap_pos = me.pos;
        let locked_ptr = wcm.chr_ins_by_handle(&locked).map(|c| c as *const ChrIns as usize).filter(|p| alive.contains(p));

        // kills: characters we damaged whose HP reached 0
        let mut xp_gain = 0.0;
        let mut kills = 0;
        let damaged: Vec<(usize, i32)> = self.damaged.iter().map(|(k, v)| (*k, *v)).collect();
        for (p, max_hp) in damaged {
            if !alive.contains(&p) {
                self.damaged.remove(&p);
                continue;
            }
            if chr_mut(p).modules.data.hp <= 0 {
                self.damaged.remove(&p);
                if chr_mut(p).team_type == 7 {
                    // a strong enemy: a boss if the game asks for its "felled" banner (track_bosses)
                    self.strong_kill_at = self.clock;
                    log!("strong enemy killed: chr {} (max hp {max_hp})", chr_mut(p).character_id);
                }
                xp_gain += (max_hp as f32 * tuning::XP_PER_ENEMY_HP).clamp(tuning::XP_MIN, tuning::XP_MAX);
                kills += 1;
            }
        }
        if kills > 0 {
            self.kills += kills;
            log!("KILL x{kills} (+{xp_gain:.0} xp, total kills {})", self.kills);
            self.gain_xp(xp_gain);
            self.sound(if kills >= 5 { "vo_penta" } else { "vo_kill" });
        }
        self.cc.retain(|c| alive.contains(&c.ptr));
        self.missiles.retain(|m| alive.contains(&m.target));

        // death and respawn
        if me.hp <= 0 && !self.dead {
            self.dead = true;
            self.cancel_channels(player_ptr);
            self.anim.play("death", true);
            self.sound("vo_death");
            self.sound("sfx_death");
        } else if me.hp > 0 && self.dead {
            self.dead = false;
            self.anim.play("respawn", true);
            self.sound("vo_respawn");
            self.sound("sfx_respawn");
        }

        // movement speed and facing
        if let Some(lp) = self.last_pos {
            let d = sub(me.pos, lp);
            if dt > 0.0 && flat(d) < 20.0 {
                self.speed += (flat(d) / dt - self.speed) * (dt * 10.0).min(1.0);
            }
        }
        self.last_pos = Some(me.pos);
        let q = glam::Quat::from_xyzw(player_orient.0, player_orient.1, player_orient.2, player_orient.3);
        // Elden Ring characters face their local -Z (checked in game: +Z pointed back at the camera)
        let fwd = q * glam::Vec3::NEG_Z;
        let forward = norm([fwd.x, 0.0, fwd.z]);

        for c in self.cd.iter_mut() {
            *c = (*c - dt).max(0.0);
        }

        // casting
        let rooted = matches!(self.ult, Some(Ult::Channel { .. } | Ult::Leap { .. }));
        if !self.dead && !rooted {
            let mut wanted: Option<usize> = None;
            for slot in 0..5 {
                let bit = ACTION_BIT[slot];
                if pressed_actions & (1u64 << bit) != 0 || (slot > 0 && key_pressed[slot - 1]) {
                    wanted = Some(slot);
                }
            }
            if self.test && hostiles.is_empty() {
                self.test_spawn_t += dt;
                if self.test_spawn_t > 6.0 && self.test_spawns < 2 {
                    self.test_spawns += 1;
                    self.test_spawn_t = 0.0;
                    let at = add(me.pos, mul(forward, 6.0));
                    log!("TEST spawning a Wandering Noble (c4300) at {at:?}");
                    wcm.spawn_debug_character(&eldenring::cs::ChrDebugSpawnRequest {
                        chr_id: 4300,
                        chara_init_param_id: -1,
                        npc_param_id: 43000000,
                        npc_think_param_id: 43000000,
                        event_entity_id: -1,
                        talk_id: -1,
                        pos_x: at[0],
                        pos_y: at[1],
                        pos_z: at[2],
                    });
                }
            }
            if self.test {
                self.test_t += dt;
                if self.test_t > 2.2 {
                    self.test_t = 0.0;
                    wanted = Some(self.test_next);
                    self.test_next = (self.test_next + 1) % 5;
                }
            }
            for s in 1..5 {
                if key_pressed[8 + s - 1] {
                    self.rank_up(s);
                }
            }
            if let Some(slot) = wanted {
                // pressing an ability not learned yet spends a free point on it (handy on a pad)
                if self.rank(slot) == 0 {
                    self.rank_up(slot);
                } else {
                    self.try_cast(slot, &me, forward, &hostiles, locked_ptr, player_level_ap);
                }
            }
            for (i, e) in ["joke", "taunt", "dance", "laugh"].iter().enumerate() {
                if key_pressed[4 + i] {
                    self.anim.play(e, true);
                    self.sound(&format!("vo_{e}"));
                    if *e == "dance" {
                        self.sound("sfx_dance");
                    }
                }
            }
        }

        self.update_pending(dt, &me, &hostiles, player_level_ap);
        self.update_missiles(dt, &hostiles, player_level_ap);
        if interrupted && self.drain.is_some() {
            // what would have staggered him breaks the channel
            log!("W interrupted by a hit that would have staggered him");
            self.drain = None;
            self.anim.play("w_end", true);
            if let Some(a) = self.audio.as_mut() {
                a.stop("sfx_w_");
            }
        }
        self.update_drain(dt, &me, player_ptr, player_level_ap);
        // Bountiful Harvest: the Tarnished drags their feet while it drains (root motion follows animation speed)
        if self.drain.is_some() || self.w_slowed {
            self.w_slowed = self.drain.is_some();
            chr_mut(player_ptr).modules.behavior.animation_speed = if self.w_slowed { tuning::W_SELF_SLOW } else { 1.0 };
        }
        self.update_ult(dt, &me, forward, player_ptr, &hostiles, player_level_ap);
        self.update_cc();
        self.snap_fwd = self.face.map(|f| f.0).unwrap_or(forward);
        self.sync_vfx();

        // locomotion when no one-shot is playing
        let id = self.anim.id();
        let held = matches!(id, "w_loop" | "dance" | "recall" | "r_channel");
        let emote_broken = matches!(id, "dance" | "recall") && self.speed > 0.8;
        if !self.dead && ((!self.anim.busy(&self.c) && !held) || emote_broken) {
            let league_run = self.stat("move_speed") * tuning::MODEL_SCALE;
            if self.speed > 4.6 {
                self.anim.play("run_fast", false);
                self.anim.rate = (self.speed / (league_run * 1.6)).clamp(0.7, 1.8);
            } else if self.speed > 0.4 {
                self.anim.play("run", false);
                self.anim.rate = (self.speed / league_run).clamp(0.6, 1.8);
                if !self.moved_once {
                    self.moved_once = true;
                    self.sound("vo_first_move");
                }
            } else {
                self.anim.play("idle", false);
            }
        }
        if self.anim.id() == "w_loop" && self.drain.is_none() {
            self.anim.play("w_end", true);
        }
        self.anim.update(&self.c, dt);

        self.status_t += dt;
        if self.status_t > 5.0 {
            self.status_t = 0.0;
            if let Ok(cam) = unsafe { CSCamera::instance() } {
                let m = &cam.pers_cam_1.matrix;
                log!("facing player {:?} camera fwd ({:.2},{:.2},{:.2}) cam pos ({:.1},{:.1},{:.1})", forward, m.2.0, m.2.1, m.2.2, m.3.0, m.3.1, m.3.2);
            }
            log!(
                "status lvl {} xp {:.0} hp {} speed {:.1} anim {} hostiles {} cd {:?}",
                self.level,
                self.xp,
                me.hp,
                self.speed,
                self.anim.id(),
                hostiles.len(),
                self.cd.map(|c| c.round())
            );
        }
        self.save_t += dt;
        if self.save_t > 10.0 {
            self.save_t = 0.0;
            self.save();
        }

        // facing: towards the current cast target for a moment, otherwise the Tarnished's
        let mut face = forward;
        if let Some((dir, t)) = self.face.as_mut() {
            *t -= dt;
            face = *dir;
            if *t <= 0.0 {
                self.face = None;
            }
        }
        self.publish(Some((me, face)));
    }

    /// Logs which Elden Ring UI elements are visible and the HUD state, on every change (to map screens to ids).
    fn watch_menus(&mut self) {
        let Ok(menu) = (unsafe { eldenring::cs::CSMenuManImp::instance() }) else { return };
        let mut vis: u128 = 0;
        for (i, s) in menu.ui_states.iter().enumerate() {
            if s.visible() {
                vis |= 1 << i;
            }
        }
        let hud = unsafe { eldenring::cs::CSFeManImp::instance() }.map(|f| f.hud_state as i32).unwrap_or(-1);
        // League champions have no equipment or inventory: back out of those screens as they open
        if sheet::CLOSE_UI.iter().any(|&i| vis & (1 << i) != 0) && self.clock - self.last_close > 0.3 {
            self.last_close = self.clock;
            press_escape();
        }
        if vis != self.ui_seen || hud != self.hud_seen {
            let ids: Vec<usize> = (0..0x46).filter(|i| vis & (1 << i) != 0).collect();
            log!("UI visible {ids:?} hud_state {hud}");
            self.ui_seen = vis;
            self.hud_seen = hud;
        }
    }

    fn cancel_channels(&mut self, player_ptr: usize) {
        if self.drain.take().is_some() {
            if let Some(a) = self.audio.as_mut() {
                a.stop("sfx_w_");
            }
        }
        if matches!(self.ult, Some(Ult::Channel { .. } | Ult::Leap { .. })) {
            chr_mut(player_ptr).debug_flags.set_disabled_movement(false);
        }
        self.ult = None;
        self.pending = None;
    }

    fn try_cast(&mut self, slot: usize, me: &Chr, forward: V3, hostiles: &[Chr], locked: Option<usize>, _ap: f32) {
        let rank = self.rank(slot);
        let ab = ability(slot);
        if rank == 0 || self.cd[slot] > 0.0 || self.pending.is_some() {
            return;
        }
        let range = match slot {
            0 => self.stat("attack_range"),
            4 => 800.0,
            _ => val(&self.c, slot, "CastRange", rank),
        } * tuning::RANGE_SCALE;
        // target: lock-on first, else the nearest hostile in front of the camera-facing direction
        let pick = |range: f32| -> Option<Chr> {
            if let Some(l) = locked {
                if let Some(h) = hostiles.iter().find(|h| h.ptr == l) {
                    if len(sub(h.pos, me.pos)) <= range + 1.0 {
                        return Some(*h);
                    }
                }
            }
            let cos = (tuning::AA_CONE_DEG.to_radians()).cos();
            let nearest = |cone: bool| {
                hostiles
                    .iter()
                    .filter(|h| {
                        let d = sub(h.pos, me.pos);
                        len(d) <= range && (!cone || flat(d) < 1.5 || (d[0] * forward[0] + d[2] * forward[2]) / flat(d) >= cos)
                    })
                    .min_by(|a, b| len(sub(a.pos, me.pos)).total_cmp(&len(sub(b.pos, me.pos))))
                    .copied()
            };
            nearest(true).or_else(|| nearest(false))
        };
        let (target, point) = match slot {
            0 | 1 => match pick(range) {
                Some(t) => (Some(t.ptr), t.pos),
                None => {
                    // say who was around, to spot enemies whose team the sheet does not count as hostile
                    if self.clock - self.no_target_logged > 2.0 {
                        self.no_target_logged = self.clock;
                        let near: Vec<String> = self.near.iter().map(|(id, team, hp, d, dh)| format!("c{id} team {team} hp {hp} {d:.1}m (flat {dh:.1})")).collect();
                        log!("{}: no enemy within {range:.1} m; nearest: {}", ab.id, near.join(", "));
                    }
                    return;
                }
            },
            // Bountiful Harvest needs someone to drain (the creator's rule, unlike League)
            2 if !hostiles.iter().any(|h| len(sub(h.pos, me.pos)) <= range) => return,
            2 => (None, me.pos),
            3 => match pick(range) {
                Some(t) => (Some(t.ptr), t.pos),
                None => (None, add(me.pos, mul(forward, range * 0.6))),
            },
            _ => (pick(range).map(|t| t.ptr), me.pos),
        };
        let cd = match slot {
            0 => 1.0 / self.attack_speed(),
            _ => {
                let haste: f32 = self.items.iter().filter_map(|&i| self.item_info(i)).map(|i| i.haste).sum::<f32>() + if slot == 4 { self.item_effect("r_haste") } else { 0.0 };
                val(&self.c, slot, "CooldownTime", rank) * 100.0 / (100.0 + haste)
            }
        };
        self.cd[slot] = cd;
        // basic attack: League's melee swipe when the target is close, otherwise the ranged cast
        let melee = slot == 0 && flat(sub(point, me.pos)) <= tuning::MELEE_RANGE;
        let alt = self.frame % 2 == 0;
        let clip = match (slot, melee, alt) {
            (0, true, true) => "melee2",
            (0, true, false) => "melee1",
            (0, false, true) => "attack2",
            _ => ab.anim,
        };
        self.anim.play(clip, true);
        let cast_vfx = match (slot, melee) {
            (0, false) => Some("aa_cast"),
            (1, _) => Some("q_cast"),
            (2, _) => Some("w_cast"),
            _ => None,
        };
        if let Some(id) = cast_vfx {
            let follow = if id == "aa_cast" { Follow::Bone("L_Hand") } else { Follow::Caster };
            let at = self.caster_frame();
            self.vfx_once(id, follow, at);
        }
        self.sound(ab.sfx_cast);
        self.sound(ab.vo);
        let dir = norm([point[0] - me.pos[0], 0.0, point[2] - me.pos[2]]);
        if flat(sub(point, me.pos)) > 0.3 {
            self.face = Some((dir, 0.8));
        }
        let mut windup = spell(&self.c, slot).map(|s| s.cast_frame / 30.0).unwrap_or(0.25).min(anim::length(&self.c, ab.anim) * 0.6).max(0.05);
        if slot == 0 {
            // stretch the swing over most of the attack period (League's clip alone looks rushed), never faster than authored;
            // locomotion leaves `rate` alone while the one-shot is busy
            let clip_len = anim::length(&self.c, clip);
            self.anim.rate = (clip_len / (cd * tuning::AA_ANIM_FRACTION)).clamp(0.25, 1.0);
            windup = (clip_len / self.anim.rate * tuning::AA_WINDUP_FRACTION).max(0.05);
        }
        log!("CAST {} rank {rank} cd {cd:.1}s target {}", ab.id, target.is_some());
        if self.test && self.shot_at.is_none() {
            let delay = match slot { 2 => windup + 0.8, 4 => 3.0, _ => windup + 0.15 };
            self.shot_at = Some((format!("{}_{:.0}", ab.id, self.clock), self.clock + delay));
        }
        if slot == 2 {
            self.anim.play("w_start", true);
        }
        if slot == 4 {
            self.ult = Some(Ult::Channel { t: 0.0, rank, target });
            return;
        }
        self.pending = Some(Pending { slot, t: windup, target, point, rank, melee });
    }

    fn find(hostiles: &[Chr], p: usize) -> Option<Chr> {
        hostiles.iter().find(|h| h.ptr == p).copied()
    }

    fn update_pending(&mut self, dt: f32, me: &Chr, hostiles: &[Chr], ap: f32) {
        let Some(p) = self.pending.as_mut() else { return };
        p.t -= dt;
        if p.t > 0.0 {
            return;
        }
        let Some(p) = self.pending.take() else { return };
        match p.slot {
            0 | 1 => {
                let Some(t) = p.target.and_then(|t| Self::find(hostiles, t)) else { return };
                if p.melee {
                    let ad = self.stat("ad") + self.stat("ad_per_level") * growth(self.level);
                    self.damage(t, ad, "AA");
                    self.sound("sfx_aa_hit");
                    let dir = norm(sub(me.pos, t.pos));
                    let which = if self.anim.id() == "melee2" { "melee2_hit" } else { "melee1_hit" };
                    let at = self.chr_frame(t.ptr).unwrap_or(self.caster_frame());
                    if !self.vfx_once(which, Follow::Chr(t.ptr), at) {
                        self.fx.burst("aa_melee", "hit", add(t.pos, [0.0, t.height * 0.6, 0.0]), dir, 0.0);
                    }
                    return;
                }
                let group = if p.slot == 0 { "aa_missile" } else { "q_fear" };
                let start = add(me.pos, [0.0, 1.6, 0.0]);
                self.next_missile += 1;
                let dir = norm(sub(t.pos, me.pos));
                self.missiles.push(Missile { id: self.next_missile, group, slot: p.slot, pos: start, dir, target: t.ptr, rank: p.rank });
            }
            2 => {
                let range = val(&self.c, 2, "CastRange", p.rank) * tuning::RANGE_SCALE;
                let targets: Vec<usize> = hostiles.iter().filter(|h| len(sub(h.pos, me.pos)) <= range).map(|h| h.ptr).collect();
                log!("W drains {} target(s)", targets.len());
                self.sound("sfx_w_drain");
                self.anim.play("w_loop", true);
                self.drain = Some(Drain { targets, t: 0.0, tick: 0.0, rank: p.rank });
            }
            3 => {
                let radius = val(&self.c, 3, "castRadius", p.rank) * tuning::RANGE_SCALE * 0.5;
                let dmg = (val(&self.c, 3, "BaseDamage", p.rank) + 0.5 * ap) * tuning::E_DAMAGE_MULT;
                let slow = -val(&self.c, 3, "SlowAmount", p.rank);
                let silence = val(&self.c, 3, "SilenceDuration", p.rank);
                let dir = norm([p.point[0] - me.pos[0], 0.0, p.point[2] - me.pos[2]]);
                if !self.vfx_once("e_cast", Follow::Fixed, Frame { pos: p.point, fwd: dir, target: None }) {
                    self.fx.burst("e_reap", "area", p.point, dir, radius);
                }
                log!("E at {:?} (player {:?}) radius {radius:.1}", p.point, me.pos);
                let mut hit = false;
                for h in hostiles.iter().filter(|h| flat(sub(h.pos, p.point)) <= radius) {
                    self.damage(*h, dmg, "E");
                    let at = self.chr_frame(h.ptr).unwrap_or(self.caster_frame());
                    if !self.vfx_once("e_hit", Follow::Chr(h.ptr), at) {
                        self.fx.burst("e_reap", "hit", add(h.pos, [0.0, h.height * 0.5, 0.0]), [0.0, 1.0, 0.0], 0.0);
                    }
                    let inner = flat(sub(h.pos, p.point)) <= radius * tuning::E_SILENCE_INNER;
                    self.apply_cc(h.ptr, tuning::E_SLOW_DURATION, false, false, slow);
                    if inner {
                        self.apply_cc(h.ptr, silence, false, true, 0.0);
                    }
                    hit = true;
                }
                if hit {
                    self.sound("sfx_e_hit");
                }
            }
            _ => {}
        }
        let _ = ap;
    }

    fn update_missiles(&mut self, dt: f32, hostiles: &[Chr], ap: f32) {
        let mut landed = vec![];
        for (i, m) in self.missiles.iter_mut().enumerate() {
            let Some(t) = Self::find(hostiles, m.target) else {
                landed.push((i, None));
                continue;
            };
            let aim = add(t.pos, [0.0, t.height * 0.6, 0.0]);
            let d = sub(aim, m.pos);
            let step = tuning::MISSILE_SPEED * dt;
            if len(d) <= step.max(0.3) {
                landed.push((i, Some(t)));
            } else {
                m.dir = norm(d);
                m.pos = add(m.pos, mul(m.dir, step));
            }
        }
        let trails: Vec<(&'static str, usize, V3, V3)> =
            self.missiles.iter().filter(|m| !self.vfx_live.contains_key(&(if m.slot == 0 { "aa_missile" } else { "q_missile" }, m.id))).map(|m| (m.group, m.id, m.pos, m.dir)).collect();
        for (group, id, pos, dir) in trails {
            self.fx.stream(group, "missile_trail", id, pos, mul(dir, -1.0), 0.0, dt);
        }
        for (i, t) in landed.into_iter().rev() {
            let m = self.missiles.remove(i);
            let Some(t) = t else { continue };
            let hit_at = add(t.pos, [0.0, t.height * 0.6, 0.0]);
            if m.slot == 0 {
                let ad = self.stat("ad") + self.stat("ad_per_level") * growth(self.level);
                self.damage(t, ad, "AA");
                self.sound("sfx_aa_hit");
                let at = self.chr_frame(t.ptr).unwrap_or(self.caster_frame());
                if !self.vfx_once("aa_hit", Follow::Chr(t.ptr), at) {
                    self.fx.burst("aa_missile", "hit", hit_at, mul(m.dir, -1.0), 0.0);
                }
            } else {
                let feared = self.cc.iter().any(|c| c.ptr == t.ptr && c.fear && c.until > self.clock);
                let cur = chr_mut(t.ptr).modules.data.hp as f32;
                let pct = val(&self.c, 1, "MaxHealthDamage", m.rank) + 0.0003 * ap;
                let mut dmg = (pct * cur).max(val(&self.c, 1, "MinimumDamage", m.rank));
                if feared {
                    dmg *= 2.0;
                }
                self.damage(t, dmg, "Q");
                let dur = val(&self.c, 1, "FearDuration", m.rank);
                self.apply_cc(t.ptr, dur, true, true, 0.0);
                self.sound("sfx_q_hit");
                let at = self.chr_frame(t.ptr).unwrap_or(self.caster_frame());
                if !self.vfx_once(if feared { "q_hit_feared" } else { "q_hit" }, Follow::Chr(t.ptr), at) {
                    self.fx.burst("q_fear", "hit", hit_at, mul(m.dir, -1.0), 0.0);
                }
            }
        }
    }

    fn update_drain(&mut self, dt: f32, me: &Chr, player_ptr: usize, ap: f32) {
        let Some(mut d) = self.drain.take() else { return };
        let dur = val(&self.c, 2, "DrainDuration", d.rank);
        let tps = val(&self.c, 2, "TicksPerSecond", d.rank).max(1.0);
        let leash = val(&self.c, 2, "DrainLeashRange", d.rank) * tuning::RANGE_SCALE;
        d.t += dt;
        d.tick += dt;
        let alive: Vec<Chr> = d.targets.iter().filter_map(|&p| {
            let c = chr_mut(p);
            let ch = read_chr(c);
            (ch.hp > 0 && len(sub(ch.pos, me.pos)) <= leash).then_some(ch)
        }).collect();
        d.targets = alive.iter().map(|c| c.ptr).collect();
        if alive.is_empty() {
            // everyone it drained died or broke free: the channel stops at once, and so does its sound
            self.anim.play("w_end", true);
            if let Some(a) = self.audio.as_mut() {
                a.stop("sfx_w_");
            }
            return;
        }
        let vamp = val(&self.c, 2, "VampPercentage", d.rank) / 100.0;
        let mut healed = 0.0;
        let finished = d.t >= dur;
        if d.tick >= 1.0 / tps || finished {
            d.tick = 0.0;
            let per = (val(&self.c, 2, "DamagePerSecond", d.rank) + 0.45 * ap) / tps;
            for t in &alive {
                let mut dmg = per;
                if finished {
                    dmg += val(&self.c, 2, "PercentMultiplier", d.rank) * (t.max_hp - t.hp).max(0) as f32;
                }
                healed += self.damage(*t, dmg, "W") * vamp;
                let chest = add(t.pos, [0.0, t.height * 0.6, 0.0]);
                if !self.vfx_once("w_heal_missile", Follow::ToCaster, Frame { pos: chest, fwd: norm(sub(me.pos, t.pos)), target: None }) {
                    self.fx.burst("w_drain", "tick", chest, norm(sub(me.pos, t.pos)), 0.0);
                }
            }
        }
        for t in &alive {
            if !self.vfx_has("w_target") {
                self.fx.stream("w_drain", "victim", t.ptr, add(t.pos, [0.0, t.height * 0.5, 0.0]), [0.0, 1.0, 0.0], 0.0, dt);
            }
        }
        if !self.vfx_has("w_body") {
            self.fx.stream("w_drain", "channel", 0, me.pos, [0.0, 1.0, 0.0], 0.0, dt);
        }
        if healed > 0.0 {
            // no effect on him: League's green heal sparks sat in the middle of the model (creator's request)
            let p = chr_mut(player_ptr);
            p.modules.data.hp = (p.modules.data.hp + healed.round() as i32).min(p.modules.data.max_hp);
        }
        if finished {
            if finished && !alive.is_empty() {
                self.sound("sfx_w_final");
                for t in &alive {
                    let at = self.chr_frame(t.ptr).unwrap_or(self.caster_frame());
                    if !self.vfx_once("w_final", Follow::Chr(t.ptr), at) {
                        self.fx.burst("w_drain", "final", add(t.pos, [0.0, t.height * 0.6, 0.0]), [0.0, 1.0, 0.0], 0.0);
                    }
                }
            }
            self.anim.play("w_end", true);
            return;
        }
        self.drain = Some(d);
    }

    fn update_ult(&mut self, dt: f32, me: &Chr, forward: V3, player_ptr: usize, hostiles: &[Chr], ap: f32) {
        let Some(u) = self.ult.take() else { return };
        self.ult = match u {
            Ult::Channel { t, rank, target } => {
                let player = chr_mut(player_ptr);
                player.debug_flags.set_disabled_movement(true);
                let t = t + dt;
                if !self.vfx_has("r_chest") {
                    self.fx.stream("r_crowstorm", "r_channel", 0, me.pos, [0.0, 1.0, 0.0], 0.0, dt);
                }
                if t < val(&self.c, 4, "ChannelTime", rank) {
                    if self.anim.id() != "r_channel" {
                        self.anim.play("r_channel", true);
                    }
                    Some(Ult::Channel { t, rank, target })
                } else {
                    player.debug_flags.set_disabled_movement(false);
                    // jump: onto the locked target, else straight ahead
                    let to = match target.and_then(|p| Self::find(hostiles, p)) {
                        Some(tp) => {
                            let back = norm([me.pos[0] - tp.pos[0], 0.0, me.pos[2] - tp.pos[2]]);
                            add(tp.pos, mul(back, 1.2))
                        }
                        None => add(me.pos, mul(forward, tuning::R_LEAP_DISTANCE)),
                    };
                    let dir = norm([to[0] - me.pos[0], 0.0, to[2] - me.pos[2]]);
                    if flat(sub(to, me.pos)) > 0.3 {
                        self.face = Some((dir, tuning::R_LEAP_TIME + 0.3));
                    }
                    self.anim.play("r_channel", true);
                    log!("R leap {:?} -> {:?}", me.pos, to);
                    Some(Ult::Leap { t: 0.0, rank, from: me.pos, to })
                }
            }
            Ult::Leap { t, rank, from, to } => {
                let player = chr_mut(player_ptr);
                let t = t + dt;
                let f = (t / tuning::R_LEAP_TIME).min(1.0);
                // ease out: a fast launch that settles; x/z only, the game keeps him on the ground
                let e = 1.0 - (1.0 - f) * (1.0 - f);
                let ph = &mut player.modules.physics;
                ph.position.0 = from[0] + (to[0] - from[0]) * e;
                ph.position.2 = from[2] + (to[2] - from[2]) * e;
                ph.chr_proxy_pos_update_requested = true;
                self.fx.stream("r_crowstorm", "r_leap", 2, add(me.pos, [0.0, 1.0 + self.leap_lift(), 0.0]), [0.0, 1.0, 0.0], 0.0, dt);
                if f < 1.0 {
                    Some(Ult::Leap { t, rank, from, to })
                } else {
                    self.fx.burst("r_crowstorm", "r_land", me.pos, [0.0, 1.0, 0.0], 0.0);
                    self.sound("sfx_r_storm");
                    Some(Ult::Storm { t: 0.0, tick: 0.0, rank })
                }
            }
            Ult::Storm { t, tick, rank } => {
                let t = t + dt;
                let mut tick = tick + dt;
                let radius = val(&self.c, 4, "castRadius", rank) * tuning::RANGE_SCALE;
                if !self.vfx_has("r_storm") {
                    self.fx.stream("r_crowstorm", "storm", 1, me.pos, [0.0, 1.0, 0.0], radius, dt);
                }
                if tick >= 1.0 / tuning::R_TICKS_PER_SECOND {
                    tick = 0.0;
                    let per = (val(&self.c, 4, "DamagePerSecond", rank) + val(&self.c, 4, "APRatio", rank) * ap) / tuning::R_TICKS_PER_SECOND;
                    for h in hostiles.iter().filter(|h| flat(sub(h.pos, me.pos)) <= radius) {
                        self.damage(*h, per, "R");
                        let at = self.chr_frame(h.ptr).unwrap_or(self.caster_frame());
                        if !self.vfx_once("r_hit", Follow::Chr(h.ptr), at) {
                            self.fx.burst("r_crowstorm", "hit", add(h.pos, [0.0, h.height * 0.6, 0.0]), [0.0, 1.0, 0.0], 0.0);
                        }
                    }
                }
                (t < val(&self.c, 4, "Duration", rank)).then_some(Ult::Storm { t, tick, rank })
            }
        };
    }

    fn update_cc(&mut self) {
        // Elden Ring has no fear/silence/slow statuses we can give: Terrify stuns (frozen animation, no actions or movement), silence blocks actions, slow slows animation
        let clock = self.clock;
        let mut state: HashMap<usize, (bool, bool, f32)> = HashMap::new();
        for c in &self.cc {
            let e = state.entry(c.ptr).or_insert((false, false, 0.0));
            if c.until > clock {
                e.0 |= c.fear;
                e.1 |= c.silence;
                e.2 = e.2.max(c.slow);
            }
        }
        let dt = self.dt;
        for (&p, &(fear, silence, slow)) in &state {
            let c = chr_mut(p);
            // the dead play their death at full speed
            let fear = fear && c.modules.data.hp > 0;
            if fear {
                let ch = read_chr(c);
                if !self.vfx_live.contains_key(&("feared", p)) {
                    self.fx.stream("q_fear", "feared", p, add(ch.pos, [0.0, ch.height + 0.3, 0.0]), [0.0, 1.0, 0.0], 0.0, dt);
                }
            }
            c.debug_flags.set_disabled_secondary_actions(fear || silence);
            c.debug_flags.set_disabled_movement(fear);
            let speed = if fear { tuning::FEAR_ANIM_SPEED } else { 1.0 - slow.clamp(0.0, 0.9) };
            c.modules.behavior.animation_speed = speed;
        }
        self.cc.retain(|c| c.until > clock);
    }

    fn effect_radius(&self, slot_group: &str) -> f32 {
        match slot_group {
            "w_drain" => val(&self.c, 2, "CastRange", self.rank(2).max(1)) * tuning::RANGE_SCALE,
            "r_crowstorm" => val(&self.c, 4, "castRadius", self.rank(4).max(1)) * tuning::RANGE_SCALE,
            "e_reap" => val(&self.c, 3, "castRadius", self.rank(3).max(1)) * tuning::RANGE_SCALE * 0.5,
            _ => 1.0,
        }
    }

    /// Builds the frame for the renderer.
    fn publish(&mut self, me: Option<(Chr, V3)>) {
        let Some((me, face)) = me else {
            *SCENE.lock().unwrap() = None;
            return;
        };
        let Ok(cam) = (unsafe { CSCamera::instance() }) else { return };
        let pc = &cam.pers_cam_1;
        let m = &pc.matrix;
        let cam_m: M4 = [
            m.0.0, m.0.1, m.0.2, 0.0, m.1.0, m.1.1, m.1.2, 0.0, m.2.0, m.2.1, m.2.2, 0.0, m.3.0, m.3.1, m.3.2, 1.0,
        ];
        let right = [m.0.0, m.0.1, m.0.2];
        let up = [m.1.0, m.1.1, m.1.2];
        let cam_pos = [m.3.0, m.3.1, m.3.2];

        // turn towards where he faces over a moment instead of snapping (model_turn_rate: how fast)
        let want = face[0].atan2(face[2]);
        let diff = (want - self.model_yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        self.model_yaw += diff * (1.0 - (-tuning::MODEL_TURN_RATE * self.dt).exp());
        let yaw = self.model_yaw + tuning::MODEL_YAW_DEG.to_radians();
        let (s, c) = yaw.sin_cos();
        let k = tuning::MODEL_SCALE;
        let lift = self.leap_lift();
        let world: M4 = [
            k * c * tuning::MODEL_MIRROR_X, 0., -k * s * tuning::MODEL_MIRROR_X, 0.,
            0., k, 0., 0.,
            k * s, 0., k * c, 0.,
            me.pos[0], me.pos[1] + lift, me.pos[2], 1.,
        ];
        let palette = self.anim.palette(&self.c);
        let soul = self.joint_world(&palette, &world, "Demon_Soul").unwrap_or(add(me.pos, [0.0, 1.0, 0.0]));
        let model = ModelDraw { model: 0, world, palette, visible: self.anim.visible(&self.c) };

        let mut scene = Scene { models: vec![model], cam: cam_m, fov: pc.fov, near: pc.near_plane, far: pc.far_plane, world: vec![], vfx: vec![], screen: vec![], shot: None };
        let view = crate::fx::View { right, up, pos: cam_pos };
        let mut crows: Vec<crate::fx::Crow> = vec![];

        // particles (effects sheet)
        // League particle systems: placed on what they follow, simulated, drawn
        let bones: Vec<(&'static str, V3)> = sheet::VFX_EVENTS
            .iter()
            .filter_map(|e| e.attach.strip_prefix("bone:"))
            .chain(["L_Hand", "R_Hand", "Chest"])
            .filter_map(|b| self.joint_world(&scene.models[0].palette, &scene.models[0].world, b).map(|p| (b, p)))
            .collect();
        {
            let missiles: Vec<(usize, V3, V3)> = self.missiles.iter().map(|m| (m.id, m.pos, m.dir)).collect();
            let alive = &self.alive_ptrs;
            let chest_of = |p: usize| -> Option<V3> {
                alive.contains(&p).then(|| {
                    let ch = read_chr(chr_mut(p));
                    add(ch.pos, [0.0, ch.height * 0.6, 0.0])
                })
            };
            let place = |f: Follow| -> Option<Frame> {
                match f {
                    Follow::Caster => Some(Frame { pos: me.pos, fwd: face, target: None }),
                    Follow::Bone(b) => bones.iter().find(|x| x.0 == b).map(|x| Frame { pos: x.1, fwd: face, target: None }),
                    Follow::Chr(p) => alive.contains(&p).then(|| {
                        let ch = read_chr(chr_mut(p));
                        let d = [ch.pos[0] - me.pos[0], 0.0, ch.pos[2] - me.pos[2]];
                        Frame { pos: ch.pos, fwd: if flat(d) > 0.1 { norm(d) } else { face }, target: None }
                    }),
                    Follow::Missile(id) => missiles.iter().find(|m| m.0 == id).map(|m| Frame { pos: m.1, fwd: norm([m.2[0], 0.0, m.2[2]]), target: None }),
                    Follow::Beam(p) => chest_of(p).map(|c| Frame { pos: soul, fwd: face, target: Some(c) }),
                    Follow::Fixed | Follow::ToCaster => None,
                }
            };
            let mut vfx = std::mem::take(&mut self.vfx);
            vfx.update(&self.c.vfx, self.dt, &place);
            let theme = self.vfx_theme;
            let recolor = |c: [f32; 4]| -> [f32; 4] {
                match &theme {
                    Some(t) => {
                        let r = lolread::theme::recolor([c[0], c[1], c[2]], t);
                        [r[0], r[1], r[2], c[3]]
                    }
                    None => c,
                }
            };
            let mut league_crows = vec![];
            vfx.draw(&self.c.vfx, cam_pos, right, up, &recolor, &mut scene.vfx, &mut league_crows);
            self.vfx = vfx;
            if let Some(raven) = &self.c.raven {
                for (pos, fwd, size, _alpha) in league_crows.into_iter().take(32) {
                    scene.models.push(crate::vfx::crow_draw(raven, pos, fwd, size, self.clock));
                }
            }
        }
        self.fx.update(self.dt, me.pos, soul);
        let radii = [("w_drain", self.effect_radius("w_drain")), ("e_reap", self.effect_radius("e_reap")), ("r_crowstorm", self.effect_radius("r_crowstorm"))];
        let radius_of = |g: &str| radii.iter().find(|r| r.0 == g).map(|r| r.1).unwrap_or(1.0);
        self.fx.draw(&view, radius_of, &mut scene.world, &mut crows);

        // missiles: their heads are emitter rows too (a glowing bolt, or a real crow for Terrify)
        for m in self.missiles.iter().filter(|m| !self.vfx_live.contains_key(&(if m.slot == 0 { "aa_missile" } else { "q_missile" }, m.id))) {
            for e in crate::fx::rows(m.group, "missile_head") {
                if e.render == "model" {
                    crows.push(crate::fx::Crow { pos: m.pos, forward: m.dir, scale: e.size_start, anim: self.clock + m.id as f32, alpha: 1.0 });
                } else {
                    scene.world.push(crate::fx::billboard(e.texture, e.additive, m.pos, e.size_start, self.clock * 6.0, e.color_start, quad_uv(), &view));
                }
            }
        }
        // Terrify: its icon over every feared head
        for c in self.cc.iter().filter(|c| c.fear && c.until > self.clock) {
            let ch = read_chr(chr_mut(c.ptr));
            let bob = 0.08 * (self.clock * 4.0).sin();
            let at = add(ch.pos, [0.0, ch.height + 0.55 + bob, 0.0]);
            let k = tuning::FEAR_ICON_SIZE;
            scene.world.push(crate::fx::billboard("white", false, at, k * 1.12, 0.0, [0.0, 0.0, 0.0, 0.85], quad_uv(), &view));
            scene.world.push(crate::fx::billboard("white", false, at, k * 1.06, 0.0, [0.78, 0.67, 0.43, 1.0], quad_uv(), &view));
            scene.world.push(crate::fx::billboard("icon_q", false, at, k, 0.0, [1.0, 1.0, 1.0, 1.0], quad_uv(), &view));
        }
        // Bountiful Harvest: tendrils from his hands to every victim
        if let Some(d) = self.drain.as_ref().filter(|_| !self.vfx_has("w_drain")) {
            for &p in &d.targets {
                let ch = read_chr(chr_mut(p));
                let to = add(ch.pos, [0.0, ch.height * 0.6, 0.0]);
                self.draw_tendrils(soul, to, p as f32, &view, &mut scene.world);
            }
        }
        // crows: the raven model, flapping its own League flight loop, facing where it flies
        if let Some((raven, clip)) = &self.c.raven {
            let vis = vec![true; raven.submeshes.len()];
            for c in crows.iter().take(48) {
                let k = tuning::MODEL_SCALE * c.scale * (c.alpha * 4.0).min(1.0);
                if k <= 0.0 {
                    continue;
                }
                let f = if len(c.forward) < 0.01 { [0.0, 0.0, 1.0] } else { norm(c.forward) };
                let r = norm(cross([0.0, 1.0, 0.0], f));
                let r = if len(r) < 0.01 { [1.0, 0.0, 0.0] } else { r };
                let u = cross(f, r);
                let mx = tuning::MODEL_MIRROR_X;
                let world: M4 = [
                    r[0] * k * mx, r[1] * k * mx, r[2] * k * mx, 0.,
                    u[0] * k, u[1] * k, u[2] * k, 0.,
                    f[0] * k, f[1] * k, f[2] * k, 0.,
                    c.pos[0], c.pos[1], c.pos[2], 1.,
                ];
                let locals = lolread::pose::sample_locals(raven, Some(clip), c.anim, true);
                scene.models.push(ModelDraw { model: 1, world, palette: lolread::pose::palette(raven, &locals), visible: vis.clone() });
            }
        }

        if let Some(t) = &self.theme {
            for s in scene.world.iter_mut() {
                let c = lolread::theme::recolor([s.color[0], s.color[1], s.color[2]], t);
                s.color = [c[0], c[1], c[2], s.color[3]];
            }
        }
        self.draw_health_bars(&me, &cam_m, pc.fov, pc.near_plane, pc.far_plane, &mut scene);
        // talking to someone (er_menus action hud): no HUD over the dialogue, nor between its lines
        if sheet::HUD_HIDE_UI.iter().any(|&i| self.ui_seen & (1 << i) != 0) {
            self.hud_hidden_until = self.clock + 1.5;
        }
        if self.clock >= self.hud_hidden_until {
            self.build_hud(&mut scene, &me);
        }
        if let Some((name, at)) = &self.shot_at {
            if self.clock >= *at {
                let dir = crate::log::data_dir().join("shots");
                let _ = std::fs::create_dir_all(&dir);
                scene.shot = Some(dir.join(format!("{name}.png")).to_string_lossy().into_owned());
                self.shot_at = None;
            }
        }
        *SCENE.lock().unwrap() = Some(scene);
    }

    /// World position of one of Fiddlesticks' bones this frame.
    fn joint_world(&self, palette: &[M4], world: &M4, name: &str) -> Option<V3> {
        let i = self.c.model.joints.iter().position(|j| j.name == name)?;
        let bind = lolread::math::transform_point(&lolread::math::inverse(&self.c.model.joints[i].ibind), [0.0; 3]);
        let model = lolread::math::transform_point(palette.get(i)?, bind);
        Some(lolread::math::transform_point(world, model))
    }

    /// Bountiful Harvest: one tendril from his chest to a victim, a single smooth ribbon that writhes, thin where
    /// it leaves him and thick where it grips, its texture flowing towards him.
    fn draw_tendrils(&self, from: V3, to: V3, seed: f32, view: &crate::fx::View, out: &mut Vec<Sprite>) {
        const SEGS: usize = 24;
        let d = sub(to, from);
        let side = norm(cross(d, [0.0, 1.0, 0.0]));
        let side = if len(side) < 0.5 { [1.0, 0.0, 0.0] } else { side };
        let lift = norm(cross(side, d));
        let reach = len(d);
        let ph = seed * 0.37;
        let amp = 0.15 + 0.04 * reach.min(8.0);
        let at = |t: f32| -> V3 {
            let bow = (t * std::f32::consts::PI).sin();
            let a = amp * bow * (self.clock * 3.5 + ph + t * 5.0).sin();
            let b = amp * 0.6 * bow * (self.clock * 2.7 + ph * 1.7 + t * 4.0).cos() + 0.3 * bow;
            add(add(from, mul(d, t)), add(mul(side, a), mul(lift, b)))
        };
        let pts: Vec<V3> = (0..=SEGS).map(|i| at(i as f32 / SEGS as f32)).collect();
        // distance along the tendril, for a texture that runs the whole length without seams
        let mut along = vec![0.0f32];
        for i in 0..SEGS {
            along.push(along[i] + len(sub(pts[i + 1], pts[i])));
        }
        for e in crate::fx::rows("w_drain", "tether_beam") {
            // each point's edges, shared by the pieces on both sides of it
            let edge = |i: usize| -> (V3, V3) {
                let t = i as f32 / SEGS as f32;
                let tangent = norm(sub(pts[(i + 1).min(SEGS)], pts[i.saturating_sub(1)]));
                let across = norm(cross(tangent, norm(sub(view.pos, pts[i]))));
                let width = e.size_start * (0.35 + 0.85 * t) * (1.0 + 0.12 * (self.clock * 6.0 + ph + t * 9.0).sin());
                (add(pts[i], mul(across, width * 0.5)), sub(pts[i], mul(across, width * 0.5)))
            };
            let tile = e.size_start * 4.0;
            let v = |i: usize| along[i] / tile - self.clock * 1.5;
            for i in 0..SEGS {
                let (a0, a1) = edge(i);
                let (b0, b1) = edge(i + 1);
                out.push(Sprite {
                    tex: e.texture,
                    additive: e.additive,
                    pos: [a0, b0, b1, a1],
                    uv: [[0.0, v(i)], [0.0, v(i + 1)], [1.0, v(i + 1)], [1.0, v(i)]],
                    color: e.color_start,
                });
            }
        }
    }

    /// Height of the Crowstorm jump's arc right now (0 when not jumping).
    fn leap_lift(&self) -> f32 {
        match self.ult {
            Some(Ult::Leap { t, .. }) => {
                let f = (t / tuning::R_LEAP_TIME).clamp(0.0, 1.0);
                4.0 * tuning::R_LEAP_HEIGHT * f * (1.0 - f)
            }
            _ => 0.0,
        }
    }

    /// Keeps the Tarnished from being staggered and says whether a hit would have broken his real poise.
    fn immune_to_cc(&mut self, sa: &mut eldenring::cs::CSChrSuperArmorModule) -> bool {
        let max = sa.sa_durability_max.max(1.0);
        let now = sa.sa_durability;
        // a drop from what we wrote is a hit's poise damage; at or under his real max the game clamped our value
        let mut broken = false;
        if self.poise_written > 0.0 && now <= max && !self.poise_clamp_logged {
            self.poise_clamp_logged = true;
            log!("CC immunity: the game keeps poise at {now:.0} (max {max:.0}), our value was clamped");
        }
        if self.poise_written > 0.0 && now > max && now < self.poise_written - 0.01 {
            log!("POISE hit {:.0} (real poise {:.0} of {max:.0})", self.poise_written - now, self.real_poise - (self.poise_written - now));
            self.real_poise -= self.poise_written - now;
            self.poise_hit_at = self.clock;
            if self.real_poise <= 0.0 {
                broken = true;
                self.real_poise = max;
            }
        }
        if self.clock - self.poise_hit_at > tuning::CC_POISE_RESET {
            self.real_poise = max;
        }
        sa.sa_durability = tuning::CC_IMMUNE_POISE;
        self.poise_written = tuning::CC_IMMUNE_POISE;
        broken
    }

    /// The skin's League system for a vfx_events moment and its scale, if it has one.
    fn vfx_sys(&self, id: &str) -> Option<(u32, f32)> {
        let row = sheet::VFX_EVENTS.iter().find(|e| e.id == id)?;
        crate::vfx::event_system(&self.c.vfx, row.key).map(|s| (s, tuning::MODEL_SCALE * row.scale))
    }

    fn vfx_has(&self, id: &str) -> bool {
        self.vfx_sys(id).is_some()
    }

    fn caster_frame(&self) -> Frame {
        Frame { pos: self.snap_pos, fwd: self.snap_fwd, target: None }
    }

    fn chr_frame(&self, ptr: usize) -> Option<Frame> {
        self.alive_ptrs.contains(&ptr).then(|| {
            let ch = read_chr(chr_mut(ptr));
            let d = [ch.pos[0] - self.snap_pos[0], 0.0, ch.pos[2] - self.snap_pos[2]];
            Frame { pos: ch.pos, fwd: if flat(d) > 0.1 { norm(d) } else { self.snap_fwd }, target: None }
        })
    }

    /// Plays a one-off League effect; false when the skin has none (the caller falls back to the effects sheet).
    fn vfx_once(&mut self, id: &str, follow: Follow, at: Frame) -> bool {
        let Some((sys, unit)) = self.vfx_sys(id) else { return false };
        self.vfx.spawn(&self.c.vfx, sys, follow, at, unit, 6.0).is_some()
    }

    /// Keeps the lasting League effects in step with the game: missiles, the drain, fear, Crowstorm.
    fn sync_vfx(&mut self) {
        let cf = self.caster_frame();
        let mut want: Vec<(&'static str, usize, Follow, Frame)> = vec![];
        for m in &self.missiles {
            want.push((if m.slot == 0 { "aa_missile" } else { "q_missile" }, m.id, Follow::Missile(m.id), Frame { pos: m.pos, fwd: norm([m.dir[0], 0.0, m.dir[2]]), target: None }));
        }
        if let Some(d) = &self.drain {
            want.push(("w_body", 0, Follow::Caster, cf));
            for &t in &d.targets {
                if let Some(f) = self.chr_frame(t) {
                    want.push(("w_drain", t, Follow::Beam(t), Frame { pos: add(cf.pos, [0.0, 1.0, 0.0]), fwd: cf.fwd, target: Some(add(f.pos, [0.0, 1.0, 0.0])) }));
                    want.push(("w_target", t, Follow::Chr(t), f));
                }
            }
        }
        for c in self.cc.iter().filter(|c| c.fear && c.until > self.clock) {
            if let Some(f) = self.chr_frame(c.ptr) {
                want.push(("feared", c.ptr, Follow::Chr(c.ptr), f));
            }
        }
        match self.ult {
            Some(Ult::Channel { .. }) => {
                want.push(("r_chest", 0, Follow::Bone("Chest"), cf));
                want.push(("r_hands", 0, Follow::Bone("R_Hand"), cf));
                want.push(("r_hands", 1, Follow::Bone("L_Hand"), cf));
                want.push(("r_ground", 0, Follow::Caster, cf));
            }
            Some(Ult::Storm { .. }) => want.push(("r_storm", 0, Follow::Caster, cf)),
            _ => {}
        }
        for (id, key, follow, at) in &want {
            if self.vfx_live.contains_key(&(*id, *key)) {
                continue;
            }
            if let Some((sys, unit)) = self.vfx_sys(id) {
                if let Some(h) = self.vfx.spawn(&self.c.vfx, sys, *follow, *at, unit, 120.0) {
                    self.vfx_live.insert((*id, *key), h);
                }
            }
        }
        let keep: HashSet<(&'static str, usize)> = want.iter().map(|w| (w.0, w.1)).collect();
        let vfx = &mut self.vfx;
        self.vfx_live.retain(|k, h| {
            let on = keep.contains(k) && vfx.alive(*h);
            if !on {
                vfx.stop(*h);
            }
            on
        });
    }

    fn attack_speed(&self) -> f32 {
        let lvl_as = self.stat("attack_speed_per_level") / 100.0 * growth(self.level);
        (self.stat("attack_speed") * (1.0 + lvl_as)).max(0.2)
    }

    /// The ability being channelled and how far along it is, for the cast bar.
    fn channel(&self) -> Option<(&'static str, f32)> {
        if let Some(d) = &self.drain {
            return Some(("Cosecha abundante", d.t / val(&self.c, 2, "DrainDuration", d.rank).max(0.01)));
        }
        match self.ult {
            Some(Ult::Channel { t, rank, .. }) => Some(("Tormenta de cuervos", t / val(&self.c, 4, "ChannelTime", rank).max(0.01))),
            Some(Ult::Storm { t, rank, .. }) => Some(("Tormenta de cuervos", 1.0 - t / val(&self.c, 4, "Duration", rank).max(0.01))),
            Some(Ult::Leap { .. }) => Some(("Tormenta de cuervos", 1.0)),
            None => None,
        }
    }

    fn build_hud(&self, scene: &mut Scene, me: &Chr) {
        let rect = |x: f32, y: f32, w: f32, h: f32, color: [f32; 4], tex: &'static str| Sprite {
            tex,
            additive: false,
            pos: [[x, y, 0.], [x + w, y, 0.], [x + w, y + h, 0.], [x, y + h, 0.]],
            uv: quad_uv(),
            color,
        };
        let out = &mut scene.screen;
        let gold = [0.78, 0.67, 0.43, 1.0];
        let gold_dim = [0.42, 0.36, 0.24, 1.0];
        let ink = [0.0, 0.0, 0.0, 0.85];
        let white = [0.96, 0.94, 0.90, 1.0];
        // League's bevelled frame: dark rim, gold line, dark gap
        let frame = |out: &mut Vec<Sprite>, x: f32, y: f32, w: f32, h: f32, line: [f32; 4]| {
            out.push(rect(x - 4.0, y - 4.0, w + 8.0, h + 8.0, [0.0, 0.0, 0.0, 0.9], "white"));
            out.push(rect(x - 2.5, y - 2.5, w + 5.0, h + 5.0, line, "white"));
            out.push(rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, [0.02, 0.03, 0.04, 1.0], "white"));
        };
        // vertical gradient in strips (a sprite carries one colour)
        let gradient = |out: &mut Vec<Sprite>, x: f32, y: f32, w: f32, h: f32, top: [f32; 4], bottom: [f32; 4]| {
            let n = 8;
            for i in 0..n {
                let t = i as f32 / (n - 1) as f32;
                let c = [0, 1, 2, 3].map(|k| top[k] + (bottom[k] - top[k]) * t);
                out.push(rect(x, y + h * i as f32 / n as f32, w, h / n as f32 + 0.5, c, "white"));
            }
        };
        // text with a drop shadow, centred on cx
        let label = |out: &mut Vec<Sprite>, s: &str, cx: f32, y: f32, size: f32, color: [f32; 4]| {
            out.extend(font::text(s, cx + 1.5, y + 1.5, size, ink));
            out.extend(font::text(s, cx, y, size, color));
        };
        for item in sheet::HUD {
            let (x, y, w, h) = (item.x, item.y, item.w, item.h);
            match item.kind {
                "rect" => {
                    frame(out, x, y, w, h, gold);
                    gradient(out, x, y, w, h, [0.07, 0.12, 0.14, 0.95], [0.01, 0.03, 0.04, 0.97]);
                    out.push(rect(x + 12.0, y + 3.0, w - 24.0, 1.0, [gold[0], gold[1], gold[2], 0.35], "white"));
                }
                "title" => {
                    label(out, "FIDDLESTICKS", x, y, h, gold);
                }
                "castbar" => {
                    let Some((name, f)) = self.channel() else { continue };
                    frame(out, x, y, w, h, gold_dim);
                    out.push(rect(x, y, w, h, [0.0, 0.0, 0.0, 0.85], "white"));
                    gradient(out, x, y, w * f.clamp(0.0, 1.0), h, [0.72, 0.35, 0.95, 1.0], [0.36, 0.12, 0.55, 1.0]);
                    label(out, name, x + w * 0.5, y + 2.0, h - 4.0, white);
                }
                "image" => {
                    let slot = item.binding.strip_prefix("ability:").and_then(|a| SLOTS.iter().position(|s| *s == a));
                    let Some(s) = slot else {
                        // portrait: the worn skin's icon
                        frame(out, x, y, w, h, gold);
                        let tex = if self.skin != 0 && self.c.icons.contains_key(skin_icon(self.skin)) { skin_icon(self.skin) } else { item.source };
                        out.push(rect(x, y, w, h, [1.0, 1.0, 1.0, 1.0], tex));
                        continue;
                    };
                    let ab = ability(s);
                    let rank = self.rank(s);
                    let cd = self.cd[s];
                    let off = !self.in_range[s] && !(s == 2 && self.drain.is_some());
                    let ready = rank > 0 && cd <= 0.0 && !off;
                    let line = if ready { gold } else if rank > 0 { gold_dim } else { [0.25, 0.25, 0.25, 1.0] };
                    frame(out, x, y, w, h, line);
                    let dim = if rank == 0 || off { 0.25 } else { 1.0 };
                    out.push(rect(x, y, w, h, [dim, dim, dim, 1.0], item.source));
                    if cd > 0.0 && rank > 0 {
                        // League's sweep, as a dark curtain that lifts, with a bright edge
                        let full = val(&self.c, s, "CooldownTime", rank).max(0.01);
                        let f = (cd / full).clamp(0.0, 1.0);
                        out.push(rect(x, y, w, h * f, [0.0, 0.0, 0.0, 0.7], "white"));
                        out.push(rect(x, y + h * f - 1.0, w, 2.0, [1.0, 0.9, 0.6, 0.8], "white"));
                        let txt = if cd < 1.0 { format!("{cd:.1}") } else { format!("{}", cd.ceil() as u32) };
                        label(out, &txt, x + w * 0.5, y + h * 0.27, h * 0.46, white);
                    }
                    // key badge
                    out.push(rect(x - 1.0, y + h - 17.0, 17.0, 18.0, [0.0, 0.0, 0.0, 0.85], "white"));
                    label(out, ["", "1", "2", "3", "4"][s], x + 7.5, y + h - 16.0, 15.0, [0.9, 0.85, 0.7, 1.0]);
                    // rank pips
                    let pw = (w + 4.0) / ab.max_rank as f32;
                    for r in 0..ab.max_rank {
                        let col = if r < rank { [0.95, 0.82, 0.35, 1.0] } else { [0.12, 0.12, 0.12, 1.0] };
                        out.push(rect(x - 2.0 + r as f32 * pw, y + h + 5.0, pw - 3.0, 4.0, col, "white"));
                    }
                    label(out, ab.name, x + w * 0.5, y + h + 12.0, 13.0, if rank > 0 { [0.85, 0.80, 0.68, 1.0] } else { [0.45, 0.45, 0.45, 1.0] });
                }
                "bar" => {
                    out.push(rect(x - 2.0, y - 2.0, w + 4.0, h + 4.0, [0.0, 0.0, 0.0, 0.9], "white"));
                    out.push(rect(x - 1.0, y - 1.0, w + 2.0, h + 2.0, gold_dim, "white"));
                    out.push(rect(x, y, w, h, [0.02, 0.02, 0.02, 1.0], "white"));
                    if item.binding == "hp_fraction" {
                        let frac = (me.hp.max(0) as f32 / me.max_hp.max(1) as f32).clamp(0.0, 1.0);
                        let (top, bottom) = if frac > 0.3 { ([0.35, 0.80, 0.30, 1.0], [0.10, 0.42, 0.10, 1.0]) } else { ([0.90, 0.30, 0.22, 1.0], [0.50, 0.08, 0.06, 1.0]) };
                        gradient(out, x, y, w * frac, h, top, bottom);
                        // League's ticks: thin every 100 HP, bold every 1000
                        let step = w * 100.0 / me.max_hp.max(1) as f32;
                        if step > 3.0 {
                            let mut i = 1;
                            while step * (i as f32) < w * frac {
                                let bold = i % 10 == 0;
                                out.push(rect(x + step * i as f32, y, if bold { 2.0 } else { 1.0 }, if bold { h } else { h * 0.45 }, [0.0, 0.0, 0.0, 0.6], "white"));
                                i += 1;
                            }
                        }
                        label(out, &format!("{} / {}", me.hp.max(0), me.max_hp), x + w * 0.5, y + 2.0, h - 4.0, white);
                    } else {
                        let lo = sheet::LEVELS[(self.level - 1) as usize].xp_total;
                        let hi = sheet::LEVELS.get(self.level as usize).map(|l| l.xp_total).unwrap_or(lo + 1.0);
                        let frac = if self.level >= 18 { 1.0 } else { (self.xp - lo) / (hi - lo).max(1.0) };
                        gradient(out, x, y, w * frac.clamp(0.0, 1.0), h, [0.75, 0.50, 0.98, 1.0], [0.40, 0.18, 0.62, 1.0]);
                    }
                }
                "text" => {
                    frame(out, x, y, w, h, gold);
                    out.push(rect(x, y, w, h, [0.03, 0.05, 0.07, 1.0], "white"));
                    label(out, &format!("{}", self.level), x + w * 0.5, y + 4.0, h - 8.0, white);
                }
                "stats" => {
                    // League's stat box: what his kit uses right now
                    frame(out, x, y, w, h, gold_dim);
                    out.push(rect(x, y, w, h, [0.0, 0.0, 0.0, 0.55], "white"));
                    let owned: Vec<&lolread::cache::ItemInfo> = self.items.iter().filter_map(|&i| self.item_info(i)).collect();
                    let haste: f32 = owned.iter().map(|i| i.haste).sum();
                    let (mut pen, mut pen_pct) = (0.0f32, 0.0f32);
                    for line in owned.iter().flat_map(|i| i.stat_lines.iter()).filter(|l| l.contains("penetración mágica")) {
                        let n = line.split_whitespace().next().unwrap_or("0");
                        let v: f32 = n.trim_end_matches('%').parse().unwrap_or(0.0);
                        if n.ends_with('%') { pen_pct += v } else { pen += v }
                    }
                    let ad = self.stat("ad") + self.stat("ad_per_level") * growth(self.level);
                    let pen_txt = match (pen > 0.0, pen_pct > 0.0) {
                        (true, true) => format!("{pen:.0} | {pen_pct:.0}%"),
                        (false, true) => format!("{pen_pct:.0}%"),
                        _ => format!("{pen:.0}"),
                    };
                    let rows = [
                        ("Daño de ataque", format!("{ad:.0}"), [0.95, 0.62, 0.30, 1.0]),
                        ("Poder de habilidad", format!("{:.0}", self.ap_total), [0.62, 0.55, 0.98, 1.0]),
                        ("Vel. de ataque", format!("{:.2}", self.attack_speed()), [0.95, 0.85, 0.40, 1.0]),
                        ("Vel. de habilidad", format!("{:.0}", haste.max(0.0)), [0.55, 0.85, 0.95, 1.0]),
                        ("Pen. mágica", pen_txt, [0.80, 0.45, 0.95, 1.0]),
                        ("Alcance", format!("{:.0}", self.stat("attack_range")), [0.75, 0.75, 0.70, 1.0]),
                    ];
                    let rh = h / rows.len() as f32;
                    for (i, (k, v, col)) in rows.iter().enumerate() {
                        let ry = y + rh * i as f32 + (rh - 13.0) * 0.5;
                        out.push(rect(x + 6.0, ry + 4.0, 5.0, 5.0, *col, "white"));
                        label(out, k, x + 15.0 + font::width(k, 13.0) * 0.5, ry, 13.0, [0.78, 0.75, 0.68, 1.0]);
                        label(out, v, x + w - 6.0 - font::width(v, 13.0) * 0.5, ry, 13.0, white);
                    }
                }
                "levelup" => {
                    for slot_item in sheet::HUD.iter().filter(|h| h.binding.starts_with("ability:")) {
                        let Some(s) = SLOTS.iter().position(|s| Some(*s) == slot_item.binding.strip_prefix("ability:")) else { continue };
                        if !self.can_rank(s) {
                            continue;
                        }
                        let (bx, by) = (slot_item.x + x, slot_item.y + y);
                        let pulse = 0.75 + 0.25 * (self.clock * 5.0).sin();
                        out.push(rect(bx - 2.0, by - 2.0, w + 4.0, h + 4.0, [gold[0] * pulse, gold[1] * pulse, gold[2] * pulse, 1.0], "white"));
                        out.push(rect(bx, by, w, h, [0.05, 0.12, 0.10, 0.95], "white"));
                        label(out, "+", bx + w * 0.5, by + 1.0, h - 2.0, [0.95, 0.85, 0.35, 1.0]);
                    }
                }
                _ => {}
            }
        }
        self.draw_items(out, &rect, &frame, &gradient, &label);
        if let Some((row, ch)) = self.skin_menu {
            self.draw_skin_menu(out, row, ch, &rect, &frame, &gradient, &label);
        }
    }
}

/// Texture key of a skin's round icon (static: sprites hold &'static str keys).
fn skin_icon(n: u32) -> &'static str {
    static KEYS: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| (0..100).map(|i| &*Box::leak(format!("skin_icon_{i}").into_boxed_str())).collect())
        .get(n as usize)
        .copied()
        .unwrap_or("white")
}

fn skin_choice_path() -> std::path::PathBuf {
    crate::log::data_dir().join("skin.txt")
}

fn skin_cache_path(n: u32) -> std::path::PathBuf {
    crate::log::data_dir().join("cache").join("skins").join(format!("skin{n}.bin"))
}

// Skins: F2 opens a picker; a skin not built yet is built by fiddlering-prep --skin N in the background.
impl Game {
    /// Skins (not chromas), in League's order.
    fn skin_rows(&self) -> Vec<u32> {
        self.base.skins.iter().filter(|s| s.parent == s.num).map(|s| s.num).collect()
    }

    fn chromas(&self, parent: u32) -> Vec<u32> {
        self.base.skins.iter().filter(|s| s.parent == parent && s.num != parent).map(|s| s.num).collect()
    }

    fn skin_name(&self, n: u32) -> &str {
        self.base.skins.iter().find(|s| s.num == n).map(|s| s.name.as_str()).unwrap_or("Fiddlesticks")
    }

    fn menu_pick(&self, row: usize, ch: usize) -> u32 {
        let rows = self.skin_rows();
        let p = rows.get(row).copied().unwrap_or(0);
        if ch == 0 { p } else { self.chromas(p).get(ch - 1).copied().unwrap_or(p) }
    }

    fn update_skins(&mut self) {
        // a background build finished
        if let Some((n, rx)) = &self.skin_job {
            let n = *n;
            match rx.try_recv() {
                Ok(ok) => {
                    self.skin_job = None;
                    match std::fs::read(skin_cache_path(n)).ok().and_then(|b| lolread::cache::load_skin(&b).ok()).filter(|_| ok) {
                        Some(sc) => self.apply_skin(Some(sc)),
                        None => self.skin_note = Some(("No se pudo preparar esa skin (mira fiddlering.log)".into(), self.clock + 4.0)),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.skin_job = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        let focused = game_focused();
        let keys = [VK_F2, VK_UP, VK_DOWN, VK_LEFT, VK_RIGHT, VK_RETURN].map(|k| focused && key_down(k.0 as i32));
        let pressed: Vec<bool> = (0..6).map(|i| keys[i] && !self.menu_keys[i]).collect();
        self.menu_keys = keys;
        let rows = self.skin_rows();
        if rows.is_empty() {
            return;
        }
        if pressed[0] {
            self.skin_menu = match self.skin_menu {
                Some(_) => None,
                None => {
                    // open on what is worn
                    let parent = self.base.skins.iter().find(|s| s.num == self.skin).map(|s| s.parent).unwrap_or(0);
                    let row = rows.iter().position(|&r| r == parent).unwrap_or(0);
                    let ch = self.chromas(parent).iter().position(|&c| c == self.skin).map(|i| i + 1).unwrap_or(0);
                    Some((row, ch))
                }
            };
        }
        let Some((mut row, mut ch)) = self.skin_menu else { return };
        let n = rows.len();
        if pressed[1] {
            row = (row + n - 1) % n;
            ch = 0;
        }
        if pressed[2] {
            row = (row + 1) % n;
            ch = 0;
        }
        let k = self.chromas(rows[row]).len() + 1;
        if pressed[3] {
            ch = (ch + k - 1) % k;
        }
        if pressed[4] {
            ch = (ch + 1) % k;
        }
        self.skin_menu = Some((row, ch));
        if pressed[5] {
            let pick = self.menu_pick(row, ch);
            self.choose_skin(pick);
        }
    }

    /// Wears skin `n`: at once if it is built, else after building it in the background.
    fn choose_skin(&mut self, n: u32) {
        if n == self.skin && self.skin_job.is_none() {
            return;
        }
        if n == 0 {
            self.apply_skin(None);
            return;
        }
        if let Some(sc) = std::fs::read(skin_cache_path(n)).ok().and_then(|b| lolread::cache::load_skin(&b).ok()) {
            self.apply_skin(Some(sc));
            return;
        }
        if self.skin_job.is_some() {
            return;
        }
        let prep = crate::dll_dir().join("fiddlering-prep.exe");
        let (tx, rx) = std::sync::mpsc::channel();
        log!("SKIN {n}: building with {}", prep.display());
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(&prep);
            cmd.arg("--skin").arg(n.to_string());
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            let ok = match cmd.output() {
                Ok(o) => {
                    log!("SKIN {n}: {}{}", String::from_utf8_lossy(&o.stdout).trim(), String::from_utf8_lossy(&o.stderr).trim());
                    o.status.success()
                }
                Err(e) => {
                    log!("SKIN {n}: prep failed to start: {e}");
                    false
                }
            };
            let _ = tx.send(ok);
        });
        self.skin_job = Some((n, rx));
    }

    /// Swaps Fiddlesticks' model, animations and sounds (None = the base skin) and redraws him.
    fn apply_skin(&mut self, sc: Option<lolread::cache::SkinCache>) {
        let mut c = (*self.base).clone();
        let n = sc.as_ref().map(|s| s.num).unwrap_or(0);
        if let Some(s) = sc {
            c.model = s.model;
            c.clips = s.clips;
            c.sounds.extend(s.sounds);
            c.icons.extend(s.fx);
            c.vfx = s.vfx;
        }
        self.c = Arc::new(c);
        crate::gfx::set_cache(self.c.clone());
        crate::gfx::reset();
        if let Some(a) = self.audio.as_mut() {
            a.set_sounds(&self.c.sounds);
        }
        self.skin = n;
        let parent = self.base.skins.iter().find(|s| s.num == n).map(|s| s.parent).unwrap_or(n);
        self.theme = lolread::theme::for_skin(n, parent, self.skin_name(n));
        // a chroma recolours its skin's own League effects, from the skin's colour to the chroma's
        self.vfx_theme = (n != parent).then(|| lolread::theme::for_skin(n, parent, self.skin_name(n))).flatten().map(|c| {
            let from = lolread::theme::for_skin(parent, parent, self.skin_name(parent)).map(|p| p.hue.max(0.0)).unwrap_or(0.0);
            lolread::theme::Theme { hue: if c.hue >= 0.0 { (c.hue - from).rem_euclid(360.0) } else { -1.0 }, ..c }
        });
        self.vfx.clear();
        self.vfx_live.clear();
        let _ = std::fs::write(skin_choice_path(), n.to_string());
        log!("SKIN {n} worn: {} ({} vertices)", self.skin_name(n), self.c.model.vertices.len());
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_skin_menu(
        &self,
        out: &mut Vec<Sprite>,
        row: usize,
        ch: usize,
        rect: &dyn Fn(f32, f32, f32, f32, [f32; 4], &'static str) -> Sprite,
        frame: &dyn Fn(&mut Vec<Sprite>, f32, f32, f32, f32, [f32; 4]),
        gradient: &dyn Fn(&mut Vec<Sprite>, f32, f32, f32, f32, [f32; 4], [f32; 4]),
        label: &dyn Fn(&mut Vec<Sprite>, &str, f32, f32, f32, [f32; 4]),
    ) {
        let gold = [0.78, 0.67, 0.43, 1.0];
        let gold_dim = [0.42, 0.36, 0.24, 1.0];
        let white = [0.96, 0.94, 0.90, 1.0];
        let grey = [0.70, 0.67, 0.60, 1.0];
        let left = |out: &mut Vec<Sprite>, s: &str, x: f32, y: f32, size: f32, color: [f32; 4]| label(out, s, x + font::width(s, size) * 0.5, y, size, color);
        let icon = |out: &mut Vec<Sprite>, n: u32, x: f32, y: f32, size: f32, dim: f32| {
            let key = if self.c.icons.contains_key(skin_icon(n)) { skin_icon(n) } else { "icon_portrait" };
            out.push(rect(x, y, size, size, [dim, dim, dim, 1.0], key));
        };

        out.push(rect(0.0, 0.0, 1920.0, 1080.0, [0.0, 0.0, 0.0, 0.6], "white"));
        let (px, py, pw, ph) = (460.0, 170.0, 1000.0, 700.0);
        frame(out, px, py, pw, ph, gold);
        gradient(out, px, py, pw, ph, [0.07, 0.12, 0.14, 0.97], [0.01, 0.03, 0.04, 0.98]);
        label(out, "SKINS DE FIDDLESTICKS", 960.0, py + 18.0, 30.0, gold);
        out.push(rect(px + 40.0, py + 60.0, pw - 80.0, 1.0, [gold[0], gold[1], gold[2], 0.5], "white"));

        // the list of skins
        let rows = self.skin_rows();
        let worn_parent = self.base.skins.iter().find(|s| s.num == self.skin).map(|s| s.parent).unwrap_or(0);
        let (lx, ly, rh) = (px + 24.0, py + 76.0, 44.0);
        for (i, &n) in rows.iter().enumerate() {
            let y = ly + rh * i as f32;
            if i == row {
                out.push(rect(lx - 4.0, y - 2.0, 400.0, rh - 4.0, [0.78, 0.67, 0.43, 0.22], "white"));
                out.push(rect(lx - 4.0, y - 2.0, 3.0, rh - 4.0, gold, "white"));
            }
            icon(out, n, lx + 4.0, y + 1.0, 36.0, 1.0);
            let name = self.skin_name(n);
            left(out, name, lx + 50.0, y + 10.0, 17.0, if i == row { white } else { grey });
            if n == worn_parent {
                out.push(rect(lx + 384.0, y + 15.0, 8.0, 8.0, gold, "white"));
            }
        }
        out.push(rect(px + 440.0, py + 76.0, 1.0, ph - 140.0, [gold[0], gold[1], gold[2], 0.35], "white"));

        // the chosen one, big
        let pick = self.menu_pick(row, ch);
        let cx = px + 720.0;
        frame(out, cx - 130.0, py + 90.0, 260.0, 260.0, gold);
        icon(out, pick, cx - 130.0, py + 90.0, 260.0, 1.0);
        label(out, self.skin_name(pick), cx, py + 372.0, 24.0, white);
        let status = if self.skin_job.as_ref().map(|j| j.0) == Some(pick) {
            ("Preparando la skin desde tu League...".to_string(), [0.95, 0.82, 0.35, 1.0])
        } else if let Some((msg, until)) = self.skin_note.as_ref().filter(|n| n.1 > self.clock) {
            let _ = until;
            (msg.clone(), [0.95, 0.40, 0.35, 1.0])
        } else if pick == self.skin {
            ("Equipada".to_string(), [0.55, 0.85, 0.45, 1.0])
        } else {
            ("Enter para equipar".to_string(), grey)
        };
        label(out, &status.0, cx, py + 408.0, 16.0, status.1);

        // its chromas
        let parent = rows.get(row).copied().unwrap_or(0);
        let chromas = self.chromas(parent);
        if !chromas.is_empty() {
            label(out, "Cromas", cx, py + 452.0, 17.0, gold);
            let all: Vec<u32> = std::iter::once(parent).chain(chromas.iter().copied()).collect();
            let step = (500.0 / all.len() as f32).min(62.0);
            let size = step - 10.0;
            let x0 = cx - step * all.len() as f32 * 0.5 + 5.0;
            for (i, &n) in all.iter().enumerate() {
                let x = x0 + step * i as f32;
                let y = py + 482.0;
                if i == ch {
                    frame(out, x, y, size, size, gold);
                } else {
                    out.push(rect(x - 2.0, y - 2.0, size + 4.0, size + 4.0, gold_dim, "white"));
                }
                icon(out, n, x, y, size, if i == ch { 1.0 } else { 0.7 });
                if n == self.skin {
                    out.push(rect(x + size * 0.5 - 4.0, y + size + 6.0, 8.0, 8.0, gold, "white"));
                }
            }
        }

        label(out, "Arriba/Abajo: skin  ·  Izquierda/Derecha: croma  ·  Enter: equipar  ·  F2: cerrar", 960.0, py + ph - 40.0, 16.0, grey);
    }
}

/// Words of `text` in lines no wider than `width` at glyph height `size`.
fn wrap(text: &str, width: f32, size: f32) -> Vec<String> {
    let mut lines = vec![];
    for para in text.lines() {
        let mut line = String::new();
        for w in para.split_whitespace() {
            let cand = if line.is_empty() { w.to_string() } else { format!("{line} {w}") };
            if font::width(&cand, size) > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line = w.to_string();
            } else {
                line = cand;
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}

fn item_icon(id: u32) -> &'static str {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static KEYS: Mutex<Option<HashMap<u32, &'static str>>> = Mutex::new(None);
    let mut g = KEYS.lock().unwrap();
    g.get_or_insert_with(HashMap::new).entry(id).or_insert_with(|| Box::leak(format!("item_{id}").into_boxed_str()))
}

// League items: a boss dies -> pick one of three; their stats and passives feed the kit; F3 lists them.
impl Game {
    fn item_info(&self, id: u32) -> Option<&lolread::cache::ItemInfo> {
        self.c.items.iter().find(|i| i.id == id)
    }

    /// Sum of an items-sheet effect over the items owned.
    fn item_effect(&self, effect: &str) -> f32 {
        self.items.iter().filter_map(|id| sheet::ITEMS.iter().find(|r| r.id == *id)).filter(|r| r.effect == effect).map(|r| r.value).sum()
    }

    /// Bosses with a health bar on screen; when one dies and every bar has gone, three items are offered.
    fn track_bosses(&mut self, wcm: &WorldChrMan) {
        let Ok(fe) = (unsafe { eldenring::cs::CSFeManImp::instance() }) else { return };
        // the game's full-screen banners ("enemy felled", "you died"...): asked for even while its HUD is hidden
        let banner = fe.frontend_values.full_screen_message_request_id;
        if banner != self.banner_id {
            log!("FE banner {} -> {banner}", self.banner_id);
            self.banner_id = banner;
            if banner > 0 {
                self.banner_at = self.clock;
            }
        }
        if self.banner_at > 0.0 && self.strong_kill_at > 0.0 && (self.banner_at - self.strong_kill_at).abs() < tuning::BOSS_BANNER_WINDOW {
            log!("BOSS defeated (banner {} with a strong enemy's death)", self.banner_id);
            self.banner_at = -100.0;
            self.strong_kill_at = -100.0;
            self.boss_died = true;
        }
        let mut any_bar = false;
        for e in fe.boss_health_displays.iter() {
            if e.fmg_id != 0 && e.fmg_id != -1 && self.frame % 300 == 0 {
                log!("boss display entry: fmg {} dmg {}", e.fmg_id, e.damage_taken);
            }
            if e.fmg_id <= 0 {
                continue;
            }
            if let Some(c) = wcm.chr_ins_by_handle(&e.field_ins_handle) {
                any_bar = true;
                let p = c as *const ChrIns as usize;
                if self.boss_ptrs.insert(p) {
                    log!("BOSS bar: chr {} (fmg {})", c.character_id, e.fmg_id);
                }
            }
        }
        let alive = &self.alive_ptrs;
        let mut died = false;
        self.boss_ptrs.retain(|&p| {
            if !alive.contains(&p) {
                return false;
            }
            if chr_mut(p).modules.data.hp <= 0 {
                died = true;
                return false;
            }
            true
        });
        if died {
            log!("BOSS defeated");
            self.boss_died = true;
        }
        if any_bar {
            self.bars_clear_at = self.clock;
        } else if self.boss_died && self.clock - self.bars_clear_at > tuning::ITEM_OFFER_DELAY {
            self.boss_died = false;
            self.offer_items();
        }
    }

    /// Three items not owned yet, picked at random.
    fn offer_items(&mut self) {
        if self.items.len() >= 6 || self.offer.is_some() {
            return;
        }
        let mut pool: Vec<u32> = sheet::ITEMS.iter().map(|r| r.id).filter(|id| !self.items.contains(id) && self.item_info(*id).is_some()).collect();
        if pool.len() < 3 {
            return;
        }
        let mut seed = (self.clock * 1000.0) as u64 ^ 0x9e37_79b9;
        let mut pick = [0u32; 3];
        for p in pick.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *p = pool.remove((seed % pool.len() as u64) as usize);
        }
        log!("ITEMS offered: {pick:?}");
        self.offer = Some((pick, 1));
    }

    fn update_items(&mut self) {
        let focused = game_focused();
        let keys = [VK_F3, VK_LEFT, VK_RIGHT, VK_RETURN].map(|k| focused && key_down(k.0 as i32));
        let pressed: Vec<bool> = (0..4).map(|i| keys[i] && !self.item_keys[i]).collect();
        self.item_keys = keys;
        if self.test && self.clock > 70.0 && self.items.is_empty() && self.offer.is_none() {
            self.offer_items(); // test runs show the choice without a boss
        }
        if let Some((pick, mut sel)) = self.offer {
            if pressed[1] {
                sel = (sel + 2) % 3;
            }
            if pressed[2] {
                sel = (sel + 1) % 3;
            }
            if pressed[3] {
                self.items.push(pick[sel]);
                self.offer = None;
                self.dirty = true;
                self.save();
                log!("ITEM taken: {} ({} owned)", pick[sel], self.items.len());
                return;
            }
            self.offer = Some((pick, sel));
            return;
        }
        if pressed[0] {
            self.inv_open = if self.inv_open.is_some() { None } else { Some(0) };
        }
        if let Some(mut sel) = self.inv_open {
            let n = self.items.len().max(1);
            if pressed[1] {
                sel = (sel + n - 1) % n;
            }
            if pressed[2] {
                sel = (sel + 1) % n;
            }
            self.inv_open = Some(sel);
        }
    }

    /// Item burns: a share of the target's max HP per second while they last.
    fn update_burns(&mut self, dt: f32) {
        let clock = self.clock;
        let alive = self.alive_ptrs.clone();
        let mut hits = vec![];
        for b in self.burns.iter_mut() {
            if !alive.contains(&b.0) || b.1 <= clock {
                continue;
            }
            b.3 += b.2 * chr_mut(b.0).modules.data.max_hp as f32 * dt;
            if b.3 >= 1.0 {
                hits.push((b.0, b.3));
                b.3 = 0.0;
            }
        }
        self.burns.retain(|b| b.1 > clock && alive.contains(&b.0));
        for (p, dmg) in hits {
            let ch = read_chr(chr_mut(p));
            self.damage(ch, dmg / tuning::DAMAGE_SCALE.max(0.001), "BURN");
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_items(
        &self,
        out: &mut Vec<Sprite>,
        rect: &dyn Fn(f32, f32, f32, f32, [f32; 4], &'static str) -> Sprite,
        frame: &dyn Fn(&mut Vec<Sprite>, f32, f32, f32, f32, [f32; 4]),
        gradient: &dyn Fn(&mut Vec<Sprite>, f32, f32, f32, f32, [f32; 4], [f32; 4]),
        label: &dyn Fn(&mut Vec<Sprite>, &str, f32, f32, f32, [f32; 4]),
    ) {
        let gold = [0.78, 0.67, 0.43, 1.0];
        let gold_dim = [0.42, 0.36, 0.24, 1.0];
        let white = [0.96, 0.94, 0.90, 1.0];
        let grey = [0.70, 0.67, 0.60, 1.0];
        let left = |out: &mut Vec<Sprite>, s: &str, x: f32, y: f32, size: f32, color: [f32; 4]| label(out, s, x + font::width(s, size) * 0.5, y, size, color);
        let icon = |out: &mut Vec<Sprite>, id: u32, x: f32, y: f32, size: f32| {
            let key = if self.c.icons.contains_key(item_icon(id)) { item_icon(id) } else { "white" };
            out.push(rect(x, y, size, size, [1.0, 1.0, 1.0, 1.0], key));
        };
        // the 6 slots in the HUD
        if let Some(h) = sheet::HUD.iter().find(|h| h.kind == "items") {
            for i in 0..6 {
                let (x, y) = (h.x + (i % 3) as f32 * 36.0, h.y + (i / 3) as f32 * 36.0);
                out.push(rect(x - 1.0, y - 1.0, 34.0, 34.0, gold_dim, "white"));
                out.push(rect(x, y, 32.0, 32.0, [0.02, 0.03, 0.04, 1.0], "white"));
                if let Some(&id) = self.items.get(i) {
                    icon(out, id, x, y, 32.0);
                }
            }
        }
        // a card: icon, name, stats, passive
        let card = |out: &mut Vec<Sprite>, id: u32, x: f32, y: f32, w: f32, h: f32, chosen: bool| {
            frame(out, x, y, w, h, if chosen { gold } else { gold_dim });
            gradient(out, x, y, w, h, [0.07, 0.12, 0.14, 0.97], [0.01, 0.03, 0.04, 0.98]);
            let Some(info) = self.item_info(id) else { return };
            icon(out, id, x + w * 0.5 - 40.0, y + 16.0, 80.0);
            let mut ty = y + 108.0;
            for line in wrap(&info.name, w - 24.0, 20.0) {
                label(out, &line, x + w * 0.5, ty, 20.0, gold);
                ty += 24.0;
            }
            ty += 4.0;
            for s in &info.stat_lines {
                left(out, s, x + 14.0, ty, 15.0, white);
                ty += 19.0;
            }
            ty += 6.0;
            for line in wrap(&info.text, w - 28.0, 13.0).into_iter().take(((y + h - ty - 10.0) / 16.0).max(0.0) as usize) {
                left(out, &line, x + 14.0, ty, 13.0, grey);
                ty += 16.0;
            }
        };
        if let Some((pick, sel)) = self.offer {
            out.push(rect(0.0, 0.0, 1920.0, 1080.0, [0.0, 0.0, 0.0, 0.55], "white"));
            label(out, "JEFE DERROTADO: ELIGE UN OBJETO", 960.0, 255.0, 30.0, gold);
            for (i, &id) in pick.iter().enumerate() {
                card(out, id, 960.0 - 490.0 + i as f32 * 340.0, 310.0, 300.0, 380.0, i == sel);
            }
            label(out, "Izquierda/Derecha: elegir  ·  Enter: quedarte con él", 960.0, 720.0, 17.0, grey);
        } else if let Some(sel) = self.inv_open {
            out.push(rect(0.0, 0.0, 1920.0, 1080.0, [0.0, 0.0, 0.0, 0.45], "white"));
            frame(out, 560.0, 180.0, 800.0, 600.0, gold);
            gradient(out, 560.0, 180.0, 800.0, 600.0, [0.07, 0.12, 0.14, 0.97], [0.01, 0.03, 0.04, 0.98]);
            label(out, "OBJETOS", 960.0, 198.0, 28.0, gold);
            if self.items.is_empty() {
                label(out, "Todavía no tienes objetos: derrota a un jefe para elegir uno.", 960.0, 420.0, 18.0, grey);
            }
            for i in 0..6 {
                let x = 960.0 - 3.0 * 76.0 + i as f32 * 76.0 + 6.0;
                frame(out, x, 250.0, 64.0, 64.0, if i == sel && i < self.items.len() { gold } else { gold_dim });
                out.push(rect(x, 250.0, 64.0, 64.0, [0.02, 0.03, 0.04, 1.0], "white"));
                if let Some(&id) = self.items.get(i) {
                    icon(out, id, x, 250.0, 64.0);
                }
            }
            if let Some(&id) = self.items.get(sel) {
                card(out, id, 760.0, 340.0, 400.0, 380.0, true);
            }
            label(out, "Izquierda/Derecha: ver  ·  F3: cerrar", 960.0, 752.0, 16.0, grey);
        }
    }
}
