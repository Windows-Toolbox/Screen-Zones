//! Screen Zones – puts games and programs borderless into a fixed area of the screen and keeps them there.
//!
//! One wide screen gets back areas that behave like screens of their own: a game is entered once, and from then on
//! it lands in its zone by itself at every start, without a frame, with the desktop next to it still visible and
//! usable. The successor of the Rayman Legends script (`rayman_borderless.ps1`) – "one script per game" becomes
//! "one entry per game".
//!
//! The files:
//!   main.rs      settings, the page, the buttons, taking over the Rayman script's position
//!   watcher.rs   the worker: window events from Windows, holding the games, the keys
//!   win.rs       every Windows call about other programs' windows
//!   zones.rs     where a zone lies on a screen (plain arithmetic)
//!
//! Everything else – the pipe to the Toolbox, loading and saving, the tray menu, single instance – is the kit's job
//! (Toolbox\DESIGN.md 5).
#![windows_subsystem = "windows"]

mod watcher;
mod win;
mod zones;

use std::path::Path;

use kit::app::{self, Program};
use kit::page::{Builder, Button, Item};
use kit::proto::{Health, MenuEntry, Status, Tray};
use kit::{process, t};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use watcher::{GameState, Report, Taken};
use zones::{Rect, Zone};

/// One game (or program) and where it belongs. A new setting = one field here + one line in `page`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Game {
    pub name: String,
    /// The program's file name ("Rayman Legends.exe") – how its window is recognised. Upper and lower case do
    /// not matter.
    pub exe: String,
    pub zone: Zone,
    /// Off = the entry stays, but the game is left alone.
    pub enabled: bool,
    pub borderless: bool,
    /// Above the taskbar while it is the active window.
    pub on_top: bool,
    /// Seconds between the window's appearing and putting it into its zone. 0 by default: a game that rebuilds its
    /// window right after starting gets it fixed again at once anyway (the Rayman script waited 3 s).
    pub delay: u32,
    /// What "Play" opens: `uplay://launch/410/0`, `steam://rungameid/…`, or the path of an exe. Empty = no Play.
    pub launch: String,
    /// When its window was last found running (seconds since 1970; 0 = never) – for the sort "Last played".
    pub last: u64,
}

impl Default for Game {
    /// What an entry is when it is added – the program's purpose, so these are on (not "optional", Toolbox
    /// DESIGN.md 2.7).
    fn default() -> Self {
        Game { name: String::new(), exe: String::new(), zone: Zone::Center, enabled: true, borderless: true, on_top: true, delay: 0, launch: String::new(), last: 0 }
    }
}

/// Which keys go with the arrows. Chosen on the page: a combination another program (or Windows) has taken
/// already does nothing here, so there is a choice – Win+Alt+arrows, for example, is taken on some PCs.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum KeySet {
    /// Hardly ever taken.
    #[default]
    WinCtrlAlt,
    /// Turns the screen with some graphics drivers (Intel).
    CtrlAlt,
    WinAlt,
}

impl KeySet {
    pub const ALL: [KeySet; 3] = [KeySet::WinCtrlAlt, KeySet::CtrlAlt, KeySet::WinAlt];

