# Screen Zones

Puts games and programs borderless into a fixed area of the screen and keeps them there – a program of the
[Windows Toolbox](https://github.com/Windows-Toolbox/Toolbox).

> **Downloaded from the releases?** The `.exe` is not signed (signing costs money). Windows SmartScreen may warn
> once at the first start: *More info → Run anyway*.

One wide screen gets back areas that behave like screens of their own. A game is entered once; from then on it
lands in its zone by itself at every start – without a frame, with the desktop next to it visible and usable. On a
5120×1440 screen, for example, a game runs at 2560×1440 on the left, in the middle or on the right.

Why do all this? Because on my ultrawide with one displayPort instead of two I couldnt get all games/programs to 
only use the left half, while borderless, keeping the mouse in and staying on top of the taskbar.
So I figures I could just do it properly myself.

## Using it

Add the folder in the Toolbox (**Add program…**), press **Start**, and switch on **Start with Windows** if it should
always run. Everything else is on the program's page there:

- **Add a game**: start the game in windowed mode, then click it in the list – it shows every open program that has
  no entry yet.
- **Per game**: the zone (16:9 left / middle / right, 21:9 middle, left or right half, whole screen) – changing it
  moves a running game at once; **Play** (opens `uplay://…`, `steam://rungameid/…` or an exe); **Give the frame
  back**; switches for the frame and for staying in front of the taskbar; the waiting time after the window appears;
  the size to set in the game.
- **Keys** (off until switched on): the arrows ← / ↓ / → put the active window into 16:9 left / middle / right,
  ↑ gives its frame back – together with Win+Ctrl+Alt (the default), Ctrl+Alt or Win+Alt, chosen on the page. A
  combination that another program or Windows has taken already gets a thin red frame there, and each of its keys
  that does nothing is named. A window without an entry can be kept as a game with one click.

Without the Toolbox the program runs too: a double-click starts it, and after a moment it shows a tray icon of its
own (status, Play for each game, Quit). Its settings then stand in `data\settings.json`.

**Switching over from the Rayman Legends script:** at its first start Screen Zones finds `..\Rayman Legends\` and
takes Rayman over as its first entry, at the position chosen there. Then remove *Rayman Legends* from the Toolbox's
list – otherwise script and Screen Zones both move the window.

### What it does not do

- **Exclusive fullscreen** is not redirected: the game needs a windowed mode.
- **The picture is not scaled**: the game draws at the size of its window – set the zone's size in the game.
- **Nothing is injected into a game**: only the normal window functions of Windows are used, the same technique as
  Borderless Gaming. For games with kernel anti-cheat there is still no guarantee.
- **Games running as administrator** cannot be changed by a normal program; the entry says so.

## How it works

`ScreenZones.exe` runs in the background without a window. Windows reports what happens to windows
(`SetWinEventHook`: shown, activated, moved, minimised, gone); the program reacts and otherwise sleeps.

- **Finding the game**: a shown window whose program (`Rayman Legends.exe`) has an entry. Splash screens, launchers'
  small windows, tool windows and hidden helper windows are not taken: the biggest real window of the process is.
  After the entry's waiting time (default 3 s) the window is taken. Windows that are already open when the program
  starts – or when an entry is added – are taken at once.
- **Taking it**: frame and edges removed, placed exactly on the zone of the screen it is on, not activated.
- **Holding it**: if the game moves or resizes itself, it is put back at once (at most three times in two
  seconds – a game that keeps undoing it is left alone and its entry says “does not take the size”; clicking into
  the game tries again). While a game is held the program also looks once a second whether the frame came back
  (a game may change its style without moving). In front of the taskbar only while the game is the active window.
- **Giving back**: removing or switching off an entry, “Give the frame back”, Stop and Quit give each window its
  styles and its old place back. Only a crash of the program leaves a window as it is.

## Building

`build.ps1` builds the program and installs it into `dist\` – that folder is the installed program. Requirement:
[Rust](https://rustup.rs). `release.ps1` publishes a signed release (needs the Toolbox repo next to this one).

The shared library (`toolbox-kit`) comes from the Toolbox repo; cargo fetches it. With the Toolbox repo next to
this one and the small `.cargo\config.toml` in the folder above both (Toolbox `DESIGN.md`, section 3), the local
kit is used instead. Screen Zones needs a kit that knows keys pointing into a list (`games.0.zone`, Toolbox
`DESIGN.md` 15.2).

| File | What it holds |
|---|---|
| `src\main.rs` | settings, the page, the buttons, taking over the Rayman script's position |
| `src\watcher.rs` | the worker: window events, holding the games, the keys |
| `src\win.rs` | every Windows call about other programs' windows |
| `src\zones.rs` | where a zone lies on a screen (plain arithmetic) |
| `assets\i18n.json` | every text, German and English |
| `assets\icon.ico` | the icon, made from `assets\logo_source.png` |

A new setting of a game is one field in `Game` and one line in `page`; a new zone is one entry in `Zone` with its
rectangle and its texts.

**Tests**: `cargo test` – zones, page, buttons, texts, taking over Rayman, and taking and giving back a hidden window
of the test itself (nothing pops up). `cargo test -- --ignored --test-threads=1` adds the whole chain with Windows'
real events against a window of a second process (fully transparent, never activated, lets clicks through – only a
taskbar button shows for a moment): it appears, waits, is taken, gets its frame back and loses it again, moves
itself and is put back, changes zone, and gets everything back on Stop. `keys_on_this_pc` (also `--ignored`, with
`--nocapture`) prints which key combinations are taken on this PC.

## What the Rayman script could do – the checklist

Written from `rayman_borderless.ps1` and `program.json` of the Rayman Legends repo (Toolbox `DESIGN.md`, section 14).

✔ in this version, covered by a test · ◐ in this version, **not yet tried** with the real game · ↻ works differently
on purpose (why)

- ✔ removes the frame (`WS_CAPTION`, `WS_THICKFRAME`, `WS_SYSMENU`, min/max) – and also the edge styles
- ✔ 2560×1440 at the left, in the middle or on the right, y = 0, in physical pixels (DPI aware: the exe's manifest)
  ↻ as zones 16:9 left / middle / right, worked out from the screen – the same 2560×1440 on 5120×1440, the right
  size on any other screen, on the screen the window is on instead of always the main screen
- ✔ the position switched on the page moves the running game at once ↻ through the Toolbox's `set` instead of the
  script re-reading its `data\settings.json`
- ✔ 3 seconds for the game to finish its window ↻ a waiting time per entry, 3 s by default
- ✔ the frame removed again when the game brings it back ↻ on the game's own move/resize at once, otherwise within a
  second (the script looked every 300 ms whether it was running or not)
- ✔ in front of the taskbar only while active
- ✔ started before or after the game
- ↻ waited at most 5 minutes for the game and ended with it – Screen Zones runs in the background and takes the
  game at every start
- ✔ “Play” opens `uplay://launch/410/0` ↻ per entry, also in the tray menu
- ✔ the position chosen on the script's page is taken over once (`..\Rayman Legends\data\settings.json`)
- ✔ the hint “set windowed mode at 2560×1440 once in the game” ↻ per entry, with the size of its zone
- ◐ all of it with the real Rayman Legends: three zones, the options menu (frame comes back), Alt+Tab, Stop (frame
  must come back) – only the developer can confirm that at the game

## Later

From the concept (`KONZEPT.md` in the Rayman Legends repo), not built yet:

- **Special cases**: games running as administrator (the program started through a task with admin rights), keeping
  the mouse inside the zone (`ClipCursor`, for games that scroll at the screen's edge), own zones (fixed pixels,
  shares of the width), detecting exclusive fullscreen and saying so.
- **Extensions**: a black background around the game (“cinema mode”, toggled with a key); start parameters per game
  (`-windowed`); scaling a smaller picture up to the zone; two games side by side with a key that swaps them; a zone
  editor; profiles; recognising by window title; muting a game that is not active; zones on several screens in the
  page; ready-made entries for known games.
