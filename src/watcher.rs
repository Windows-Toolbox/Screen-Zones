//! The worker: it watches windows appear, puts every game that has an entry into its zone and keeps it there.
//!
//! It does not ask Windows again and again: Windows reports what happens to windows (`SetWinEventHook` – a window
//! is shown, becomes active, is moved, disappears), and the worker reacts. Idle, it sleeps in the message loop.
//! Only while it holds a game it also looks once a second whether the game brought its frame back – a game may
//! change its style without moving (Rayman after changing its options).
//!
//! Everything the worker took it gives back – when an entry is removed or switched off, on "Give the frame back",
//! when the program is stopped or quits. No window stays without its frame.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kit::app::Runtime;
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VK_DOWN, VK_LEFT, VK_RIGHT, VK_UP};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::win::{self, Hwnd, Original};
use crate::zones::{Rect, Zone};
use crate::{Game, KeySet, Order, Settings, Zones};

/// The windows the worker holds, with how they were before – shared with the program, so that quitting gives
/// them back even while the worker is busy.
pub type Taken = Arc<Mutex<HashMap<Hwnd, Original>>>;

/// What a game's entry is doing right now – for its line on the page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GameState {
    NotRunning,
    /// The window is there; the entry's waiting time runs.
    Waiting,
    Held,
    /// The game keeps changing its size back.
    Refused,
    /// The game runs with admin rights.
    Denied,
    Minimized,
    /// "Give the frame back" was clicked – left alone until "Put it back" or the next start of the game.
    Released,
}

/// A window moved with a key that has no entry yet – the page offers to keep it as a game.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub exe: String,
    pub title: String,
    pub zone: Zone,
}

/// What the worker found out – published to the program for status, page and tray.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Report {
    /// Per entry, by its exe in lower case.
    pub games: BTreeMap<String, GameState>,
    /// The arrows of the chosen keys another program had already taken ("←").
    pub taken_keys: Vec<String>,
    /// The key combinations with at least one arrow taken – the chosen one and, looked at when the keys are
    /// switched on or another combination is chosen, the others.
    pub taken_sets: Vec<KeySet>,
    pub moved: Option<Moved>,
}

/// The keys (switched on with "Keys" on the page, with the modifiers chosen there): id, key, zone – `None` = give
/// the frame back – and the arrow's sign.
pub const KEYS: [(i32, u16, Option<Zone>, &str); 4] = [(1, VK_LEFT, Some(Zone::Left), "←"), (2, VK_DOWN, Some(Zone::Center), "↓"), (3, VK_RIGHT, Some(Zone::Right), "→"), (4, VK_UP, None, "↑")];

/// Can another program's keys be told apart from free ones? Only by trying: registered and given back at once.
fn taken(set: KeySet) -> bool {
    KEYS.iter().any(|(id, vk, _, _)| {
        let probe = 100 + id;
        let ok = unsafe { RegisterHotKey(std::ptr::null_mut(), probe, set.modifiers() | MOD_NOREPEAT, *vk as u32) } != 0;
        if ok {
            unsafe { UnregisterHotKey(std::ptr::null_mut(), probe) };
        }
        !ok
    })
}

/// Posted to the worker's thread: settings, an order, Start or Stop – look at it.
pub const WM_WAKE: u32 = WM_APP + 1;

/// How the worker treats a held window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Target {
    zone: Zone,
    borderless: bool,
    on_top: bool,
}

impl Target {
    fn of(game: &Game) -> Target {
        Target { zone: game.zone, borderless: game.borderless, on_top: game.on_top }
    }
}

struct Held {
    pid: u32,
    /// Lower case.
    exe: String,
    target: Target,
    /// Moved with a key, no entry.
    by_key: bool,
    state: GameState,
    /// When it was put back into its zone lately – a game that undoes it more than `FIXES` times in `FIX_TIME`
    /// is left alone ("refuses the size") instead of fought with.
    fixes: Vec<Instant>,
    on_top_now: bool,
    /// It took "on top" away by itself once – logged once, not every second.
    top_lost: bool,
}

const FIXES: usize = 3;
const FIX_TIME: Duration = Duration::from_secs(2);
/// While a game is held: the look whether its frame came back.
const LOOK: Duration = Duration::from_secs(1);
/// The list of windows on the page is built again this long after windows came or went – once for a burst.
const LIST_DELAY: Duration = Duration::from_millis(500);

// ---------- the events from Windows ----------