    /// The modifier keys for `RegisterHotKey`.
    pub fn modifiers(self) -> u32 {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_WIN};
        match self {
            KeySet::WinCtrlAlt => MOD_WIN | MOD_CONTROL | MOD_ALT,
            KeySet::CtrlAlt => MOD_CONTROL | MOD_ALT,
            KeySet::WinAlt => MOD_WIN | MOD_ALT,
        }
    }

    /// "Win+Strg+Alt" – Ctrl is written as on the keyboard of the language.
    pub fn name(self) -> String {
        t!(match self {
            KeySet::WinCtrlAlt => "keys.win_ctrl_alt",
            KeySet::CtrlAlt => "keys.ctrl_alt",
            KeySet::WinAlt => "keys.win_alt",
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub games: Vec<Game>,
    /// The keys to move the active window (opt-in: they are global and could be in use elsewhere).
    pub hotkeys: bool,
    pub keys: KeySet,
}

/// What a button asks the worker to do – only the worker touches windows while it runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Order {
    /// "Give the frame back": the game's window as it was, left alone until "Put it back".
    Release(String),
    Retake(String),
}

pub struct Zones {
    settings: Settings,
    /// The worker is at work.
    running: bool,
    report: Report,
    /// For "Add a game".
    windows: Vec<win::Window>,
    orders: Vec<Order>,
    taken: Taken,
    /// The main screen – for the sizes the page names.
    screen: Rect,
}

impl Zones {
    fn new() -> Zones {
        Zones { settings: Settings::default(), running: false, report: Report::default(), windows: vec![], orders: vec![], taken: Taken::default(), screen: win::main_screen() }
    }

    fn state(&self, game: &Game) -> GameState {
        self.report.games.get(&game.exe.to_lowercase()).copied().unwrap_or(GameState::NotRunning)
    }
}

fn zone_name(zone: Zone) -> String {
    t!(&format!("zone.{}", zone.key()))
}

/// The list's groups, in their order – "Active first" sorts by it.
const GROUPS: [(&str, bool); 4] = [("active", true), ("problem", true), ("idle", false), ("off", false)];

/// The list's key – the Toolbox remembers view, filter and sort under it.
const LIST: &str = "screen_zones.games";

impl Zones {
    /// The group, the dot and its text of an entry.
    fn row_state(&self, g: &Game) -> (&'static str, Health, String) {
        let size = g.zone.rect(self.screen);
        if !g.enabled {
            return ("off", Health::Unknown, t!("game.off"));
        }
        if !self.running {
            return ("idle", Health::Unknown, t!("game.stopped"));
        }
        match self.state(g) {
            GameState::NotRunning => ("idle", Health::Unknown, t!("game.not_running")),
            GameState::Waiting => ("active", Health::Ok, t!("game.waiting")),
            GameState::Held => ("active", Health::Ok, t!("game.held", zone = zone_name(g.zone), w = size.w, h = size.h)),
            GameState::Minimized => ("active", Health::Ok, t!("game.minimized")),
            GameState::Released => ("active", Health::Warn, t!("game.released")),
            GameState::Refused => ("problem", Health::Warn, t!("game.refused", w = size.w, h = size.h)),
            GameState::Denied => ("problem", Health::Bad, t!("game.denied")),
        }
    }
}

impl Program for Zones {
    type Settings = Settings;
    const META: kit::Meta = kit::meta!();

    fn page(&self, p: &mut Builder, s: &Settings) {
        // While it runs, the Toolbox's frame shows the status line above the page.
        if !self.running {
            p.section(t!("page.status"), |p| p.text(t!("page.stopped")));
        }
        let zones: Vec<(Zone, String)> = Zone::ALL.iter().map(|z| (*z, zone_name(*z))).collect();
        p.list(LIST, |l| {
            for (id, open) in GROUPS {
                l.group(id, t!(&format!("group.{id}")), open);
            }
            l.sort("last", t!("page.sort.last")).empty(t!("page.empty"));
            l.add(t!("page.add.button"), |p| {
                p.text(t!("page.add.text"));
                if self.windows.is_empty() {
                    p.text(t!("page.add.none"));
                }
                for w in &self.windows {
                    p.buttons(|b| {
                        b.button(t!("page.add.window", title = w.title.trim(), exe = w.exe), &format!("add:{}", w.exe));
                    });
                }
                p.buttons(|b| {
                    b.button(t!("page.add.refresh"), "refresh");
                });
            });
            for (i, g) in s.games.iter().enumerate() {
                let k = |field: &str| format!("games.{i}.{field}");
                let size = g.zone.rect(self.screen);
                let (group, state, text) = self.row_state(g);
                let item = Item {
                    id: g.exe.to_lowercase(),
                    title: if g.name.trim().is_empty() { g.exe.clone() } else { g.name.clone() },
                    subtitle: g.exe.clone(),
                    state,
                    text: text.clone(),
                    group: group.into(),
                    tags: vec![zone_name(g.zone)],
                    button: (!g.launch.trim().is_empty()).then(|| Button { label: t!("game.play"), action: format!("play:{}", g.exe) }),
                    // Most recent first; never played last.
                    order: if g.last > 0 { [("last".to_string(), -(g.last as f64))].into() } else { Default::default() },
                    ..Item::default()
                };
                l.item(item, |p| {
                    if g.enabled && self.running {
                        p.status(text, if state == Health::Unknown { Health::Ok } else { state });
                    }
                    p.choice(t!("game.zone"), &k("zone"), &zones);
                    p.buttons(|b| {
                        if !g.launch.trim().is_empty() {
                            b.button(t!("game.play"), &format!("play:{}", g.exe));
                        }
                        if self.running && self.state(g) == GameState::Released {
                            b.button(t!("game.retake"), &format!("retake:{}", g.exe));
                        } else if self.running && g.enabled && self.state(g) != GameState::NotRunning {
                            b.button(t!("game.release"), &format!("release:{}", g.exe));
                        }
                        b.button(t!("game.remove"), &format!("remove:{}", g.exe));
                    });
                    p.text(t!("game.set_in_game", w = size.w, h = size.h));
                    p.switch(t!("game.enabled"), &k("enabled"));
                    p.switch(t!("game.borderless"), &k("borderless"));
                    p.switch(t!("game.on_top"), &k("on_top"));
                    p.slider(t!("game.delay"), &k("delay"), 0.0, 15.0, 1.0, "s");
                    p.entry(t!("game.name"), &k("name"));
                    p.entry(t!("game.launch"), &k("launch"));
                    p.text(t!("game.recognised", exe = g.exe));
                });
            }
        });
        p.section_closed(t!("page.keys"), |p| {
            p.switch(t!("page.keys.switch"), "hotkeys");
            // A combination found taken gets a thin red frame – so another one can be chosen at a glance.
            let sets: Vec<(KeySet, String, Option<Health>)> =
                KeySet::ALL.iter().map(|k| (*k, k.name(), self.report.taken_sets.contains(k).then_some(Health::Bad))).collect();
            p.choice_marked(t!("page.keys.set"), "keys", &sets);
            p.text(t!("page.keys.text", keys = s.keys.name()));
            if !self.report.taken_keys.is_empty() {
                let keys: Vec<String> = self.report.taken_keys.iter().map(|arrow| format!("{}+{arrow}", s.keys.name())).collect();
                p.status(t!("page.keys.taken", keys = keys.join(", ")), Health::Warn);
                p.text(t!("page.keys.choose_other"));
            }
            if let Some(m) = &self.report.moved {
                p.status(t!("page.keys.moved", title = m.title.trim(), exe = m.exe, zone = zone_name(m.zone)), Health::Ok);
                p.buttons(|b| {
                    b.button(t!("page.keys.keep"), "keep");
                });
            }
        });
        p.section_closed(t!("page.guide"), |p| {
            let s = self.screen;
            let (c, w) = (Zone::Center.rect(s), Zone::Wide.rect(s));
            p.text(t!("page.guide.text", sw = s.w, sh = s.h, w169 = c.w, h169 = c.h, w219 = w.w, h219 = w.h));
        });
        p.section_closed(t!("page.advanced"), |p| {
            p.buttons(|b| {
                b.button(t!("page.open_log"), "open_log").button(t!("page.open_settings"), "open_settings");
            });
        });
    }

    fn status(&self, running: bool) -> Status {
        if !running {
            return Status::default();
        }
        let games = &self.settings.games;
        let with = |state: GameState| -> Vec<&str> { games.iter().filter(|g| g.enabled && self.state(g) == state).map(|g| g.name.as_str()).collect() };
        let (held, waiting) = ([with(GameState::Held), with(GameState::Minimized), with(GameState::Refused), with(GameState::Denied)].concat(), with(GameState::Waiting));
        let text = if !held.is_empty() {
            t!("status.held", games = held.join(", "))
        } else if !waiting.is_empty() {
            t!("status.waiting", games = waiting.join(", "))
        } else if games.is_empty() {
            t!("status.no_games")
        } else {
            t!("status.ready")
        };
        let state = if !with(GameState::Denied).is_empty() {
            Health::Bad
        } else if !with(GameState::Refused).is_empty() {
            Health::Warn
        } else {
            Health::Ok
        };
        Status { running, state, text }
    }

    fn tray(&self) -> Tray {
        let menu = self.settings.games.iter().filter(|g| !g.launch.trim().is_empty()).map(|g| MenuEntry { label: t!("menu.play", game = g.name), action: format!("play:{}", g.exe) }).collect();
        Tray { line: self.status(self.running).text, menu, live: vec![] }
    }

    fn applied(&mut self, s: &Settings) {
        self.settings = s.clone();
        let mut windows = std::mem::take(&mut self.windows);
        windows.retain(|w| !s.games.iter().any(|g| g.exe.eq_ignore_ascii_case(&w.exe)));
        self.windows = windows;
    }

    fn action(&mut self, name: &str, s: &mut Settings) -> Option<String> {
        let (verb, exe) = name.split_once(':').unwrap_or((name, ""));
        let game = s.games.iter().position(|g| g.exe.eq_ignore_ascii_case(exe));
        match verb {
            "add" if game.is_some() => return Some(t!("toast.already", exe = exe)),
            "add" => {
                let title = self.windows.iter().find(|w| w.exe == exe).map(|w| w.title.trim().to_string()).filter(|t| !t.is_empty());
                let name = title.unwrap_or_else(|| exe.trim_end_matches(".exe").trim_end_matches(".EXE").to_string());
                s.games.push(Game { name: name.clone(), exe: exe.to_string(), ..Game::default() });
                self.windows.retain(|w| w.exe != exe);
                return Some(t!("toast.added", game = name));
            }
            "remove" => {
                if let Some(i) = game {
                    let g = s.games.remove(i);
                    return Some(t!("toast.removed", game = g.name));
                }
            }
            "play" => {
                let g = game.map(|i| &s.games[i])?;
                if g.launch.trim().is_empty() || !process::open(g.launch.trim()) {
                    return Some(t!("toast.play_failed", launch = g.launch.trim()));
                }
                if !self.running {
                    return Some(t!("toast.play_not_running"));
                }
            }
            "release" => self.orders.push(Order::Release(exe.to_string())),
            "retake" => self.orders.push(Order::Retake(exe.to_string())),
            "keep" => {
                let m = self.report.moved.take()?;
                if !s.games.iter().any(|g| g.exe.eq_ignore_ascii_case(&m.exe)) {
                    let name = if m.title.trim().is_empty() { m.exe.trim_end_matches(".exe").to_string() } else { m.title.trim().to_string() };
                    // The zone it was moved into is the one it keeps.
                    s.games.push(Game { name: name.clone(), exe: m.exe, zone: m.zone, ..Game::default() });
                    return Some(t!("toast.added", game = name));
                }
            }
            "refresh" => {
                self.windows = watcher::addable(win::windows(), s);
            }
            "open_log" => {
                process::open(&app::data_dir().join("log.txt").to_string_lossy());
            }
            "open_settings" => {
                process::open(&app::data_dir().join("settings.json").to_string_lossy());
            }
            _ => {}
        }
        None
    }

    /// Quitting gives every window back – also while the worker is busy elsewhere.
    fn shutdown(&mut self) {
        for (hwnd, original) in self.taken.lock().unwrap_or_else(|e| e.into_inner()).drain() {
            win::give_back(hwnd, &original);
        }
    }
}

// ---------- from the Rayman Legends script ----------

/// On the very first start (no `data\settings.json` yet): if the Rayman Legends script lies next to this program
/// (`..\Rayman Legends\` – the folder of all Toolbox programs), its game becomes the first entry, at the position
/// chosen on its page. So switching over keeps everything.
fn carry_over(program_dir: &Path, data: &Path) {
    if data.join("settings.json").exists() {
        return;
    }
    let read = |file: &Path| -> Option<Value> { serde_json::from_str(std::fs::read_to_string(file).ok()?.trim_start_matches('\u{feff}')).ok() };
    for above in program_dir.ancestors().skip(1).take(3) {
        let rayman = above.join("Rayman Legends");
        let Some(manifest) = read(&rayman.join("program.json")) else { continue };
        if manifest["id"] != "rayman_legends" {
            continue;
        }
        let zone = match read(&rayman.join("data").join("settings.json")).and_then(|s| s["Pos"].as_str().map(str::to_string)).as_deref() {
            Some("Left") => Zone::Left,
            Some("Right") => Zone::Right,
            _ => Zone::Center,
        };
        let launch = manifest["launch"].as_str().unwrap_or_default().to_string();
        let settings = Settings { games: vec![Game { name: "Rayman Legends".into(), exe: "Rayman Legends.exe".into(), zone, launch, ..Game::default() }], ..Settings::default() };
        let _ = std::fs::create_dir_all(data);
        let _ = std::fs::write(data.join("settings.json"), serde_json::to_string_pretty(&settings).unwrap_or_default());
        app::log(&format!("Rayman Legends taken over from {}", rayman.display()));
        return;
    }
}

fn main() {
    kit::texts!();
    app::init(&Zones::META);
    carry_over(&app::program_dir(), &app::data_dir());
    app::run(Zones::new(), watcher::work);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones(running: bool) -> Zones {
        kit::texts!();
        kit::i18n::set_language("en");
        Zones { running, screen: Rect::new(0, 0, 5120, 1440), ..Zones::new() }
    }

    fn rayman() -> Game {
        Game { name: "Rayman Legends".into(), exe: "Rayman Legends.exe".into(), launch: "uplay://launch/410/0".into(), ..Game::default() }
    }

    fn page(z: &Zones, s: &Settings) -> String {
        let v = serde_json::to_value(s).unwrap();
        let mut b = Builder::new(&v);
        z.page(&mut b, s);
        serde_json::to_string(&b.finish()).unwrap()
    }

    /// Every key the page names is a field of the settings (Toolbox\DESIGN.md 13) – with an entry, so that its
    /// keys (`games.0.zone`) are there.
    #[test]
    fn page_keys_are_settings() {
        let z = zones(true);
        let s = Settings { games: vec![rayman(), Game::default()], ..Settings::default() };
        kit::page::check_keys_with(&s, |p| z.page(p, &s)).unwrap();
        kit::page::check_keys::<Settings>(|p| z.page(p, &Settings::default())).unwrap();
    }

    /// Every text id used in the code exists, in every language.
    #[test]
    fn texts_are_complete() {
        kit::i18n::check_ids(env!("CARGO_MANIFEST_DIR")).unwrap();
        zones(false);
        for z in Zone::ALL {
            assert!(kit::i18n::has(&format!("zone.{}", z.key())), "{z:?}");
        }
    }

    #[test]
    fn the_page_tells_the_state_of_each_game() {
        let mut z = zones(false);
        let s = Settings { games: vec![rayman()], ..Settings::default() };
        let text = page(&z, &s);
        assert!(text.contains("Stopped") && text.contains("2560×1440") && text.contains("play:Rayman Legends.exe"));
        assert!(text.contains(r#""type":"list","key":"screen_zones.games""#) && text.contains(r#""id":"rayman legends.exe""#));
        assert!(text.contains(r#""group":"idle""#) && text.contains(r#""tags":["16:9 middle"]"#));
        assert!(!text.contains("release:"), "nothing to give back while stopped");

        z.running = true;
        z.applied(&s);
        z.report.games.insert("rayman legends.exe".into(), GameState::Held);
        let text = page(&z, &s);
        assert!(text.contains("Held: 16:9 middle, 2560×1440") && text.contains("release:Rayman Legends.exe"));
        assert!(text.contains(r#""group":"active""#), "a held game is active");
        assert_eq!(z.status(true).text, "Holding: Rayman Legends");
        z.report.games.insert("rayman legends.exe".into(), GameState::Released);
        assert!(page(&z, &s).contains("retake:Rayman Legends.exe"));
        z.report.games.insert("rayman legends.exe".into(), GameState::Denied);
        assert_eq!(z.status(true).state, Health::Bad);
        assert!(page(&z, &s).contains(r#""group":"problem""#));
        z.report.games.insert("rayman legends.exe".into(), GameState::Refused);
        assert_eq!(z.status(true).state, Health::Warn);
        let off = Settings { games: vec![Game { enabled: false, last: 50, ..rayman() }], ..Settings::default() };
        let text = page(&z, &off);
        assert!(text.contains(r#""group":"off""#) && text.contains(r#""order":{"last":-50.0}"#));
        assert!(page(&z, &s).contains("2560×1440"));

        z.report.games.clear();
        assert_eq!(z.status(true).text, "Ready – none of your games is running");
        z.applied(&Settings::default());
        assert!(z.status(true).text.contains("No game entered yet"));
        assert_eq!(z.status(false), Status::default());
        // A key taken by another program, a window moved with a key.
        z.report.taken_keys.extend(["←".into(), "↑".into()]);
        z.report.taken_sets.push(KeySet::WinCtrlAlt);
        z.report.moved = Some(watcher::Moved { exe: "game.exe".into(), title: "Game".into(), zone: Zone::Right });
        let text = page(&z, &Settings::default());
        assert!(text.contains("Already used by another program or by Windows: Win+Ctrl+Alt+←, Win+Ctrl+Alt+↑") && text.contains("\"keep\""));
        assert!(text.contains("\"label\":\"Win+Ctrl+Alt\",\"state\":\"bad\"") && text.contains("\"label\":\"Ctrl+Alt\"}"), "the taken set is marked");
        // German keyboards say Strg.
        kit::i18n::set_language("de");
        assert_eq!(KeySet::WinCtrlAlt.name(), "Win+Strg+Alt");
        kit::i18n::set_language("en");
    }

    #[test]
    fn buttons() {
        let mut z = zones(true);
        let mut s = Settings::default();
        z.windows = vec![win::Window { hwnd: 1, pid: 2, exe: "Game.exe".into(), title: " My Game ".into(), area: 9 }];
        assert_eq!(z.action("add:Game.exe", &mut s).unwrap(), "Added: My Game");
        assert_eq!(s.games, vec![Game { name: "My Game".into(), exe: "Game.exe".into(), ..Game::default() }]);
        assert!(z.windows.is_empty(), "no longer offered");
        assert!(z.action("add:game.EXE", &mut s).unwrap().contains("already"));
        // Without a window in the list the name comes from the file.
        z.action("add:Other.exe", &mut s);
        assert_eq!(s.games[1].name, "Other");
        z.action("release:Game.exe", &mut s);
        z.action("retake:Game.exe", &mut s);
        assert_eq!(z.orders, vec![Order::Release("Game.exe".into()), Order::Retake("Game.exe".into())]);
        assert_eq!(z.action("remove:Game.exe", &mut s).unwrap(), "Removed: My Game");
        assert_eq!(s.games.len(), 1);
        // Keeping a window moved with a key.
        z.report.moved = Some(watcher::Moved { exe: "Moved.exe".into(), title: "Moved".into(), zone: Zone::Left });
        z.action("keep", &mut s);
        assert_eq!((s.games[1].exe.as_str(), s.games[1].zone), ("Moved.exe", Zone::Left));
        assert_eq!(z.action("keep", &mut s), None, "only once");
        // Play only for an entry with a target.
        assert_eq!(z.action("play:Other.exe", &mut s).map(|t| t.contains("could not be opened")), Some(true));
        // The tray offers Play for the games that have one.
        s.games.push(rayman());
        z.applied(&s);
        assert_eq!(z.tray().menu, vec![MenuEntry { label: "Play Rayman Legends".into(), action: "play:Rayman Legends.exe".into() }]);
    }

    #[test]
    fn a_set_reaches_one_game() {
        let s = Settings { games: vec![rayman(), Game::default()], ..Settings::default() };
        let s = kit::page::apply_set(&s, "games.0.zone", serde_json::json!("left")).unwrap();
        assert_eq!((s.games[0].zone, s.games[1].zone), (Zone::Left, Zone::Center));
        let s = kit::page::apply_set(&s, "games.1.delay", serde_json::json!(5.0)).unwrap();
        assert_eq!(s.games[1].delay, 5);
        assert!(kit::page::apply_set(&s, "games.0.zone", serde_json::json!("middle")).is_err());
        // An old file with fields missing keeps the defaults.
        let s: Settings = serde_json::from_str(r#"{"games":[{"exe":"a.exe"}]}"#).unwrap();
        assert_eq!(s.games[0], Game { exe: "a.exe".into(), ..Game::default() });
    }

    #[test]
    fn the_rayman_script_is_taken_over() {
        let root = std::env::temp_dir().join(format!("screen-zones-carry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (program, data) = (root.join("Screen Zones").join("dist"), root.join("Screen Zones").join("dist").join("data"));
        std::fs::create_dir_all(root.join("Rayman Legends").join("data")).unwrap();
        std::fs::create_dir_all(&program).unwrap();
        // Without the script nothing happens.
        carry_over(&program, &data);
        assert!(!data.join("settings.json").exists());
        std::fs::write(root.join("Rayman Legends/program.json"), r#"{"id":"rayman_legends","launch":"uplay://launch/410/0"}"#).unwrap();
        std::fs::write(root.join("Rayman Legends/data/settings.json"), "\u{feff}{\"Pos\": \"Left\"}").unwrap();
        carry_over(&program, &data);
        let s: Settings = serde_json::from_str(&std::fs::read_to_string(data.join("settings.json")).unwrap()).unwrap();
        assert_eq!(s.games, vec![Game { zone: Zone::Left, ..rayman() }]);
        std::fs::write(data.join("settings.json"), "{}").unwrap();
        carry_over(&program, &data);
        assert_eq!(std::fs::read_to_string(data.join("settings.json")).unwrap(), "{}", "never a second time");
        std::fs::remove_dir_all(&root).ok();
    }
}
