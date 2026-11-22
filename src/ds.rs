//! Design tokens: the one place that says how the interface is spaced, rounded and layered.
//! Colors stay in `theme::Chrome` (they follow the theme); everything geometric lives here so a
//! screen never invents a magic number. All values are logical pixels (`ui::sc` scales them).

#![allow(dead_code)]

/// 4-px spacing scale.
pub const S1: i32 = 4;
pub const S2: i32 = 8;
pub const S3: i32 = 12;
pub const S4: i32 = 16;
pub const S5: i32 = 24;
pub const S6: i32 = 32;

/// Corner radii: controls < rows < cards < floating surfaces.
pub const R_CTRL: i32 = 6;
pub const R_ROW: i32 = 8;
pub const R_CARD: i32 = 12;
pub const R_FLOAT: i32 = 16;

/// Gap between the floating cards of the shell (rail, sidebar, editor, dock).
pub const GAP: i32 = 8;

/// Icon sizes (HugeIcons masks are drawn square).
pub const ICON_S: i32 = 14;
pub const ICON_M: i32 = 16;
pub const ICON_L: i32 = 20;

/// Interactive rows: tall enough to hit, dense enough for code.
pub const HIT_MIN: i32 = 24;
pub const ROW_LIST: i32 = 28;

/// Elevation: 0 flat on the canvas, 1 card, 2 popover/menu, 3 modal/command palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elev {
    Flat,
    Card,
    Pop,
    Modal,
}

impl Elev {
    /// Alpha of the border ring relative to the theme border (hairline always present).
    pub fn lift(self) -> u8 {
        match self {
            Elev::Flat => 0,
            Elev::Card => 6,
            Elev::Pop => 10,
            Elev::Modal => 14,
        }
    }
}