thread_local! {
    /// What the hooks reported – collected by the callback, worked through by the loop.
    static EVENTS: RefCell<Vec<(u32, Hwnd)>> = const { RefCell::new(Vec::new()) };
}

unsafe extern "system" fn on_event(_hook: HWINEVENTHOOK, event: u32, hwnd: windows_sys::Win32::Foundation::HWND, object: i32, child: i32, _thread: u32, _time: u32) {
    // Only whole windows – not the caret, the cursor or a part of a window.
    if object == OBJID_WINDOW && child == CHILDID_SELF as i32 && !hwnd.is_null() {
        EVENTS.with(|e| e.borrow_mut().push((event, hwnd as Hwnd)));
    }
}

fn hook(from: u32, to: u32, pid: u32) -> HWINEVENTHOOK {
    unsafe { SetWinEventHook(from, to, std::ptr::null_mut(), Some(on_event), pid, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS) }
}

// ---------- the watcher ----------

pub struct Watcher {
    settings: Settings,
    taken: Taken,
    held: HashMap<Hwnd, Held>,
    /// Process → when to take its window, and the entry's exe (lower case).
    pending: HashMap<u32, (Instant, String)>,
    /// Process → exe (lower case): "Give the frame back" – left alone.
    released: HashMap<u32, String>,
    hooks: Vec<HWINEVENTHOOK>,
    /// "Moved" is reported only for the windows of held games, so it is hooked per process.
    moves: HashMap<u32, HWINEVENTHOOK>,
    /// The keys registered right now, and with which modifiers.
    keys_on: Option<KeySet>,
    report: Report,
    /// The windows last shown in "Add a game", and since when that list is out of date.
    listed: Vec<Hwnd>,
    list_due: Option<Instant>,
    last_look: Instant,
}

fn key(exe: &str) -> String {
    exe.to_lowercase()
}

impl Watcher {
    pub fn new(settings: Settings, taken: Taken) -> Watcher {
        Watcher {
            settings,
            taken,
            held: HashMap::new(),
            pending: HashMap::new(),
            released: HashMap::new(),
            hooks: vec![],
            moves: HashMap::new(),
            keys_on: None,
            report: Report::default(),
            listed: vec![],
            list_due: Some(Instant::now()),
            last_look: Instant::now(),
        }
    }

    fn game(&self, exe: &str) -> Option<&Game> {
        self.settings.games.iter().find(|g| g.enabled && key(&g.exe) == exe)
    }

    /// Starts watching: the hooks, the keys, and one look at the windows that are open already – so "game first,
    /// then Screen Zones" works as well as the other way round.
    pub fn start(&mut self) {
        self.hooks = [(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND), (EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZEEND), (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE)]
            .into_iter()
            .map(|(from, to)| hook(from, to, 0))
            .filter(|h| !h.is_null())
            .collect();
        if self.hooks.len() < 3 {
            kit::app::log("not every window hook could be set");
        }
        self.keys(self.settings.hotkeys.then_some(self.settings.keys));
        self.scan();
    }

    /// Stops watching and gives every window back.
    pub fn stop(&mut self) {
        for hwnd in self.held.keys().copied().collect::<Vec<_>>() {
            self.give_back(hwnd);
        }
        for h in self.hooks.drain(..).chain(self.moves.drain().map(|(_, h)| h)) {
            unsafe { UnhookWinEvent(h) };
        }
        self.keys(None);
        self.pending.clear();
        self.released.clear();
    }

    /// Registers the keys with these modifiers (`None`: no keys), and finds out which combinations are taken.
    fn keys(&mut self, want: Option<KeySet>) {
        if want == self.keys_on {
            return;
        }
        if self.keys_on.is_some() {
            for (id, ..) in KEYS {
                unsafe { UnregisterHotKey(std::ptr::null_mut(), id) };
            }
        }
        self.keys_on = want;
        self.report.taken_keys.clear();
        self.report.taken_sets.clear();
        let Some(set) = want else {
            self.report.moved = None;
            return;
        };
        for (id, vk, _, arrow) in KEYS {
            if unsafe { RegisterHotKey(std::ptr::null_mut(), id, set.modifiers() | MOD_NOREPEAT, vk as u32) } == 0 {
                self.report.taken_keys.push(arrow.to_string());
            }
        }
        // The chosen combination as just found; the others tried – the own one cannot be, it is registered now.
        self.report.taken_sets = KeySet::ALL.into_iter().filter(|k| if *k == set { !self.report.taken_keys.is_empty() } else { taken(*k) }).collect();
    }

