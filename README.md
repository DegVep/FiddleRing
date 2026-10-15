# FiddleRing

Play **Elden Ring as Fiddlesticks** from League of Legends: his model, animations, voice lines, skins, visual
effects and kit, read from **your own League install**. No League of Legends or Elden Ring files are distributed.

## Features

- **Full kit**: basic attack (ranged and melee), Q *Terrify* (stun), W *Bountiful Harvest* (drain and heal),
  E *Reap* (silence, slow and frostbite), R *Crowstorm* (leap and storm). Every ability builds up madness.
- **Levels 1–18** like in League, with XP from kills and ability ranks you choose (Ctrl+1–4).
- **All 55 skins and chromas** you have in League, with their own voices, animations and effects (F2 menu).
- **League items**: every boss lets you pick 1 of 3 (F3 inventory). Item slots open as you level up, and once all
  six are full bosses upgrade your items instead. Zhonya's Hourglass is activated with T.
- **League-style HUD**: portrait, abilities with cooldowns, stats, custom health bars and boss names.
- **Magic resistance and penetration**, crowd-control immunity, Scadutree Blessing scaling, and balance tuned
  for the whole game (`scripts/balance_sim.py`).
- **F4** switches between Fiddlesticks and your Tarnished at any time.
- Friendly NPCs can be attacked by locking onto them, and they turn hostile.
- Progress is saved per save slot and character.
- The mod's HUD follows your League client's language: Spanish for a Spanish client, English otherwise.

## Requirements

- Elden Ring 2.7.1.0 (Steam), played **offline** with Easy Anti-Cheat disabled (ModEngine2 takes care of it).
- League of Legends installed on the same PC (the data is read from it).
- [ModEngine2](https://github.com/soulsmods/ModEngine2) 2.1.0.
- Back up your save (`ER0000.sl2`) before playing with mods.

## Controls

| Key | Action |
|---|---|
| Click / RB | Basic attack |
| 1–4 (or RT, LB, LT, R) | Q, W, E, R |
| Ctrl+1–4 | Rank up an ability |
| Alt+1–4 | Joke, taunt, dance, laugh |
| T | Zhonya's Hourglass active |
| F2 | Skins and chromas |
| F3 | Items |
| F4 | Switch between Fiddlesticks and the Tarnished |
| Lock-on | Target a friendly NPC so you can attack it |

## How it works

- `fiddlering-prep.exe` reads League's WAD files (model, animations, textures, sounds, effects, items) and writes a
  cache to `%LOCALAPPDATA%\FiddleRing\cache`. The DLL runs it on its own when the cache is missing.
- `fiddlering.dll` is loaded by ModEngine2 (`package/config_fiddlering.toml`). It hides the Tarnished, draws
  Fiddlesticks with its own D3D12 renderer and runs the whole kit inside Elden Ring.
- `sheets/*.json` are the source of truth (abilities, effects, items, tuning). `scripts/sheets.py gen` generates
  `crates/lolread/src/sheet.rs`.

## Building

Windows, stable Rust `x86_64-pc-windows-gnu` (see `rust-toolchain.toml`). Linking uses
[llvm-mingw](https://github.com/mstorsjo/llvm-mingw), because GNU `ld` mis-links raw-dylib imports inside a DLL.
`tools/` (not part of the repo) needs:

- `tools/llvm-mingw-20260922-ucrt-x86_64/`: the llvm-mingw release, unpacked.
- `tools/gnu-shim/libgcc.a` (a copy of `libclang_rt.builtins-x86_64.a`) and `libgcc_eh.a` (a copy of
  `libunwind.a`), both from llvm-mingw.
- `tools/dlltool-only/`: llvm-mingw's `dlltool.exe` with its DLLs (`libLLVM-*.dll`, `libc++.dll`, `libunwind.dll`).

```bash
export PATH="$PWD/tools/dlltool-only:$PATH"
python -I scripts/sheets.py preflight && python -I scripts/sheets.py gen
cargo build --release
```

To play, copy into `modengine2/mod/fiddlering/`: `fiddlering.dll`, `fiddlering-prep.exe`,
`package/config_fiddlering.toml`, and [vgmstream-cli](https://github.com/vgmstream/vgmstream) with its DLLs
(it converts League's sounds). Then run
`modengine2_launcher.exe -t er -p <eldenring.exe> -c config_fiddlering.toml`.

## Credits

- [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs) (Vincent Swarte and contributors): Elden Ring
  structures. Copied into `vendor/`, under its MIT / Apache NON-AI licenses.
- [ModEngine2](https://github.com/soulsmods/ModEngine2) (soulsmods), [vgmstream](https://github.com/vgmstream/vgmstream),
  [llvm-mingw](https://github.com/mstorsjo/llvm-mingw).
- [CommunityDragon](https://www.communitydragon.org/) (hash tables, development only),
  [Smithbox](https://github.com/vawser/Smithbox) (Elden Ring team types).
- Fiddlesticks and League of Legends belong to Riot Games; Elden Ring belongs to FromSoftware and Bandai Namco.
  This is a fan project, not affiliated with any of them.

## License

[MIT](LICENSE) for FiddleRing's own code. Remixes welcome.
