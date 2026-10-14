# FiddleRing — MODLOG

Mashup: play Elden Ring as League of Legends' Fiddlesticks (model, animations, abilities, voice), League-style
levels 1-18. Host: Elden Ring 2.7.1.0 (Steam, D:\SteamLibrary\steamapps\common\ELDEN RING). Companion: League of
Legends 16.20 (C:\Riot Games\League of Legends). Solo, offline (Melty launches ER through ModEngine2, EAC off).

## Route
- ER: native Rust DLL loaded by ModEngine2 (`external_dlls`), Melty installs ModEngine2 release-2.1.0 itself.
  Game structures from fromsoftware-rs 0.14 (`eldenring` crate, Ww2710 RVA table matches exe 2.7.1.0).
- League: `fiddlering-prep.exe` runs as Melty's setup step, reads the player's own WADs
  (`{game:league-of-legends}/Game/DATA/FINAL/Champions/FiddleSticks*.wad.client`) and writes a local cache in
  `%LOCALAPPDATA%\FiddleRing\cache`. Nothing from League is shipped.
- Rendering: own D3D12 renderer hooked on the game's swapchain Present (vtable hooks), draws the skinned League
  mesh at the hidden Tarnished's transform and a League-style HUD.
- Toolchain: Rust stable-x86_64-pc-windows-gnu (no MSVC on this PC).

## League facts (16.20)
- WAD 3.4: TOC entry byte 20 = kind|subchunks<<4, bytes 22..24 = first subchunk (u16), byte 21 = its high byte.
  Subchunk table chunk: `data/final/champions/fiddlesticks.wad.subchunktoc` (16 bytes each).
- Skin0 bin `data/characters/fiddlesticks/skins/skin0.bin`: skl/skn `ASSETS/Characters/Fiddlesticks/Skins/Base/Fiddlesticks_Base.*`,
  texture link 7194126061746782290 (BC1 1024², TEX), Weapon submesh texture link 17933396972034605902,
  initial hidden submeshes "L_Demon_Arm Lantern Tongue R_Demon_Arm", bbox 140x300x140.
- Animations `data/characters/fiddlesticks/animations/skin0.bin`: AtomicClipData → mAnimationFilePath (WAD link),
  all clips are r3d2anmd v5 (uncompressed). SubmeshVisibilityEventData shows demon arms etc.
- Spells in `data/characters/fiddlesticks/fiddlesticks.bin` (DataValues indexed by rank).
- VO (es_ES) lives in FiddleSticks.es_ES.wad.client under unknown paths: identify banks by event ids.
- SFX: `ASSETS/Sounds/Wwise2016/SFX/Characters/Fiddlesticks/Skins/Base/Fiddlesticks_Base_SFX_{audio,events}.bnk`.
- Icons: `assets/characters/fiddlesticks/hud/icons2d/fiddlesticks{q,w,e,r}.dds` (DDS), portrait
  `assets/characters/fiddlesticks/hud/fiddlesticks_circle_0.tex`.

## ER facts
- Default KB binds: Q lock-on, E interact, R use item, F jump, Space dodge, arrows switch slots → 1-4 are free.
- `CSChrActionRequestModule.disabled_action_inputs` blocks ER actions (binding independent).
- `ChrIns.debug_flags` per character: disabled_secondary_actions / disabled_movement (used for fear/silence).

