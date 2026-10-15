# FiddleRing

Juega **Elden Ring como Fiddlesticks** de League of Legends: su modelo, animaciones, voces, skins, efectos
visuales y kit de habilidades, leídos de **tu propia instalación de League**. No se distribuye ningún archivo
de League ni de Elden Ring.

## Qué incluye

- **Kit completo**: ataque básico (a distancia y cuerpo a cuerpo), Q *Terror* (aturde), W *Cosecha generosa*
  (drena y cura), E *Siega* (silencia, ralentiza y congela), R *Tormenta de cuervos* (salto y tormenta).
  Todas las habilidades acumulan locura.
- **Niveles 1–18** como en League, con experiencia por enemigo y rangos elegidos por ti (Ctrl+1–4).
- **Las 55 skins y cromas** de tu League con sus voces, animaciones y efectos propios (menú F2).
- **Objetos de League**: cada jefe te deja elegir 1 de 3 (inventario F3). Los huecos se abren con el nivel y,
  con los 6 llenos, los jefes mejoran tus objetos. Zhonya se activa con la T.
- **HUD estilo League**: retrato, habilidades con enfriamiento, estadísticas, barras de vida propias y nombre
  de los jefes.
- **Resistencia y penetración mágica**, inmunidad a control, bendición del Árbol Umbrío y un balance pensado
  para todo el juego (`scripts/balance_sim.py`).
- **F4**: cambia entre Fiddlesticks y tu Sinluz cuando quieras.
- El progreso se guarda por partida y personaje.

## Requisitos

- Elden Ring 2.7.1.0 (Steam), **sin conexión** y con Easy Anti-Cheat desactivado (ModEngine2 lo hace).
- League of Legends instalado en el mismo PC (los datos se leen de ahí).
- [ModEngine2](https://github.com/soulsmods/ModEngine2) 2.1.0.
- Haz una copia de seguridad de tu partida (`ER0000.sl2`) antes de jugar con mods.

## Controles

| Tecla | Acción |
|---|---|
| Clic / RB | Ataque básico |
| 1–4 (o RT, LB, LT, R) | Q, W, E, R |
| Ctrl+1–4 | Subir una habilidad |
| Alt+1–4 | Bromear, provocar, bailar, reír |
| T | Activa del Reloj de arena de Zhonya |
| F2 | Skins y cromas |
| F3 | Objetos |
| F4 | Cambiar entre Fiddlesticks y el Sinluz |
| Bloqueo de objetivo | Fijar un NPC para poder atacarlo |

## Cómo funciona

- `fiddlering-prep.exe` lee los WAD de League (modelo, animaciones, texturas, sonidos, efectos, objetos) y escribe
  una caché en `%LOCALAPPDATA%\FiddleRing\cache`. La DLL lo lanza sola si falta.
- `fiddlering.dll` se carga con ModEngine2 (`package/config_fiddlering.toml`), oculta al Sinluz, dibuja a
  Fiddlesticks con su propio renderer D3D12 y lleva toda la lógica del kit dentro de Elden Ring.
- `sheets/*.json` son la fuente de verdad (habilidades, efectos, objetos, ajustes). `scripts/sheets.py gen`
  genera `crates/lolread/src/sheet.rs`.

## Compilar

Windows, Rust estable `x86_64-pc-windows-gnu` (ver `rust-toolchain.toml`). El enlazado usa
[llvm-mingw](https://github.com/mstorsjo/llvm-mingw) porque el `ld` de GNU enlaza mal las importaciones
raw-dylib dentro de una DLL. En `tools/` (no incluido en el repo) hacen falta:

- `tools/llvm-mingw-20260922-ucrt-x86_64/`: la release de llvm-mingw descomprimida.
- `tools/gnu-shim/libgcc.a` (copia de `libclang_rt.builtins-x86_64.a`) y `libgcc_eh.a` (copia de `libunwind.a`),
  ambos de llvm-mingw.
- `tools/dlltool-only/`: `dlltool.exe` de llvm-mingw con sus DLL (`libLLVM-*.dll`, `libc++.dll`, `libunwind.dll`).

```bash
export PATH="$PWD/tools/dlltool-only:$PATH"
python -I scripts/sheets.py preflight && python -I scripts/sheets.py gen
cargo build --release
```

Para jugar, copia en `modengine2/mod/fiddlering/`: `fiddlering.dll`, `fiddlering-prep.exe`,
`package/config_fiddlering.toml` y [vgmstream-cli](https://github.com/vgmstream/vgmstream) con sus DLL (convierte
los sonidos de League). Luego lanza `modengine2_launcher.exe -t er -p <eldenring.exe> -c config_fiddlering.toml`.

## Créditos

- [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs) (Vincent Swarte y colaboradores): estructuras de
  Elden Ring. Copia en `vendor/`, con su licencia MIT/Apache NON-AI.
- [ModEngine2](https://github.com/soulsmods/ModEngine2) (soulsmods), [vgmstream](https://github.com/vgmstream/vgmstream),
  [llvm-mingw](https://github.com/mstorsjo/llvm-mingw).
- [CommunityDragon](https://www.communitydragon.org/) (tablas de hashes, solo durante el desarrollo),
  [Smithbox](https://github.com/vawser/Smithbox) (tablas de equipos de Elden Ring).
- Fiddlesticks y League of Legends son de Riot Games; Elden Ring es de FromSoftware y Bandai Namco. Proyecto de
  fans sin relación con ninguno de ellos.

## Licencia

[MIT](LICENSE) para el código de FiddleRing. Se permite remezclarlo.