    /// New settings (a change on the page, an entry added or removed): every held window follows its entry at once.
    pub fn settings(&mut self, settings: Settings) {
        let old = std::mem::replace(&mut self.settings, settings);
        for hwnd in self.held.keys().copied().collect::<Vec<_>>() {
            let h = &self.held[&hwnd];
            if h.by_key {
                continue;
            }
            let Some(target) = self.game(&h.exe).map(Target::of) else {
                self.give_back(hwnd); // removed or switched off
                continue;
            };
            if target != h.target {
                if h.target.borderless && !target.borderless {
                    // The frame comes back: give everything back, then place the framed window.
                    if let Some(o) = self.taken.lock().unwrap_or_else(|e| e.into_inner()).get(&hwnd) {
                        win::give_back(hwnd, o);
                    }
                }
                let h = self.held.get_mut(&hwnd).expect("just looked up");
                h.target = target;
                h.fixes.clear();
                self.put(hwnd);
            }
        }
        // A zone chosen anew for a released game: it is wanted back.
        for g in &self.settings.games {
            if old.games.iter().any(|o| key(&o.exe) == key(&g.exe) && o.zone != g.zone) {
                self.released.retain(|_, exe| *exe != key(&g.exe));
            }
        }
        let settings = &self.settings;
        self.pending.retain(|_, (_, exe)| settings.games.iter().any(|g| g.enabled && key(&g.exe) == *exe));
        self.keys(self.settings.hotkeys.then_some(self.settings.keys));
        self.scan();
    }

    pub fn orders(&mut self, orders: Vec<Order>) {
        for order in orders {
            match order {
                Order::Release(exe) => {
                    let exe = key(&exe);
                    for hwnd in self.held.iter().filter(|(_, h)| h.exe == exe).map(|(w, _)| *w).collect::<Vec<_>>() {
                        let pid = self.held[&hwnd].pid;
                        self.give_back(hwnd);
                        self.released.insert(pid, exe.clone());
                    }
                    self.pending.retain(|_, (_, e)| *e != exe);
                }
                Order::Retake(exe) => {
                    let exe = key(&exe);
                    self.released.retain(|_, e| *e != exe);
                    self.scan();
                }
            }
        }
    }

    /// One look at all windows: every game with an entry that is not held yet is taken – at once, its window is
    /// there already.
    fn scan(&mut self) {
        for w in win::windows() {
            self.seen(&w, false);
        }
    }

    /// A window that could be a game. `fresh`: it has just appeared – then the entry's waiting time runs first,
    /// so the game can finish building its window.
    fn seen(&mut self, w: &win::Window, fresh: bool) {
        let exe = key(&w.exe);
        let Some(game) = self.game(&exe) else { return };
        let delay = Duration::from_secs(game.delay as u64);
        if self.held.values().any(|h| h.pid == w.pid) || self.released.contains_key(&w.pid) {
            return;
        }
        if fresh && !delay.is_zero() {
            self.pending.entry(w.pid).or_insert((Instant::now() + delay, exe));
        } else {
            self.pending.remove(&w.pid);
            self.take(w.pid, &exe);
        }
    }

    /// Takes the biggest window of a process – a launcher's small window of the same program is not the game.
    fn take(&mut self, pid: u32, exe: &str) {
        let Some(game) = self.game(exe) else { return };
        let target = Target::of(game);
        let Some(w) = win::windows().into_iter().filter(|w| w.pid == pid).max_by_key(|w| w.area) else { return };
        self.hold(w.hwnd, pid, exe, target, false);
    }

    fn hold(&mut self, hwnd: Hwnd, pid: u32, exe: &str, target: Target, by_key: bool) {
        self.taken.lock().unwrap_or_else(|e| e.into_inner()).entry(hwnd).or_insert_with(|| win::original(hwnd));
        self.held.insert(hwnd, Held { pid, exe: exe.to_string(), target, by_key, state: GameState::Held, fixes: vec![], on_top_now: false, top_lost: false });
        if let std::collections::hash_map::Entry::Vacant(e) = self.moves.entry(pid) {
            let h = hook(EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE, pid);
            if !h.is_null() {
                e.insert(h);
            }
        }
        self.put(hwnd);
    }

