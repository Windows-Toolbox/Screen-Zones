//! Every Windows call about other programs' windows: finding them, taking their frame, placing them, giving
//! everything back. Only the normal window functions of Windows – nothing is injected into a game, the same
//! technique as Borderless Gaming.
//!
//! A window is kept as a number (`Hwnd`) – Windows' handle, which is only a name and may be handed between threads.

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, HWND, LPARAM, POINT, RECT, SetLastError};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint, MonitorFromWindow};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use windows_sys::core::BOOL;

use crate::zones::Rect;

pub type Hwnd = isize;

fn h(hwnd: Hwnd) -> HWND {
    hwnd as HWND
}

/// The frame: title bar, sizing border, system menu, minimise and maximise buttons (WS_OVERLAPPEDWINDOW).
const FRAME: isize = (WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX) as isize;
/// The extended styles that draw an edge around the window.
const EDGES: isize = (WS_EX_DLGMODALFRAME | WS_EX_CLIENTEDGE | WS_EX_STATICEDGE | WS_EX_WINDOWEDGE) as isize;
/// A window smaller than this is no game window: splash screens, launchers' small windows, helpers.
const MIN_W: i32 = 200;
const MIN_H: i32 = 150;
/// Windows of the desktop and the taskbar – never a game.
const SHELL: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];
/// Programs whose windows are no program of their own (the frame of every Store app belongs to it).
const NOT_A_PROGRAM: [&str; 1] = ["applicationframehost.exe"];

/// A window that could be a game: what the list "Add a game" shows and what a game's entry is matched against.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub hwnd: Hwnd,
    pub pid: u32,
    /// The program's file name, as Windows writes it ("Rayman Legends.exe").
    pub exe: String,
    pub title: String,
    pub area: i64,
}

/// The file name of a process' program – `None` if Windows does not tell (the process ended meanwhile).
pub fn exe_of(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(process);
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        ok.then(|| path.rsplit('\\').next().unwrap_or(&path).to_string())
    }
}

pub fn pid_of(hwnd: Hwnd) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h(hwnd), &mut pid) };
    pid
}

pub fn title(hwnd: Hwnd) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h(hwnd), buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn class(hwnd: Hwnd) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(h(hwnd), buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_rect(hwnd: Hwnd) -> Option<Rect> {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    (unsafe { GetWindowRect(h(hwnd), &mut r) } != 0).then(|| Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top))
}

/// The rectangle one sees: without the invisible border Windows 10/11 puts around a framed window for resizing.
fn visible_rect(hwnd: Hwnd) -> Option<Rect> {
    let mut r: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe { DwmGetWindowAttribute(h(hwnd), DWMWA_EXTENDED_FRAME_BOUNDS as u32, (&mut r as *mut RECT).cast(), size_of::<RECT>() as u32) } == 0;
    if ok { Some(Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)) } else { window_rect(hwnd) }
}

fn cloaked(hwnd: Hwnd) -> bool {
    let mut value: u32 = 0;
    let ok = unsafe { DwmGetWindowAttribute(h(hwnd), DWMWA_CLOAKED as u32, (&mut value as *mut u32).cast(), 4) } == 0;
    ok && value != 0
}

/// The top-level window a window belongs to.
pub fn root(hwnd: Hwnd) -> Hwnd {
    unsafe { GetAncestor(h(hwnd), GA_ROOT) as Hwnd }
}

