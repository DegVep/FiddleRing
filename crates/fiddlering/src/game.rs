//! Fiddlesticks in the Lands Between: reads the Tarnished, the camera and nearby characters each frame, runs his
//! kit (abilities sheet, numbers from the player's League data), League levelling, and hands the frame to
//! the renderer. Runs on the game thread from Elden Ring's own task loop.

use crate::anim::{self, Anim};
use crate::audio::Audio;
use crate::font;
use crate::log;
use crate::scene::{ModelDraw, SCENE, Scene, Sprite, quad_uv};
use eldenring::cs::{CSCamera, ChrIns, WorldChrMan};
use fromsoftware_shared::FromStatic;
use lolread::cache::{Cache, Spell};
use lolread::math::M4;
use lolread::sheet::{self, tuning};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};
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
    dt: f32,
    ui_seen: u128,
    hud_seen: i32,
    last_close: f32,
    dbg_last: u64,
}

fn progress_path() -> std::path::PathBuf {
    crate::log::data_dir().join("progress.txt")
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
        let (mut level, mut xp, mut ranks) = (1u32, 0f32, [1u32, 0, 0, 0, 0]);
        if let Ok(t) = std::fs::read_to_string(progress_path()) {
            let mut it = t.split_whitespace();
            level = it.next().and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 18);
            xp = it.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
            for r in ranks.iter_mut().skip(1) {
                *r = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            }
        }
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
        let g = Game {
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
            hidden_logged: false,
            dt: 0.0,
            ui_seen: 0,
            hud_seen: -2,
            last_close: 0.0,
            dbg_last: 0,
        };
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
        let r = &self.ranks;
        let _ = std::fs::write(progress_path(), format!("{} {:.1} {} {} {} {}\n", self.level, self.xp, r[1], r[2], r[3], r[4]));
        self.dirty = false;
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

    pub fn block_inputs() {
        unsafe {
            if let Ok(w) = WorldChrMan::instance_mut() {
                if let Some(p) = w.main_player.as_deref_mut() {
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
        let player_ptr = &player.chr_ins as *const ChrIns as usize;
        let me = read_chr(&player.chr_ins);
        let ap = unsafe { player.player_game_data.as_ref().level.saturating_sub(1) } as f32 * tuning::AP_PER_RUNE_LEVEL;

        // hide the Tarnished under Fiddlesticks
        player.chr_ins.base_transparency = 0.0;
        player.chr_ins.opacity_keyframes_multiplier = 0.0;
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
        self.update_drain(dt, &me, player_ptr, player_level_ap);
        // Bountiful Harvest: the Tarnished drags their feet while it drains (root motion follows animation speed)
        if self.drain.is_some() || self.w_slowed {
            self.w_slowed = self.drain.is_some();
            chr_mut(player_ptr).modules.behavior.animation_speed = if self.w_slowed { tuning::W_SELF_SLOW } else { 1.0 };
        }
        self.update_ult(dt, &me, forward, player_ptr, &hostiles, player_level_ap);
        self.update_cc();

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
            _ => val(&self.c, slot, "CooldownTime", rank),
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
                    self.fx.burst("aa_melee", "hit", add(t.pos, [0.0, t.height * 0.6, 0.0]), dir, 0.0);
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
                self.fx.burst("e_reap", "area", p.point, dir, radius);
                log!("E at {:?} (player {:?}) radius {radius:.1}", p.point, me.pos);
                let mut hit = false;
                for h in hostiles.iter().filter(|h| flat(sub(h.pos, p.point)) <= radius) {
                    self.damage(*h, dmg, "E");
                    self.fx.burst("e_reap", "hit", add(h.pos, [0.0, h.height * 0.5, 0.0]), [0.0, 1.0, 0.0], 0.0);
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
        let trails: Vec<(&'static str, usize, V3, V3)> = self.missiles.iter().map(|m| (m.group, m.id, m.pos, m.dir)).collect();
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
                self.fx.burst("aa_missile", "hit", hit_at, mul(m.dir, -1.0), 0.0);
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
                self.fx.burst("q_fear", "hit", hit_at, mul(m.dir, -1.0), 0.0);
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
                self.fx.burst("w_drain", "tick", add(t.pos, [0.0, t.height * 0.6, 0.0]), norm(sub(me.pos, t.pos)), 0.0);
            }
        }
        for t in &alive {
            let chest = add(t.pos, [0.0, t.height * 0.6, 0.0]);
            self.fx.stream("w_drain", "tether_flow", t.ptr, chest, norm(sub(me.pos, t.pos)), 0.0, dt);
            self.fx.stream("w_drain", "victim", t.ptr, add(t.pos, [0.0, t.height * 0.5, 0.0]), [0.0, 1.0, 0.0], 0.0, dt);
        }
        self.fx.stream("w_drain", "channel", 0, me.pos, [0.0, 1.0, 0.0], 0.0, dt);
        if healed > 0.0 {
            self.fx.burst("w_drain", "heal", add(me.pos, [0.0, 1.0, 0.0]), [0.0, 1.0, 0.0], 0.0);
            let p = chr_mut(player_ptr);
            p.modules.data.hp = (p.modules.data.hp + healed.round() as i32).min(p.modules.data.max_hp);
        }
        if finished {
            if finished && !alive.is_empty() {
                self.sound("sfx_w_final");
                for t in &alive {
                    self.fx.burst("w_drain", "final", add(t.pos, [0.0, t.height * 0.6, 0.0]), [0.0, 1.0, 0.0], 0.0);
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
                self.fx.stream("r_crowstorm", "r_channel", 0, me.pos, [0.0, 1.0, 0.0], 0.0, dt);
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
                self.fx.stream("r_crowstorm", "storm", 1, me.pos, [0.0, 1.0, 0.0], radius, dt);
                if tick >= 1.0 / tuning::R_TICKS_PER_SECOND {
                    tick = 0.0;
                    let per = (val(&self.c, 4, "DamagePerSecond", rank) + val(&self.c, 4, "APRatio", rank) * ap) / tuning::R_TICKS_PER_SECOND;
                    for h in hostiles.iter().filter(|h| flat(sub(h.pos, me.pos)) <= radius) {
                        self.damage(*h, per, "R");
                        self.fx.burst("r_crowstorm", "hit", add(h.pos, [0.0, h.height * 0.6, 0.0]), [0.0, 1.0, 0.0], 0.0);
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
                self.fx.stream("q_fear", "feared", p, add(ch.pos, [0.0, ch.height + 0.3, 0.0]), [0.0, 1.0, 0.0], 0.0, dt);
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

        let yaw = face[0].atan2(face[2]) + tuning::MODEL_YAW_DEG.to_radians();
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

        let mut scene = Scene { models: vec![model], cam: cam_m, fov: pc.fov, near: pc.near_plane, far: pc.far_plane, world: vec![], screen: vec![], shot: None };
        let view = crate::fx::View { right, up, pos: cam_pos };
        let mut crows: Vec<crate::fx::Crow> = vec![];

        // particles (effects sheet)
        self.fx.update(self.dt, me.pos, soul);
        let radii = [("w_drain", self.effect_radius("w_drain")), ("e_reap", self.effect_radius("e_reap")), ("r_crowstorm", self.effect_radius("r_crowstorm"))];
        let radius_of = |g: &str| radii.iter().find(|r| r.0 == g).map(|r| r.1).unwrap_or(1.0);
        self.fx.draw(&view, radius_of, &mut scene.world, &mut crows);

        // missiles: their heads are emitter rows too (a glowing bolt, or a real crow for Terrify)
        for m in &self.missiles {
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
        if let Some(d) = &self.drain {
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

        self.build_hud(&mut scene, &me);
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

    /// Bountiful Harvest: three writhing tendrils from his chest to a victim, thick at the victim, with soul
    /// pulses running up them to him.
    fn draw_tendrils(&self, from: V3, to: V3, seed: f32, view: &crate::fx::View, out: &mut Vec<Sprite>) {
        const SEGS: usize = 12;
        let d = sub(to, from);
        let side = norm(cross(d, [0.0, 1.0, 0.0]));
        let side = if len(side) < 0.5 { [1.0, 0.0, 0.0] } else { side };
        let lift = norm(cross(side, d));
        let reach = len(d);
        for k in 0..3 {
            let ph = seed * 0.37 + k as f32 * 2.1;
            let amp = (0.18 + 0.05 * reach.min(8.0)) * (0.7 + 0.3 * k as f32);
            let at = |t: f32| -> V3 {
                let bow = (t * std::f32::consts::PI).sin();
                let a = amp * bow * (self.clock * 5.5 + ph + t * 7.0).sin();
                let b = amp * 0.7 * bow * (self.clock * 4.3 + ph * 1.7 + t * 5.0).cos() + 0.25 * bow;
                add(add(from, mul(d, t)), add(mul(side, a), mul(lift, b)))
            };
            let pts: Vec<V3> = (0..=SEGS).map(|i| at(i as f32 / SEGS as f32)).collect();
            for e in crate::fx::rows("w_drain", "tether_beam") {
                for i in 0..SEGS {
                    // thin where it leaves him, thick where it grips the victim
                    let taper = 0.45 + 0.75 * (i as f32 + 0.5) / SEGS as f32;
                    let pulse = 1.0 + 0.2 * (self.clock * 9.0 + ph + i as f32).sin();
                    let scroll = -self.clock * 3.0 + i as f32 * 0.35;
                    out.push(crate::fx::beam(e.texture, e.additive, pts[i], pts[i + 1], e.size_start * taper * pulse, e.color_start, scroll, view));
                }
            }
            for e in crate::fx::rows("w_drain", "tether_pulse") {
                for n in 0..2 {
                    // victim (t = 1) to him (t = 0)
                    let t = 1.0 - (self.clock * 1.4 + ph * 0.3 + n as f32 * 0.5).fract();
                    let mut c = e.color_start;
                    c[3] *= (t * std::f32::consts::PI).sin();
                    out.push(crate::fx::billboard(e.texture, e.additive, at(t), e.size_start, 0.0, c, quad_uv(), view));
                }
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
                        // portrait
                        frame(out, x, y, w, h, gold);
                        out.push(rect(x, y, w, h, [1.0, 1.0, 1.0, 1.0], item.source));
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
    }
}