    /// Puts a held window into its zone, on the screen it is on.
    fn put(&mut self, hwnd: Hwnd) {
        let Some(h) = self.held.get_mut(&hwnd) else { return };
        if win::minimized(hwnd) {
            h.state = GameState::Minimized;
            return;
        }
        let zone = h.target.zone.rect(win::screen_of(hwnd));
        h.state = match win::place(hwnd, zone, h.target.borderless) {
            Ok(()) => GameState::Held,
            Err(win::Denied) => GameState::Denied,
        };
        self.on_top(hwnd, win::is_active(hwnd, win::foreground()));
    }

    /// The window moved, or the regular look: back into the zone if the game undid it.
    fn check(&mut self, hwnd: Hwnd) {
        if !win::exists(hwnd) {
            self.forget(hwnd);
            return;
        }
        let Some(h) = self.held.get_mut(&hwnd) else { return };
        if matches!(h.state, GameState::Denied | GameState::Refused) {
            return;
        }
        if win::minimized(hwnd) {
            h.state = GameState::Minimized;
            return;
        }
        let zone = h.target.zone.rect(win::screen_of(hwnd));
        if win::fits(hwnd, zone, h.target.borderless) {
            h.state = GameState::Held;
            return;
        }
        let now = Instant::now();
        h.fixes.retain(|t| now.duration_since(*t) < FIX_TIME);
        if h.fixes.len() >= FIXES {
            h.state = GameState::Refused;
            kit::app::log(&format!("{} does not take the size {}×{}", h.exe, zone.w, zone.h));
            return;
        }
        h.fixes.push(now);
        self.put(hwnd);
    }

    /// Above the taskbar only while the game is the active window – a click elsewhere takes it back. A program
    /// that takes "on top" away by itself while active (Citrix does) gets it again – at the latest with the
    /// regular look a second later.
    fn on_top(&mut self, hwnd: Hwnd, active: bool) {
        let Some(h) = self.held.get_mut(&hwnd) else { return };
        let want = active && h.target.on_top && h.state != GameState::Denied;
        let lost = want && h.on_top_now && !win::topmost(hwnd);
        if want != h.on_top_now || lost {
            if lost && !h.top_lost {
                h.top_lost = true;
                kit::app::log(&format!("{} took \"on top\" away by itself – set again", h.exe));
            }
            win::set_topmost(hwnd, want);
            h.on_top_now = want;
        }
    }

    fn give_back(&mut self, hwnd: Hwnd) {
        if let Some(o) = self.taken.lock().unwrap_or_else(|e| e.into_inner()).remove(&hwnd) {
            win::give_back(hwnd, &o);
        }
        self.forget(hwnd);
    }

    /// The window is no longer held (gone, or given back).
    fn forget(&mut self, hwnd: Hwnd) {
        self.taken.lock().unwrap_or_else(|e| e.into_inner()).remove(&hwnd);
        let Some(h) = self.held.remove(&hwnd) else { return };
        if !self.held.values().any(|o| o.pid == h.pid)
            && let Some(hook) = self.moves.remove(&h.pid)
        {
            unsafe { UnhookWinEvent(hook) };
        }
    }

    /// One event from Windows.
    pub fn event(&mut self, event: u32, hwnd: Hwnd) {
        match event {
            EVENT_OBJECT_SHOW => {
                if let Some(w) = win::window(hwnd) {
                    self.list_due.get_or_insert(Instant::now() + LIST_DELAY);
                    self.seen(&w, true);
                }
            }
            EVENT_OBJECT_HIDE | EVENT_OBJECT_DESTROY => {
                if self.listed.contains(&hwnd) {
                    self.list_due.get_or_insert(Instant::now() + LIST_DELAY);
                }
                if event == EVENT_OBJECT_DESTROY && self.held.contains_key(&hwnd) {
                    self.forget(hwnd);
                }
            }
            EVENT_OBJECT_LOCATIONCHANGE => {
                if self.held.contains_key(&hwnd) {
                    self.check(hwnd);
                }
            }
            EVENT_SYSTEM_FOREGROUND => {
                for other in self.held.keys().copied().collect::<Vec<_>>() {
                    self.on_top(other, win::is_active(other, hwnd));
                }
                if let Some(h) = self.held.get_mut(&hwnd) {
                    // Clicked into: a game that refused gets another try.
                    if h.state == GameState::Refused {
                        h.state = GameState::Held;
                        h.fixes.clear();
                    }
                    self.check(hwnd);
                } else if let Some(w) = win::window(hwnd) {
                    self.seen(&w, true); // in case its showing was missed
                }
            }
            EVENT_SYSTEM_MINIMIZEEND => {
                if let Some(h) = self.held.get_mut(&hwnd) {
                    h.state = GameState::Held;
                    h.fixes.clear();
                    self.check(hwnd);
                }
            }
            _ => {}
        }
    }

