"""Design sheets -> preflight report and generated Rust (crates/lolread/src/sheet.rs).

    python -I scripts/sheets.py preflight [--release] [--verify cache/verify.json]
    python -I scripts/sheets.py gen

Preflight lays every sheet over the others: unfilled cells, references between sheets that don't resolve,
League facts the prep step could not find in the player's install (from verify.json) and, with --release,
Elden Ring hooks not yet verified in the running game.
"""
import json
import re
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SHEETS = ROOT / "sheets"


def load():
    return {p.stem: json.loads(p.read_text(encoding="utf-8")) for p in sorted(SHEETS.glob("*.json"))}


def ids(sheet):
    return {r["id"] for r in sheet["rows"]}


def preflight(release=False, verify_path=None):
    s = load()
    problems = []

    # 1. every row x column crossing is filled
    for name, sh in s.items():
        for i, row in enumerate(sh["rows"]):
            for col in sh["columns"]:
                v = row.get(col)
                if v is None or (isinstance(v, str) and v.strip() == ""):
                    problems.append(f"{name}[{row.get('id', i)}].{col}: unfilled")
            for col in row:
                if col not in sh["columns"]:
                    problems.append(f"{name}[{row.get('id', i)}].{col}: column not declared")

    # 2. references between sheets
    anims, sounds, assets, abil = ids(s["animations"]), ids(s["sounds"]), ids(s["league_assets"]), ids(s["abilities"])

    def ref(where, value, pool, allow=("none",)):
        if value not in pool and value not in allow:
            problems.append(f"{where}: '{value}' does not resolve")

    for r in s["abilities"]["rows"]:
        w = f"abilities[{r['id']}]"
        ref(w + ".anim", r["anim"], anims)
        for c in ("vo", "sfx_cast", "sfx_hit"):
            ref(f"{w}.{c}", r[c], sounds)
        ref(w + ".icon", r["icon"], assets)
    for r in s["animations"]["rows"]:
        ref(f"animations[{r['id']}].next", r["next"], anims)
    for r in s["hud"]["rows"]:
        ref(f"hud[{r['id']}].source", r["source"], assets, ("computed",))
        b = r["binding"]
        if b.startswith("ability:"):
            ref(f"hud[{r['id']}].binding", b.split(":", 1)[1], abil)
    for r in s["progression"]["rows"]:
        ref(f"progression[{r['level']}].rank_up", r["rank_up"], abil)
    act = {r["id"]: r for r in s["er_actions"]["rows"]}
    for r in s["abilities"]["rows"]:
        a = act.get(r["er_action"])
        if a is None or a["use"] != "Fiddlesticks":
            problems.append(f"abilities[{r['id']}].er_action '{r['er_action']}' is not a Fiddlesticks row in er_actions")
    for r in s["er_actions"]["rows"]:
        ref(f"er_actions[{r['id']}].use", r["use"], {"Fiddlesticks", "blocked", "kept"})
    for r in s["er_menus"]["rows"]:
        ref(f"er_menus[{r['id']}].action", r["action"], {"hide", "close", "keep", "hud"})
    for r in s["items"]["rows"]:
        ref(f"items[{r['id']}].effect", r["effect"], {"burn", "slow", "ap_mult", "dmg_mult", "low_hp_mult", "r_haste", "none"})
    for r in s["vfx_events"]["rows"]:
        if not (r["attach"] in ("caster", "target", "point", "missile", "beam", "fly_to_caster") or r["attach"].startswith("bone:")):
            problems.append(f"vfx_events[{r['id']}].attach: '{r['attach']}' unknown")
    for r in s["skins"]["rows"]:
        if not re.fullmatch(r"(skin:\d+|chroma:.+)", r["match"]):
            problems.append(f"skins[{r['id']}].match: '{r['match']}' is neither skin:<n> nor chroma:<word>")
    effect_groups = {r["effect"] for r in s["effects"]["rows"]}
    for r in s["abilities"]["rows"]:
        ref(f"abilities[{r['id']}].effect", r["effect"], effect_groups)
    enums = {
        "trigger": {"missile_head", "missile_trail", "hit", "area", "feared", "tether_beam", "tether_pulse", "tether_flow", "victim", "tick", "heal", "channel", "final", "r_channel", "storm", "r_leap", "r_land", "burn", "frost", "amp", "aura"},
        "render": {"billboard", "stretched", "ground", "beam", "model"},
        "blend": {"add", "alpha"},
        "motion": {"free", "orbit", "home"},
    }
    for r in s["effects"]["rows"]:
        w = f"effects[{r['id']}]"
        for col, pool in enums.items():
            ref(f"{w}.{col}", r[col], pool)
        if r["render"] == "model":
            if r["texture"] != "crow":
                problems.append(f"{w}.texture: a model emitter must use the crow")
        else:
            ref(w + ".texture", r["texture"], assets)
        for c in ("color_start", "color_end"):
            if not re.fullmatch(r"[0-9a-fA-F]{8}", r[c]):
                problems.append(f"{w}.{c}: '{r[c]}' is not RRGGBBAA")
        if r["radius"] != "ability":
            try:
                float(r["radius"])
            except ValueError:
                problems.append(f"{w}.radius: '{r['radius']}' is neither a number nor 'ability'")
        burst = r["trigger"] in ("missile_head", "hit", "area", "final", "tether_beam", "tether_pulse", "tick", "heal", "r_land", "amp")
        if burst and r["count"] <= 0:
            problems.append(f"{w}.count: a {r['trigger']} emitter needs count > 0")
        if not burst and r["rate"] <= 0:
            problems.append(f"{w}.rate: a {r['trigger']} emitter needs rate > 0")

    # 3. League facts found in the player's install
    if verify_path:
        vp = pathlib.Path(verify_path)
        if not vp.exists():
            problems.append(f"{vp}: missing (run fiddlering-prep --prepare first)")
        else:
            v = json.loads(vp.read_text(encoding="utf-8"))
            for r in s["league_assets"]["rows"]:
                if r["id"] not in v.get("assets", []) and r["required"]:
                    problems.append(f"league_assets[{r['id']}]: not found in the League install")
            for r in s["animations"]["rows"]:
                if r["id"] not in v.get("clips", []):
                    problems.append(f"animations[{r['id']}].league_clip '{r['league_clip']}': not found")
            for r in s["sounds"]["rows"]:
                if r["id"] not in v.get("sounds", []):
                    problems.append(f"sounds[{r['id']}].event '{r['event']}': no audio resolved")
            spells = v.get("spells", {})
            for r in s["abilities"]["rows"]:
                sp = spells.get(r["id"])
                if sp is None:
                    problems.append(f"abilities[{r['id']}].league_spell: not found")
                    continue
                for c in ("cooldown", "range", "radius", "cc_value"):
                    val = r[c]
                    if val in ("none", "attack_speed") or ":" in val:
                        continue
                    if val not in sp:
                        problems.append(f"abilities[{r['id']}].{c} '{val}': not in the League spell")

    # 4. in-game verification (release only)
    if release:
        for r in s["er_hooks"]["rows"] + s["er_teams"]["rows"] + s["er_menus"]["rows"]:
            if r["verified"] != "yes":
                problems.append(f"er_hooks[{r['id']}]: not verified in game")

    return problems


