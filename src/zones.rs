//! Where a zone lies on a screen – plain arithmetic, no Windows calls.
//!
//! A zone is not stored in pixels but as a description (an aspect ratio or a share of the width, and where it is
//! anchored), so it survives another resolution: 16:9 in the middle of 5120×1440 is 2560×1440 at x = 1280, on a
//! 3440×1440 screen it is the same size at x = 440.

use serde::{Deserialize, Serialize};

/// A rectangle in physical pixels (the program is DPI aware, so Windows' scaling does not count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }
}

/// The zones that come with the program. A new one is a line here, one in `ALL`, one in `rect` and its texts.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Zone {
    /// 16:9 at full height, on the left.
    Left,
    /// 16:9 at full height, in the middle – Rayman's 2560×1440 on 5120×1440.
    #[default]
    Center,
    /// 16:9 at full height, on the right.
    Right,
    /// 21:9 at full height, in the middle.
    Wide,
    LeftHalf,
    RightHalf,
    /// The whole screen (borderless fullscreen).
    Full,
}

impl Zone {
    pub const ALL: [Zone; 7] = [Zone::Left, Zone::Center, Zone::Right, Zone::Wide, Zone::LeftHalf, Zone::RightHalf, Zone::Full];

    /// The name in settings and texts (`zone.left`).
    pub fn key(self) -> &'static str {
        match self {
            Zone::Left => "left",
            Zone::Center => "center",
            Zone::Right => "right",
            Zone::Wide => "wide",
            Zone::LeftHalf => "left_half",
            Zone::RightHalf => "right_half",
            Zone::Full => "full",
        }
    }

    /// Where the zone lies on a screen (`screen`: the monitor's rectangle).
    pub fn rect(self, screen: Rect) -> Rect {
        match self {
            Zone::Left => aspect(screen, 16, 9, Anchor::Start),
            Zone::Center => aspect(screen, 16, 9, Anchor::Middle),
            Zone::Right => aspect(screen, 16, 9, Anchor::End),
            Zone::Wide => aspect(screen, 21, 9, Anchor::Middle),
            Zone::LeftHalf => Rect::new(screen.x, screen.y, screen.w / 2, screen.h),
            Zone::RightHalf => Rect::new(screen.x + screen.w - screen.w / 2, screen.y, screen.w / 2, screen.h),
            Zone::Full => screen,
        }
    }
}

enum Anchor {
    Start,
    Middle,
    End,
}

/// The largest rectangle of this aspect ratio that fits: full height on a wide screen; on a screen narrower than
/// the ratio, full width and centred vertically (16:9 on 1920×1200 is 1920×1080 at y = 60).
fn aspect(screen: Rect, wide: i64, high: i64, anchor: Anchor) -> Rect {
    let (sw, sh) = (screen.w as i64, screen.h as i64);
    let (w, h) = if sh * wide / high <= sw { (sh * wide / high, sh) } else { (sw, sw * high / wide) };
    let (w, h) = (w as i32, h as i32);
    let x = match anchor {
        Anchor::Start => screen.x,
        Anchor::Middle => screen.x + (screen.w - w) / 2,
        Anchor::End => screen.x + screen.w - w,
    };
    Rect::new(x, screen.y + (screen.h - h) / 2, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ultrawide_screen() {
        let s = Rect::new(0, 0, 5120, 1440);
        assert_eq!(Zone::Left.rect(s), Rect::new(0, 0, 2560, 1440));
        assert_eq!(Zone::Center.rect(s), Rect::new(1280, 0, 2560, 1440));
        assert_eq!(Zone::Right.rect(s), Rect::new(2560, 0, 2560, 1440));
        assert_eq!(Zone::Wide.rect(s), Rect::new(880, 0, 3360, 1440));
        assert_eq!(Zone::LeftHalf.rect(s), Rect::new(0, 0, 2560, 1440));
        assert_eq!(Zone::RightHalf.rect(s), Rect::new(2560, 0, 2560, 1440));
        assert_eq!(Zone::Full.rect(s), s);
    }

    #[test]
    fn other_screens() {
        // 21:9 (3440×1440): 16:9 in the middle.
        assert_eq!(Zone::Center.rect(Rect::new(0, 0, 3440, 1440)), Rect::new(440, 0, 2560, 1440));
        // 16:9 itself: every 16:9 zone is the whole screen.
        let hd = Rect::new(0, 0, 1920, 1080);
        assert_eq!((Zone::Left.rect(hd), Zone::Right.rect(hd)), (hd, hd));
        // Narrower than the ratio: full width, centred vertically.
        assert_eq!(Zone::Center.rect(Rect::new(0, 0, 1920, 1200)), Rect::new(0, 60, 1920, 1080));
        assert_eq!(Zone::Wide.rect(hd), Rect::new(0, 129, 1920, 822));
        // A second monitor left of the first one, and an odd width: the halves still cover it.
        let second = Rect::new(-2561, 0, 2561, 1440);
        assert_eq!(Zone::Right.rect(second), Rect::new(-2560, 0, 2560, 1440));
        assert_eq!(Zone::LeftHalf.rect(second), Rect::new(-2561, 0, 1280, 1440));
        assert_eq!(Zone::RightHalf.rect(second), Rect::new(-1280, 0, 1280, 1440));
    }

    #[test]
    fn names() {
        for z in Zone::ALL {
            assert_eq!(serde_json::to_value(z).unwrap(), z.key());
        }
    }
}