/// The window if it could be a game: a visible top-level window of its own (no owner, no tool window, not hidden
/// by Windows), big enough, not part of the desktop – `None` otherwise.
pub fn window(hwnd: Hwnd) -> Option<Window> {
    unsafe {
        if hwnd == 0 || IsWindowVisible(h(hwnd)) == 0 || root(hwnd) != hwnd || !GetWindow(h(hwnd), GW_OWNER).is_null() {
            return None;
        }
        if GetWindowLongPtrW(h(hwnd), GWL_EXSTYLE) & WS_EX_TOOLWINDOW as isize != 0 || cloaked(hwnd) {
            return None;
        }
    }
    let rect = window_rect(hwnd)?;
    if rect.w < MIN_W || rect.h < MIN_H || SHELL.contains(&class(hwnd).as_str()) {
        return None;
    }
    let pid = pid_of(hwnd);
    if pid == std::process::id() {
        return None;
    }
    let exe = exe_of(pid)?;
    if NOT_A_PROGRAM.contains(&exe.to_lowercase().as_str()) {
        return None;
    }
    Some(Window { hwnd, pid, exe, title: title(hwnd), area: rect.w as i64 * rect.h as i64 })
}

/// Every window that could be a game, front to back.
pub fn windows() -> Vec<Window> {
    unsafe extern "system" fn each(hwnd: HWND, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list as *mut Vec<Hwnd>) };
        list.push(hwnd as Hwnd);
        1
    }
    let mut all: Vec<Hwnd> = vec![];
    unsafe { EnumWindows(Some(each), &mut all as *mut Vec<Hwnd> as LPARAM) };
    all.into_iter().filter_map(window).collect()
}

pub fn exists(hwnd: Hwnd) -> bool {
    unsafe { IsWindow(h(hwnd)) != 0 }
}

pub fn minimized(hwnd: Hwnd) -> bool {
    unsafe { IsIconic(h(hwnd)) != 0 }
}

pub fn foreground() -> Hwnd {
    unsafe { GetForegroundWindow() as Hwnd }
}

/// Is `held` the active window – or does the active window belong to it? Some programs put a window of their own
/// over the held one (Citrix: an overlay of a second process, owned by the viewer); clicking into it must not
/// count as leaving the game.
pub fn is_active(held: Hwnd, foreground: Hwnd) -> bool {
    let mut w = if foreground == 0 { 0 } else { root(foreground) };
    for _ in 0..8 {
        if w == 0 {
            return false;
        }
        if w == held {
            return true;
        }
        w = unsafe { GetWindow(h(w), GW_OWNER) as Hwnd };
    }
    false
}

/// Is the window "always on top" right now? A program may take that away by itself (Citrix does).
pub fn topmost(hwnd: Hwnd) -> bool {
    unsafe { GetWindowLongPtrW(h(hwnd), GWL_EXSTYLE) & WS_EX_TOPMOST as isize != 0 }
}

fn monitor_rect(monitor: windows_sys::Win32::Graphics::Gdi::HMONITOR) -> Rect {
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<MONITORINFO>() as u32;
    unsafe { GetMonitorInfoW(monitor, &mut info) };
    let r = info.rcMonitor;
    Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
}

/// The screen a window is on (most of it) – its zone is worked out on that screen.
pub fn screen_of(hwnd: Hwnd) -> Rect {
    monitor_rect(unsafe { MonitorFromWindow(h(hwnd), MONITOR_DEFAULTTONEAREST) })
}

/// The main screen – for the sizes the page names.
pub fn main_screen() -> Rect {
    monitor_rect(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) })
}

/// How a window was before it was taken – to give it back exactly so.
#[derive(Clone, Copy)]
pub struct Original {
    style: isize,
    ex_style: isize,
    placement: WINDOWPLACEMENT,
}
// Only numbers inside.
unsafe impl Send for Original {}

pub fn original(hwnd: Hwnd) -> Original {
    unsafe {
        let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
        placement.length = size_of::<WINDOWPLACEMENT>() as u32;
        GetWindowPlacement(h(hwnd), &mut placement);
        Original { style: GetWindowLongPtrW(h(hwnd), GWL_STYLE), ex_style: GetWindowLongPtrW(h(hwnd), GWL_EXSTYLE), placement }
    }
}

/// Windows refused: the game runs with admin rights, and a normal program may not change its window.
#[derive(Debug, PartialEq)]
pub struct Denied;