def rs_str(v):
    return json.dumps(v, ensure_ascii=False)


def gen():
    s = load()
    out = ["// @generated by scripts/sheets.py from sheets/*.json — edit the sheets, not this file.", ""]

    out.append("pub struct Anim { pub id: &'static str, pub league_clip: &'static str, pub looped: bool, pub speed: f32, pub blend: f32, pub next: &'static str }")
    out.append("pub const ANIMS: &[Anim] = &[")
    for r in s["animations"]["rows"]:
        out.append(f"    Anim {{ id: {rs_str(r['id'])}, league_clip: {rs_str(r['league_clip'])}, looped: {str(r['loop']).lower()}, speed: {float(r['speed'])}, blend: {float(r['blend'])}, next: {rs_str(r['next'])} }},")
    out.append("];\n")

    cols = s["abilities"]["columns"]
    out.append("pub struct Ability { " + ", ".join(
        f"pub {c}: {'u32' if c in ('unlock_level', 'max_rank') else '&' + chr(39) + 'static str'}" for c in cols) + " }")
    out.append("pub const ABILITIES: &[Ability] = &[")
    for r in s["abilities"]["rows"]:
        fields = []
        for c in cols:
            fields.append(f"{c}: {r[c]}" if c in ("unlock_level", "max_rank") else f"{c}: {rs_str(r[c])}")
        out.append("    Ability { " + ", ".join(fields) + " },")
    out.append("];\n")

    out.append("pub struct Sound { pub id: &'static str, pub bank: &'static str, pub event: &'static str, pub volume: f32, pub chance: f32, pub min_interval: f32 }")
    out.append("pub const SOUNDS: &[Sound] = &[")
    for r in s["sounds"]["rows"]:
        out.append(f"    Sound {{ id: {rs_str(r['id'])}, bank: {rs_str(r['bank'])}, event: {rs_str(r['event'])}, volume: {float(r['volume'])}, chance: {float(r['chance'])}, min_interval: {float(r['min_interval'])} }},")
    out.append("];\n")

    out.append("pub struct Level { pub level: u32, pub xp_total: f32, pub rank_up: &'static str }")
    out.append("pub const LEVELS: &[Level] = &[")
    for r in s["progression"]["rows"]:
        out.append(f"    Level {{ level: {r['level']}, xp_total: {float(r['xp_total'])}, rank_up: {rs_str(r['rank_up'])} }},")
    out.append("];\n")

    out.append("pub struct Asset { pub id: &'static str, pub wad: &'static str, pub locator: &'static str, pub kind: &'static str, pub required: bool }")
    out.append("pub const ASSETS: &[Asset] = &[")
    for r in s["league_assets"]["rows"]:
        out.append(f"    Asset {{ id: {rs_str(r['id'])}, wad: {rs_str(r['wad'])}, locator: {rs_str(r['locator'])}, kind: {rs_str(r['kind'])}, required: {str(r['required']).lower()} }},")
    out.append("];\n")

    out.append("pub struct HudItem { pub id: &'static str, pub kind: &'static str, pub source: &'static str, pub x: f32, pub y: f32, pub w: f32, pub h: f32, pub binding: &'static str }")
    out.append("pub const HUD: &[HudItem] = &[")
    for r in s["hud"]["rows"]:
        out.append(f"    HudItem {{ id: {rs_str(r['id'])}, kind: {rs_str(r['kind'])}, source: {rs_str(r['source'])}, x: {float(r['x'])}, y: {float(r['y'])}, w: {float(r['w'])}, h: {float(r['h'])}, binding: {rs_str(r['binding'])} }},")
    out.append("];\n")

    out.append("pub struct Effect { pub id: &'static str, pub effect: &'static str, pub trigger: &'static str, pub texture: &'static str, pub render: &'static str, pub additive: bool, pub count: u32, pub rate: f32, pub life: f32, pub size_start: f32, pub size_end: f32, pub speed: f32, pub spread: f32, pub up: f32, pub gravity: f32, pub drag: f32, pub spin: f32, pub color_start: [f32; 4], pub color_end: [f32; 4], pub frames: u32, /** m, or -1 = the ability's radius */ pub radius: f32, pub motion: &'static str }")
    out.append("pub const EFFECTS: &[Effect] = &[")
    def rgba(h):
        return "[" + ", ".join(str(round(int(h[i:i + 2], 16) / 255, 4)) for i in (0, 2, 4, 6)) + "]"
    for r in s["effects"]["rows"]:
        radius = -1.0 if r["radius"] == "ability" else float(r["radius"])
        f = lambda c: float(r[c])
        out.append(f"    Effect {{ id: {rs_str(r['id'])}, effect: {rs_str(r['effect'])}, trigger: {rs_str(r['trigger'])}, texture: {rs_str(r['texture'])}, render: {rs_str(r['render'])}, additive: {str(r['blend'] == 'add').lower()}, count: {r['count']}, rate: {f('rate')}, life: {f('life')}, size_start: {f('size_start')}, size_end: {f('size_end')}, speed: {f('speed')}, spread: {f('spread')}, up: {f('up')}, gravity: {f('gravity')}, drag: {f('drag')}, spin: {f('spin')}, color_start: {rgba(r['color_start'])}, color_end: {rgba(r['color_end'])}, frames: {r['frames']}, radius: {radius}, motion: {rs_str(r['motion'])} }},")
    out.append("];\n")

    out.append("/// Elden Ring team types Fiddlesticks' abilities hit.")
    out.append("pub const HOSTILE_TEAMS: &[u8] = &[" + ", ".join(str(r["team_type"]) for r in s["er_teams"]["rows"] if r["hostile"]) + "];")
    out.append("pub const KNOWN_TEAMS: &[u8] = &[" + ", ".join(str(r["team_type"]) for r in s["er_teams"]["rows"]) + "];\n")

    out.append("/// ChrActions bits taken from Elden Ring (er_actions sheet: Fiddlesticks + blocked).")
    out.append("pub const BLOCKED_ACTION_BITS: &[u32] = &[" + ", ".join(str(r["bit"]) for r in s["er_actions"]["rows"] if r["use"] == "blocked") + "];")
    out.append("/// ChrActions bits read as Fiddlesticks' abilities: captured after Elden Ring reads the pad, then cleared.")
    out.append("pub const ABILITY_ACTION_BITS: &[u32] = &[" + ", ".join(str(r["bit"]) for r in s["er_actions"]["rows"] if r["use"] == "Fiddlesticks") + "];\n")

    out.append("/// Elden Ring screens (er_menus sheet): hide Fiddlesticks on them / close them as they open.")
    out.append("pub const HIDE_ON_UI: &[usize] = &[" + ", ".join(str(r["ui_index"]) for r in s["er_menus"]["rows"] if r["action"] == "hide") + "];")
    out.append("pub const CLOSE_UI: &[usize] = &[" + ", ".join(str(r["ui_index"]) for r in s["er_menus"]["rows"] if r["action"] == "close") + "];")
    out.append("pub const HUD_HIDE_UI: &[usize] = &[" + ", ".join(str(r["ui_index"]) for r in s["er_menus"]["rows"] if r["action"] == "hud") + "];")
    out.append("")

    out.append("pub struct VfxEvent { pub id: &'static str, pub key: &'static str, pub attach: &'static str, pub scale: f32 }")
    out.append("pub const VFX_EVENTS: &[VfxEvent] = &[")
    for r in s["vfx_events"]["rows"]:
        out.append(f"    VfxEvent {{ id: {rs_str(r['id'])}, key: {rs_str(r['key'])}, attach: {rs_str(r['attach'])}, scale: {float(r['scale'])} }},")
    out.append("];")
    out.append("")

    out.append("pub struct ItemRow { pub id: u32, pub effect: &'static str, pub value: f32 }")
    out.append("pub const ITEMS: &[ItemRow] = &[")
    for r in s["items"]["rows"]:
        out.append(f"    ItemRow {{ id: {int(r['id'])}, effect: {rs_str(r['effect'])}, value: {float(r['value'])} }},")
    out.append("];")
    out.append("")

    out.append("pub struct SkinTheme { pub id: &'static str, pub matches: &'static str, pub hue: f32, pub sat: f32, pub val: f32 }")
    out.append("pub const SKIN_THEMES: &[SkinTheme] = &[")
    for r in s["skins"]["rows"]:
        out.append(f"    SkinTheme {{ id: {rs_str(r['id'])}, matches: {rs_str(r['match'])}, hue: {float(r['hue'])}, sat: {float(r['sat'])}, val: {float(r['val'])} }},")
    out.append("];\n")

    out.append("pub mod tuning {")
    for r in s["tuning"]["rows"]:
        out.append(f"    /// {r['why']} ({r['unit']})")
        out.append(f"    pub const {r['id'].upper()}: f32 = {float(r['value'])};")
    out.append("}\n")

    dest = ROOT / "crates" / "lolread" / "src" / "sheet.rs"
    dest.write_text("\n".join(out), encoding="utf-8")
    print(f"wrote {dest.relative_to(ROOT)}")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "preflight"
    if cmd == "gen":
        gen()
    else:
        verify = None
        if "--verify" in sys.argv:
            verify = sys.argv[sys.argv.index("--verify") + 1]
        probs = preflight("--release" in sys.argv, verify)
        if probs:
            print(f"PREFLIGHT: {len(probs)} open item(s)")
            for p in probs:
                print("  - " + p)
            sys.exit(1)
        print("PREFLIGHT: clean")