    /// A key was pressed (`KEYS`): the active window moves into the key's zone, or gets its frame back.
    /// Returns the zone for its entry, if the window has one – the worker saves it, as if chosen on the page.
    pub fn key_pressed(&mut self, id: i32) -> Option<(String, Zone)> {
        let (_, _, zone, _) = KEYS.into_iter().find(|k| k.0 == id)?;
        let w = win::window(win::root(win::foreground()))?;
        let exe = key(&w.exe);
        let has_entry = self.settings.games.iter().any(|g| key(&g.exe) == exe);
        let held = self.held.iter().find(|(_, h)| h.pid == w.pid).map(|(hwnd, _)| *hwnd);
        let Some(zone) = zone else {
            // Frame back.
            if let Some(hwnd) = held {
                self.give_back(hwnd);
                if has_entry {
                    self.released.insert(w.pid, exe);
                }
            }
            if self.report.moved.as_ref().is_some_and(|m| key(&m.exe) == key(&w.exe)) {
                self.report.moved = None;
            }
            return None;
        };
        if has_entry {
            self.released.remove(&w.pid);
            return Some((w.exe, zone)); // the new settings put it there
        }
        let target = Target { zone, borderless: true, on_top: true };
        match held {
            Some(hwnd) => {
                let h = self.held.get_mut(&hwnd).expect("just found");
                h.target = target;
                h.fixes.clear();
                self.put(hwnd);
            }
            None => self.hold(w.hwnd, w.pid, &exe, target, true),
        }
        self.report.moved = Some(Moved { exe: w.exe, title: w.title, zone });
        None
    }

    /// The time-driven part: games whose waiting time is over, the regular look at held games.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let due: Vec<(u32, String)> = self.pending.iter().filter(|(_, (t, _))| *t <= now).map(|(pid, (_, exe))| (*pid, exe.clone())).collect();
        for (pid, exe) in due {
            self.pending.remove(&pid);
            self.take(pid, &exe);
        }
        if !self.held.is_empty() && now.duration_since(self.last_look) >= LOOK {
            self.last_look = now;
            let fg = win::foreground();
            for hwnd in self.held.keys().copied().collect::<Vec<_>>() {
                self.check(hwnd);
                self.on_top(hwnd, win::is_active(hwnd, fg));
            }
        }
    }

    /// How long the loop may sleep until `tick` has something to do (`None`: until Windows reports something).
    pub fn sleep_time(&self) -> Option<Duration> {
        let now = Instant::now();
        let mut until: Vec<Instant> = self.pending.values().map(|(t, _)| *t).collect();
        until.extend(self.list_due);
        if !self.held.is_empty() {
            until.push(self.last_look + LOOK);
        }
        until.into_iter().min().map(|t| t.saturating_duration_since(now))
    }

    /// The list "Add a game", if it is due: windows of programs without an entry, one per program.
    pub fn list(&mut self) -> Option<Vec<win::Window>> {
        if self.list_due.is_none_or(|t| t > Instant::now()) {
            return None;
        }
        self.list_due = None;
        let all = win::windows();
        self.listed = all.iter().map(|w| w.hwnd).collect();
        Some(addable(all, &self.settings))
    }

    /// The state of every entry.
    pub fn report(&self) -> Report {
        let mut r = self.report.clone();
        for g in &self.settings.games {
            let exe = key(&g.exe);
            let state = if let Some(h) = self.held.values().find(|h| h.exe == exe && !h.by_key) {
                h.state
            } else if self.pending.values().any(|(_, e)| *e == exe) {
                GameState::Waiting
            } else if self.released.values().any(|e| *e == exe) {
                GameState::Released
            } else {
                GameState::NotRunning
            };
            r.games.insert(exe, state);
        }
        r
    }
}

/// Windows of programs that have no entry yet – one per program, its biggest window.
pub fn addable(all: Vec<win::Window>, settings: &Settings) -> Vec<win::Window> {
    let mut out: Vec<win::Window> = vec![];
    for w in all.into_iter().filter(|w| !w.title.trim().is_empty()) {
        if settings.games.iter().any(|g| key(&g.exe) == key(&w.exe)) {
            continue;
        }
        match out.iter_mut().find(|o| key(&o.exe) == key(&w.exe)) {
            Some(o) if o.area < w.area => *o = w,
            Some(_) => {}
            None => out.push(w),
        }
    }
    out
}