/// `SetWindowLongPtr` returns the old value, which may be 0 – only the error code tells a refusal.
fn set_long(hwnd: Hwnd, index: WINDOW_LONG_PTR_INDEX, value: isize) -> Result<(), Denied> {
    unsafe {
        SetLastError(0);
        if SetWindowLongPtrW(h(hwnd), index, value) == 0 && GetLastError() == ERROR_ACCESS_DENIED {
            return Err(Denied);
        }
    }
    Ok(())
}

/// Puts a window into `zone`: without its frame (`borderless`) the window is exactly the zone; with its frame,
/// the visible frame lines up with the zone. Not activated, not brought to the front.
pub fn place(hwnd: Hwnd, zone: Rect, borderless: bool) -> Result<(), Denied> {
    unsafe {
        if IsZoomed(h(hwnd)) != 0 {
            // A maximised window ignores its size; back to normal first – without activating it.
            ShowWindow(h(hwnd), SW_SHOWNOACTIVATE);
        }
        let target = if borderless {
            let style = GetWindowLongPtrW(h(hwnd), GWL_STYLE);
            if style & FRAME != 0 {
                set_long(hwnd, GWL_STYLE, style & !FRAME)?;
            }
            let ex = GetWindowLongPtrW(h(hwnd), GWL_EXSTYLE);
            if ex & EDGES != 0 {
                set_long(hwnd, GWL_EXSTYLE, ex & !EDGES)?;
            }
            zone
        } else {
            // The window's rectangle includes an invisible border; make the visible part the zone.
            let (outer, seen) = (window_rect(hwnd).unwrap_or(zone), visible_rect(hwnd).unwrap_or(zone));
            let (left, top) = (seen.x - outer.x, seen.y - outer.y);
            let (right, bottom) = (outer.x + outer.w - seen.x - seen.w, outer.y + outer.h - seen.y - seen.h);
            Rect::new(zone.x - left, zone.y - top, zone.w + left + right, zone.h + top + bottom)
        };
        SetLastError(0);
        let flags = SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER;
        if SetWindowPos(h(hwnd), std::ptr::null_mut(), target.x, target.y, target.w, target.h, flags) == 0 && GetLastError() == ERROR_ACCESS_DENIED {
            return Err(Denied);
        }
    }
    Ok(())
}

/// Is the window where `place` puts it – frame gone (if `borderless`), position and size right?
pub fn fits(hwnd: Hwnd, zone: Rect, borderless: bool) -> bool {
    if borderless {
        let style = unsafe { GetWindowLongPtrW(h(hwnd), GWL_STYLE) };
        style & (WS_CAPTION | WS_THICKFRAME) as isize == 0 && window_rect(hwnd) == Some(zone)
    } else {
        visible_rect(hwnd) == Some(zone)
    }
}

/// "Always on top" – the only way to stay in front of the taskbar for a window that does not fill the whole
/// screen (Windows treats only those as full screen). Set only while the game is the active window.
pub fn set_topmost(hwnd: Hwnd, on: bool) {
    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe { SetWindowPos(h(hwnd), after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE) };
}