## Log
- 2026-10-12: design agreed with creator (Fiddlesticks kit only, League levelling, MIT + remix).
- 2026-10-14: guards at the start (Godrick soldiers c4311/c4351, c4070) are team_type 48 — added as hostile to er_teams. AA now slows its clip to ~75% of the attack period, swaps to Melee_Attack1/2 within 2.5 m (damage at windup, no missile). Casts with no target log the 6 nearest characters.
- 2026-10-14: mana removed (creator). AA clip ~0.55 s (aa_anim_fraction 0.35). W slows the Tarnished (animation_speed 0.3) while draining. New HUD: framed gradient panel, HP bar with ticks, ability names/keys/sweep, stats box, cast bar; font atlas now full ASCII + Spanish with measured advances.
- 2026-10-14: stats box removed. E damage x0.5. Q stuns (animation_speed 0, no actions/movement; not when dead) + Terrify icon billboard over the head. W needs an enemy in range (slot dark otherwise) and ends at once when every drained target is gone.
- 2026-10-14: W visuals: tendrils start at the Demon_Soul bone (was a fixed 1.5 m), 3 curved tapering tendrils per target with soul pulses, victim aura, per-tick sparks, heal glow, ground ring of its reach, crows+feathers on the last tick. Homing particles fly to the bone too. prep --show lists joints.
- 2026-10-14: Q like W: dark and uncastable with no enemy in its range. W early stop cuts sfx_w_* (Audio::stop by id prefix). R: channel -> Leap (0.45 s, ease-out x/z writes, onto locked target or 7 m ahead, model arcs 1.6 m) -> landing shockwave -> Storm with vortex/ring/smoke wall/eyes/aura.
- 2026-10-14: skins. Base cache v6 lists every skin/chroma (names from the client's rcp-be-lol-game-data <locale>-assets.wad champions/9.json, parent from the mesh folder, iconCircle as skin_icon_N). `prep --skin N` builds cache/skins/skinN.bin (model incl. StaticMaterialDef diffuse, its animation graph found by ObjLink hash, SFX/VO banks of the skin with events renamed Fiddlesticks_ -> FiddlesticksSkinNN_). DLL: F2 picker, background build, hot swap (gfx::reset rebuilds the renderer), choice in skin.txt. Particles stay the base ones.
- 2026-10-14: skins II. Icons: iconAvatar (iconCircle is the champion's for all), chromas use the client's champion-chroma-images PNG. Animations: verified per clip that League itself reuses base anims for most skins (own sets: 6,7,8,9; 27/37/46 only recall/emotes). Skin colour themes (skins sheet, lolread::theme): world sprites hue-rotated, ability icons recoloured by prep, skin particle textures with the base name pattern replace the base ones. CC immunity: sa_durability refilled to 100000 each frame, real poise tracked; breaking it during W cancels W (needs in-game check: log POISE/CC immunity lines). Cache v7.
- 2026-10-14: League VFX. lolread::vfx reads VfxSystemDefinitionData (emitters: rate/single/lifetimes, Value* curves + probability tables, birth/over-life colour & scale, velocity/drag/accel/orbit, spawn shapes, atlas frames, uv scroll, textureMult, alpha erosion, blend, pass, primitives billboard/quad/ray/beam/mesh(.scb)/trail/crow, children by resource key) through each skin's ResourceResolver (skin bin + deps). prep stores the systems the vfx_events sheet names (+ children) with their textures and meshes in the cache (base) / skin cache. DLL vfx.rs simulates and emits triangles; gfx draws them with t0 texture, t1 multiply, t2 erosion, clamp/wrap samplers and 5 blend PSOs. Blend map (tuning vfx_blend_*): 1 alpha, 2/4 additive, 5 screen (4 as premultiplied drew black squares). Distortion emitters and AttachedMesh layers are skipped; rays map V along velocity; texAddressMode default clamps, 2 wraps. Lasting effects derive from game state each frame (sync_vfx); one-offs fall back to the effects sheet when a skin lacks the system. Cache v10.
- 2026-10-14: W heal effect removed (green sparks sat in the model). W tether: back to the mod's own tendril instead of League's beam, now ONE continuous ribbon per victim (shared edges, texture along its length), no pulses or flow dots.
- 2026-10-14: VFX fixes from Star Nemesis E: particleLinger no longer added to particle life (it stretched colour curves x17, slashes stayed transparent); birthUVOffsetMult + uvScrollClamp(Mult) so swipe masks slide once; material texture prefers Main_Texture (Skin27 demon arms had a placeholder Diffuse_Texture -> black arm). Cache v11.
- 2026-10-14: destroyed effects (missile landed, target gone, stop(), max age) cap every particle to its particleLinger, like League; Praetorian Q missile parts stayed hanging in the air.