/// The worker's thread (kit::app::run). It runs a Windows message loop, because Windows delivers the window
/// events and the keys as messages to the thread that asked for them.
pub fn work(rt: Runtime<Zones>) {
    let tid = unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() };
    // The thread's message queue exists from its first look into it – before anybody posts to it.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    // The kit wakes `Runtime::sleep` on a changed setting, a button, Start and Stop; this thread passes that on
    // to the message loop.
    {
        let rt = rt.clone();
        std::thread::spawn(move || {
            loop {
                rt.sleep(Duration::from_secs(3600));
                unsafe { PostThreadMessageW(tid, WM_WAKE, 0, 0) };
            }
        });
    }
    let taken = rt.with(|p, _| p.taken.clone());
    loop {
        rt.wait_running();
        let settings = rt.with(|p, s| {
            p.running = true;
            p.orders.clear();
            s.clone()
        });
        let mut w = Watcher::new(settings, taken.clone());
        w.start();
        publish(&rt, &mut w, true);
        while rt.running() {
            let (mut woken, keys) = round(&mut w, None);
            for id in keys {
                if let Some((exe, zone)) = w.key_pressed(id) {
                    // Like choosing the zone on the page: saved, and the settings put the window there.
                    rt.update(|s| {
                        if let Some(g) = s.games.iter_mut().find(|g| key(&g.exe) == key(&exe)) {
                            g.zone = zone;
                            g.enabled = true;
                        }
                    });
                    woken = true;
                }
            }
            if woken {
                let (settings, orders) = rt.with(|p, s| (s.clone(), std::mem::take(&mut p.orders)));
                w.orders(orders);
                w.settings(settings);
            }
            w.tick();
            publish(&rt, &mut w, false);
        }
        w.stop();
        rt.with(|p, _| {
            p.running = false;
            p.report = Report::default();
        });
        rt.changed();
    }
}

/// One round of the message loop: waits until Windows reports something, the thread is woken or `tick` has work
/// (at most `limit`), then works through the window events. Returns whether the thread was woken and the keys
/// pressed.
fn round(w: &mut Watcher, limit: Option<Duration>) -> (bool, Vec<i32>) {
    let sleep = match (w.sleep_time(), limit) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    let timeout = sleep.map_or(windows_sys::Win32::System::Threading::INFINITE, |d| d.as_millis().min(u32::MAX as u128 - 1) as u32);
    unsafe { MsgWaitForMultipleObjectsEx(0, std::ptr::null(), timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
    let (mut woken, mut keys) = (false, vec![]);
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    // The window events are delivered while the queue is looked into: their callback runs inside PeekMessage.
    while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
        match msg.message {
            WM_HOTKEY => keys.push(msg.wParam as i32),
            WM_WAKE => woken = true,
            _ => unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            },
        }
    }
    for (event, hwnd) in EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut())) {
        w.event(event, hwnd);
    }
    (woken, keys)
}

