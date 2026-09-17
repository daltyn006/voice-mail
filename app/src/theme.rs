//! Present Voice palette + theme application.
//!
//! Neutrals are true grays (no blue tint — the old tinted navbar is
//! gone); per-mode helpers serve our custom chrome while kit widgets follow
//! `Theme::change`. Functional blues get a slight desaturation pass so the
//! whole UI reads calmer.

use gpui_kit::component::theme::{Theme, ThemeMode};
use gpui_kit::{App, Hsla};

/// Keep 85% of blue saturation on functional accents.
pub const BLUE_DESAT: f32 = 0.85;

pub const BG_DARK: u32 = 0x17_17_17_ff;
pub const SURFACE_DARK: u32 = 0x21_21_21_ff;
pub const FG_DARK: u32 = 0xe8_e8_e8_ff;
/// Light surfaces (true neutral).
pub const BG_LIGHT: u32 = 0xf5_f5_f5_ff;
pub const SURFACE_LIGHT: u32 = 0xff_ff_ff_ff;
pub const FG_LIGHT: u32 = 0x17_17_17_ff;

pub fn bg(dark: bool) -> u32 {
    if dark {
        BG_DARK
    } else {
        BG_LIGHT
    }
}

pub fn fg(dark: bool) -> u32 {
    if dark {
        FG_DARK
    } else {
        FG_LIGHT
    }
}

pub fn surface(dark: bool) -> u32 {
    if dark {
        SURFACE_DARK
    } else {
        SURFACE_LIGHT
    }
}

/// Pure color math: scale saturation, keep hue/lightness/alpha.
pub fn desat(c: Hsla, keep: f32) -> Hsla {
    Hsla {
        s: (c.s * keep).clamp(0.0, 1.0),
        ..c
    }
}

/// Apply the mode to kit widgets, then calm the functional blues.
/// Call on boot (after `init`) and on every toggle.
pub fn apply_theme(dark: bool, cx: &mut App) {
    Theme::change(
        if dark {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    let colors = &mut Theme::global_mut(cx).colors;
    for slot in [
        &mut colors.selection,
        &mut colors.link,
        &mut colors.link_active,
        &mut colors.link_hover,
        &mut colors.drag_border,
        &mut colors.drop_target,
    ] {
        *slot = desat(*slot, BLUE_DESAT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desat_keeps_hue_lightness_alpha() {
        let c = Hsla {
            h: 0.6,
            s: 0.8,
            l: 0.5,
            a: 1.0,
        };
        let d = desat(c, 0.85);
        assert_eq!((d.h, d.l, d.a), (0.6, 0.5, 1.0));
        assert!((d.s - 0.68).abs() < 1e-6);
    }

    #[test]
    fn desat_clamps() {
        let c = Hsla {
            h: 0.0,
            s: 2.0,
            l: 0.5,
            a: 1.0,
        };
        assert_eq!(desat(c, 2.0).s, 1.0);
    }

    #[test]
    fn mode_helpers_split() {
        assert_ne!(bg(true), bg(false));
        assert_ne!(fg(true), fg(false));
        assert_ne!(surface(true), surface(false));
    }
}