/// Frame, styles, position and size as they were before the window was taken. A minimised window stays
/// minimised; nothing is activated (except a window that was maximised – Windows has no quiet "maximise").
pub fn give_back(hwnd: Hwnd, original: &Original) {
    if !exists(hwnd) {
        return;
    }
    unsafe {
        let _ = set_long(hwnd, GWL_STYLE, original.style);
        let _ = set_long(hwnd, GWL_EXSTYLE, original.ex_style);
        SetWindowPos(h(hwnd), HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED);
        let mut placement = original.placement;
        placement.showCmd = if IsIconic(h(hwnd)) != 0 {
            SW_SHOWMINNOACTIVE as u32
        } else if placement.showCmd == SW_SHOWMAXIMIZED as u32 {
            SW_SHOWMAXIMIZED as u32
        } else if IsWindowVisible(h(hwnd)) != 0 {
            SW_SHOWNOACTIVATE as u32
        } else {
            SW_HIDE as u32
        };
        SetWindowPlacement(h(hwnd), &placement);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    /// A framed window of this test process that is never shown – nothing pops up (Toolbox\DESIGN.md 13).
    pub fn hidden_window() -> Hwnd {
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title: Vec<u16> = "Screen Zones test\0".encode_utf16().collect();
        unsafe {
            CreateWindowExW(0, class.as_ptr(), title.as_ptr(), WS_OVERLAPPEDWINDOW, 100, 100, 800, 600, std::ptr::null_mut(), std::ptr::null_mut(), GetModuleHandleW(std::ptr::null()), std::ptr::null()) as Hwnd
        }
    }

    #[test]
    fn take_and_give_back_a_window() {
        let hwnd = hidden_window();
        assert_ne!(hwnd, 0);
        let before = original(hwnd);
        let zone = Rect::new(-30000, -30000, 640, 360);
        assert!(!fits(hwnd, zone, true));
        assert_eq!(place(hwnd, zone, true), Ok(()));
        assert!(fits(hwnd, zone, true), "without frame and exactly the zone");
        let style = unsafe { GetWindowLongPtrW(h(hwnd), GWL_STYLE) };
        assert_eq!(style & FRAME, 0);
        assert_eq!(unsafe { IsWindowVisible(h(hwnd)) }, 0, "placing does not show the window");

        give_back(hwnd, &before);
        let style = unsafe { GetWindowLongPtrW(h(hwnd), GWL_STYLE) };
        assert_eq!(style & FRAME, FRAME, "the frame is back");
        assert_eq!(window_rect(hwnd), Some(Rect::new(100, 100, 800, 600)), "and the old place");
        assert_eq!(unsafe { IsWindowVisible(h(hwnd)) }, 0);

        // With its frame, only the place changes.
        assert_eq!(place(hwnd, zone, false), Ok(()));
        assert_eq!(unsafe { GetWindowLongPtrW(h(hwnd), GWL_STYLE) } & FRAME, FRAME);
        unsafe { DestroyWindow(h(hwnd)) };
        assert!(!exists(hwnd));
    }

    /// A window owned by the held one (Citrix's overlay) counts as the held one being active; "on top" is read
    /// from the window itself.
    #[test]
    fn active_through_an_owned_window_and_on_top() {
        let (held, other) = (hidden_window(), hidden_window());
        let class: Vec<u16> = "STATIC ".encode_utf16().collect();
        let owned = unsafe { CreateWindowExW(0, class.as_ptr(), class.as_ptr(), WS_POPUP, 0, 0, 300, 200, h(held), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null()) as Hwnd };
        assert_eq!(unsafe { GetWindow(h(owned), GW_OWNER) as Hwnd }, held);
        assert!(is_active(held, held) && is_active(held, owned));
        assert!(!is_active(held, other) && !is_active(held, 0) && !is_active(owned, held));
        assert!(!topmost(held));
        set_topmost(held, true);
        assert!(topmost(held));
        set_topmost(held, false);
        assert!(!topmost(held));
        assert_eq!(unsafe { IsWindowVisible(h(held)) }, 0, "nothing was shown");
        unsafe {
            DestroyWindow(h(owned));
            DestroyWindow(h(held));
            DestroyWindow(h(other));
        }
    }

    #[test]
    fn a_hidden_window_is_no_game() {
        let hwnd = hidden_window();
        assert_eq!(window(hwnd), None);
        assert!(windows().iter().all(|w| w.hwnd != hwnd && w.pid != std::process::id()));
        unsafe { DestroyWindow(h(hwnd)) };
        assert_eq!(exe_of(std::process::id()).map(|e| e.ends_with(".exe")), Some(true));
    }
}