/// Hands what changed to the program – status, page and tray are sent only if they really differ.
fn publish(rt: &Runtime<Zones>, w: &mut Watcher, first: bool) {
    let report = w.report();
    let list = w.list();
    let screen: Rect = win::main_screen();
    // Games that were not running a moment ago: "Last played" is now.
    let started: Vec<String> = rt.with(|p, _| {
        report.games.iter().filter(|(exe, state)| **state != GameState::NotRunning && p.report.games.get(*exe).is_none_or(|old| *old == GameState::NotRunning)).map(|(exe, _)| exe.clone()).collect()
    });
    if !started.is_empty() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        rt.update(|s| {
            for g in s.games.iter_mut().filter(|g| started.contains(&key(&g.exe))) {
                g.last = now;
            }
        });
    }
    let changed = rt.with(|p, _| {
        let mut changed = first || p.report != report || p.screen != screen;
        p.report = report;
        p.screen = screen;
        if let Some(list) = list {
            changed |= p.windows != list;
            p.windows = list;
        }
        changed
    });
    if changed {
        rt.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(exe: &str, title: &str, area: i64) -> win::Window {
        win::Window { hwnd: area as Hwnd, pid: 1, exe: exe.into(), title: title.into(), area }
    }

    #[test]
    fn the_list_offers_each_program_once() {
        let settings = Settings { games: vec![Game { exe: "Rayman Legends.exe".into(), ..Game::default() }], ..Settings::default() };
        let list = addable(
            vec![window("rayman legends.EXE", "Rayman", 9), window("game.exe", "Launcher", 4), window("game.exe", "Game", 8), window("game.exe", "", 99), window("other.exe", "Other", 1)],
            &settings,
        );
        let names: Vec<&str> = list.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(names, ["Game", "Other"], "no entry twice, the biggest window, none without a title");
    }

    /// The whole chain with Windows' real events, against a window of another process (this test binary started
    /// a second time): it appears, the waiting time runs, it is taken; the "game" brings its frame back and loses it
    /// again; another zone moves it at once; Stop gives everything back. The window is fully transparent, lets every
    /// click through and is never activated – nothing to see but a taskbar button for a few seconds. Not part of a
    /// normal `cargo test` (it needs the desktop): `cargo test -- --ignored --test-threads=1`.
    #[test]
    #[ignore]
    fn a_game_window_of_another_process() {
        if std::env::var_os(CHILD).is_some() {
            return;
        }
        use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_string_lossy().to_string();
        let settings = Settings { games: vec![Game { name: "Test".into(), exe: name.clone(), zone: Zone::Right, delay: 1, ..Game::default() }], ..Settings::default() };
        let taken: Taken = Arc::default();
        let mut w = Watcher::new(settings, taken.clone());
        w.start();
        let mut child = std::process::Command::new(&exe).args(["--ignored", "--exact", "watcher::tests::test_window", "--test-threads=1"]).env(CHILD, "1").spawn().unwrap();
        let run = |w: &mut Watcher, time: Duration, until: &dyn Fn(&Watcher) -> bool| {
            let end = Instant::now() + time;
            while Instant::now() < end && !until(w) {
                round(w, Some(Duration::from_millis(50)));
                w.tick();
            }
        };
        let game = name.to_lowercase();
        let state = move |w: &Watcher| w.report().games.get(&game).copied();
        run(&mut w, Duration::from_secs(10), &|w| state(w) == Some(GameState::Waiting));
        assert_eq!(state(&w), Some(GameState::Waiting), "the window was seen, the waiting time runs");
        let seen = Instant::now();
        run(&mut w, Duration::from_secs(5), &|w| state(w) == Some(GameState::Held));
        assert_eq!(state(&w), Some(GameState::Held));
        assert!(seen.elapsed() > Duration::from_millis(700), "after the waiting time");
        let (&hwnd, held) = w.held.iter().next().unwrap();
        assert_eq!(held.pid, child.id());
        let zone = Zone::Right.rect(win::screen_of(hwnd));
        assert!(win::fits(hwnd, zone, true), "borderless in the right zone");

        // The game brings its frame back: within the regular look it is gone again.
        let style = unsafe { GetWindowLongPtrW(hwnd as _, GWL_STYLE) };
        unsafe { SetWindowLongPtrW(hwnd as _, GWL_STYLE, style | (WS_CAPTION | WS_THICKFRAME) as isize) };
        assert!(!win::fits(hwnd, zone, true));
        run(&mut w, Duration::from_secs(3), &|_| win::fits(hwnd, zone, true));
        assert!(win::fits(hwnd, zone, true), "the frame was taken again");

        // The game moves itself: back at once (the location event).
        unsafe { SetWindowPos(hwnd as _, std::ptr::null_mut(), 10, 10, 640, 480, SWP_NOZORDER | SWP_NOACTIVATE) };
        run(&mut w, Duration::from_millis(900), &|_| win::fits(hwnd, zone, true));
        assert!(win::fits(hwnd, zone, true), "moved back before the regular look");

        // Another zone on the page: moved at once.
        let mut s = w.settings.clone();
        s.games[0].zone = Zone::Left;
        w.settings(s);
        assert!(win::fits(hwnd, Zone::Left.rect(win::screen_of(hwnd)), true));

        w.stop();
        let style = unsafe { GetWindowLongPtrW(hwnd as _, GWL_STYLE) };
        assert_ne!(style & WS_CAPTION as isize, 0, "Stop gives the frame back");
        let mut r: windows_sys::Win32::Foundation::RECT = unsafe { std::mem::zeroed() };
        unsafe { GetWindowRect(hwnd as _, &mut r) };
        assert_eq!((r.left, r.top, r.right - r.left, r.bottom - r.top), (200, 200, 900, 600), "and its old place");
        assert!(taken.lock().unwrap().is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }

    const CHILD: &str = "SCREEN_ZONES_TEST_WINDOW";

    /// The "game" of the test above – does nothing unless started by it.
    #[test]
    #[ignore]
    fn test_window() {
        use windows_sys::Win32::Graphics::Gdi::{GetStockObject, HBRUSH, WHITE_BRUSH};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
        if std::env::var_os(CHILD).is_none() {
            return;
        }
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let class: Vec<u16> = "ScreenZonesTestGame\0".encode_utf16().collect();
            let wc = WNDCLASSW { lpfnWndProc: Some(DefWindowProcW), hInstance: GetModuleHandleW(std::ptr::null()), lpszClassName: class.as_ptr(), hbrBackground: GetStockObject(WHITE_BRUSH) as HBRUSH, ..std::mem::zeroed() };
            RegisterClassW(&wc);
            let title: Vec<u16> = "Screen Zones test game\0".encode_utf16().collect();
            // Layered and fully transparent: invisible, and every click goes through to what lies below.
            let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
            let hwnd = CreateWindowExW(ex, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW, 200, 200, 900, 600, std::ptr::null_mut(), std::ptr::null_mut(), wc.hInstance, std::ptr::null());
            SetLayeredWindowAttributes(hwnd, 0, 0, LWA_ALPHA);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let end = Instant::now() + Duration::from_secs(20);
            let mut msg: MSG = std::mem::zeroed();
            while Instant::now() < end {
                while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// What is taken on this PC: every combination switched on once, its keys registered and given back again.
    /// Global keys, if only for a moment – so not part of a normal `cargo test`: `cargo test -- --ignored
    /// keys_on_this_pc --nocapture`.
    #[test]
    #[ignore]
    fn keys_on_this_pc() {
        kit::texts!();
        kit::i18n::set_language("en");
        let mut w = Watcher::new(Settings::default(), Taken::default());
        for set in KeySet::ALL {
            w.keys(Some(set));
            println!("{}: taken arrows {:?}, taken sets {:?}", set.name(), w.report.taken_keys, w.report.taken_sets);
            assert_eq!(w.report.taken_sets.contains(&set), !w.report.taken_keys.is_empty(), "the own set as registered");
        }
        w.keys(None);
        assert!(w.report.taken_keys.is_empty() && w.report.taken_sets.is_empty());
    }

    /// The watcher with a real (hidden) window of this test process: a key on a window without entry, then the
    /// window given back – and on `stop` nothing stays taken.
    #[test]
    fn report_and_give_back() {
        let taken: Taken = Arc::default();
        let settings = Settings { games: vec![Game { exe: "Game.exe".into(), ..Game::default() }, Game { exe: "Off.exe".into(), enabled: false, ..Game::default() }], ..Settings::default() };
        let mut w = Watcher::new(settings, taken.clone());
        let r = w.report();
        assert_eq!(r.games.get("game.exe"), Some(&GameState::NotRunning));
        assert_eq!(r.games.get("off.exe"), Some(&GameState::NotRunning));

        let hwnd = win::tests::hidden_window();
        w.hold(hwnd, 7, "game.exe", Target { zone: Zone::Center, borderless: true, on_top: false }, false);
        assert!(taken.lock().unwrap().contains_key(&hwnd));
        assert_eq!(w.report().games.get("game.exe"), Some(&GameState::Held));
        w.pending.insert(8, (Instant::now(), "game.exe".into()));
        assert!(w.sleep_time().is_some_and(|d| d.is_zero()));

        // Removing the entry gives the window back.
        w.settings(Settings::default());
        assert!(taken.lock().unwrap().is_empty() && w.held.is_empty() && w.pending.is_empty());

        // Release, then the zone chosen anew: wanted back.
        let game = Game { exe: "Game.exe".into(), ..Game::default() };
        w.settings(Settings { games: vec![game.clone()], ..Settings::default() });
        w.released.insert(7, "game.exe".into());
        assert_eq!(w.report().games.get("game.exe"), Some(&GameState::Released));
        w.settings(Settings { games: vec![Game { zone: Zone::Left, ..game }], ..Settings::default() });
        assert!(w.released.is_empty());

        w.hold(hwnd, 7, "game.exe", Target { zone: Zone::Left, borderless: true, on_top: false }, false);
        w.stop();
        assert!(taken.lock().unwrap().is_empty() && w.held.is_empty());
        let style = unsafe { GetWindowLongPtrW(hwnd as _, GWL_STYLE) } as u32;
        assert_ne!(style & WS_CAPTION, 0, "the frame is back");
        unsafe { DestroyWindow(hwnd as _) };
    }
}
