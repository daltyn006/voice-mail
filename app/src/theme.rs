//! voice mail palette + theme application.
//!
//! Six base appearances (Dark, Light, Gruvbox Dark/Light, Coffee Dark/Light)
//! plus a High Contrast *overlay* flag that maxes out text/border/label
//! contrast on top of whichever base is active (keeps the base background
//! hue so each theme stays recognizable).
//! Neutrals are true grays (no blue tint); functional blues get a slight desaturation
//! pass so the whole UI reads calmer. All tokens are accessible via helpers below.

use gpui_kit::component::theme::{Colorize as _, Theme, ThemeMode, ThemeTokens};
use gpui_kit::{div, px, rgba, App, Div, FontWeight, Hsla, ParentElement, SharedString, Styled};

/// Spacing scale (px): XS inside groups, SM between related rows, MD/LG
/// between sections, XL for page padding. Pages use XL padding with LG
/// section gaps; 8px-everywhere was the old dense look.
pub const SPACE_XS: f32 = 4.0;
pub const SPACE_SM: f32 = 8.0;
pub const SPACE_MD: f32 = 12.0;
pub const SPACE_LG: f32 = 16.0;
pub const SPACE_XL: f32 = 24.0;

/// Base theme id order. This is also the `ToggleTheme` (Ctrl-T) cycle order.
pub const THEME_ORDER: [&str; 6] = [
    "dark",
    "light",
    "gruvbox_dark",
    "gruvbox_light",
    "coffee_dark",
    "coffee_light",
];

/// Theme mode enum (stored in prefs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeModeExt {
    Dark,
    Light,
    GruvboxDark,
    GruvboxLight,
    CoffeeDark,
    CoffeeLight,
}

impl ThemeModeExt {
    pub fn from_str(s: &str) -> Self {
        match s {
            "light" => ThemeModeExt::Light,
            "gruvbox_dark" => ThemeModeExt::GruvboxDark,
            "gruvbox_light" => ThemeModeExt::GruvboxLight,
            "coffee_dark" => ThemeModeExt::CoffeeDark,
            "coffee_light" => ThemeModeExt::CoffeeLight,
            // Legacy `high_contrast` value (pre-overlay) maps to Dark; the
            // overlay flag is migrated separately in `Store::apply_prefs`.
            _ => ThemeModeExt::Dark,
        }
    }
    /// True for light-background themes (kit Light mode + dark text).
    pub fn is_light(&self) -> bool {
        matches!(
            self,
            ThemeModeExt::Light | ThemeModeExt::GruvboxLight | ThemeModeExt::CoffeeLight
        )
    }

    pub fn to_kit_mode(&self) -> ThemeMode {
        if self.is_light() {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        }
    }
}

/// Next base theme id in the Ctrl-T cycle order. Unknown ids restart at dark.
pub fn next_theme(current: &str) -> &'static str {
    let pos = THEME_ORDER.iter().position(|t| *t == current).unwrap_or(0);
    THEME_ORDER[(pos + 1) % THEME_ORDER.len()]
}

/// Palette for a given mode: chrome (bg/surface/fg/border), the accent
/// family, and semantic hues with pre-checked label colors. Fields are
/// public so views can derive contrast-safe colors via [`effective_palette`].
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: u32,
    pub surface: u32,
    pub fg: u32,
    pub border: u32,
    pub accent: u32,
    pub accent_fg: u32,
    /// Nav banner shade: equals `bg` on dark bases, a step darker on
    /// light ones so the banner reads as a distinct bar.
    pub banner: u32,
    pub danger: u32,
    pub danger_fg: u32,
    pub success: u32,
    pub success_fg: u32,
    pub warning: u32,
    pub warning_fg: u32,
    pub info: u32,
    pub info_fg: u32,
    pub link: u32,
    /// Secondary text (hints, paths, captions): dimmer than `fg` but still
    /// AA-readable on `bg`. Never use `fg` for hint text (no resting place).
    pub muted_fg: u32,
}

/// Dark+ inspired: editor `#1e1e1e`, side bar `#252526`.
const PALETTE_DARK: Palette = Palette {
    bg: 0x1e_1e_1e_ff,
    surface: 0x25_25_26_ff,
    fg: 0xd4_d4_d4_ff,
    border: 0x3e_3e_42_ff,
    accent: 0x0e_63_9c_ff,
    accent_fg: 0xff_ff_ff_ff,
    banner: 0x1e_1e_1e_ff,
    muted_fg: 0x9a_a0_a8_ff,
    danger: 0xf4_87_71_ff,
    danger_fg: 0x00_00_00_ff,
    success: 0x89_d1_85_ff,
    success_fg: 0x00_00_00_ff,
    warning: 0xcc_a7_00_ff,
    warning_fg: 0x00_00_00_ff,
    info: 0x37_94_ff_ff,
    info_fg: 0x00_00_00_ff,
    link: 0x37_94_ff_ff,
};

/// VSCode Light inspired: dimmed workbench `#ececec`, soft-white `#f8f8f8`
/// cards (pure white tires the eye), blue accents.
const PALETTE_LIGHT: Palette = Palette {
    bg: 0xec_ec_ec_ff,
    surface: 0xf8_f8_f8_ff,
    fg: 0x33_33_33_ff,
    border: 0xd0_d0_d0_ff,
    accent: 0x00_67_b8_ff,
    accent_fg: 0xff_ff_ff_ff,
    banner: 0xda_da_da_ff,
    muted_fg: 0x65_6b_73_ff,
    danger: 0xe5_14_00_ff,
    danger_fg: 0xff_ff_ff_ff,
    success: 0x22_86_3a_ff,
    success_fg: 0xff_ff_ff_ff,
    warning: 0xbf_88_03_ff,
    warning_fg: 0x00_00_00_ff,
    info: 0x00_5a_9e_ff,
    info_fg: 0xff_ff_ff_ff,
    link: 0x00_6a_b1_ff,
};

/// Zed Gruvbox Dark Hard: near-black warm bg, bg0 surfaces, bright accents.
const PALETTE_GRUVBOX_DARK: Palette = Palette {
    bg: 0x1d_20_21_ff,
    surface: 0x28_28_28_ff,
    fg: 0xeb_db_b2_ff,
    border: 0x66_5c_54_ff,
    accent: 0xfe_80_19_ff,
    accent_fg: 0x00_00_00_ff,
    banner: 0x1d_20_21_ff,
    muted_fg: 0xa8_99_84_ff,
    danger: 0xfb_49_34_ff,
    danger_fg: 0x00_00_00_ff,
    success: 0xb8_bb_26_ff,
    success_fg: 0x00_00_00_ff,
    warning: 0xfa_bd_2f_ff,
    warning_fg: 0x00_00_00_ff,
    info: 0x83_a5_98_ff,
    info_fg: 0x00_00_00_ff,
    link: 0x83_a5_98_ff,
};

/// Gruvbox Light Soft: unmistakable cream bg, faded accents.
const PALETTE_GRUVBOX_LIGHT: Palette = Palette {
    bg: 0xf2_e5_bc_ff,
    surface: 0xeb_db_b2_ff,
    fg: 0x3c_38_36_ff,
    border: 0xbd_ae_93_ff,
    accent: 0xaf_3a_03_ff,
    accent_fg: 0xff_ff_ff_ff,
    banner: 0xe3_d3_a3_ff,
    muted_fg: 0x6b_5f_54_ff,
    danger: 0x9d_00_06_ff,
    danger_fg: 0xff_ff_ff_ff,
    success: 0x79_74_0e_ff,
    success_fg: 0xff_ff_ff_ff,
    warning: 0xb5_76_14_ff,
    warning_fg: 0x00_00_00_ff,
    info: 0x07_66_78_ff,
    info_fg: 0xff_ff_ff_ff,
    link: 0x07_66_78_ff,
};

/// Coffee dark: deep-roast brown surfaces, caramel accent.
const PALETTE_COFFEE_DARK: Palette = Palette {
    bg: 0x1e_13_0b_ff,
    surface: 0x33_26_1a_ff,
    fg: 0xec_e0_d1_ff,
    border: 0x5a_4a_38_ff,
    accent: 0xc0_8a_4b_ff,
    accent_fg: 0x00_00_00_ff,
    banner: 0x1e_13_0b_ff,
    muted_fg: 0xa8_98_80_ff,
    danger: 0xe8_6a_5e_ff,
    danger_fg: 0x00_00_00_ff,
    success: 0x9d_b3_5c_ff,
    success_fg: 0x00_00_00_ff,
    warning: 0xd9_a4_41_ff,
    warning_fg: 0x00_00_00_ff,
    info: 0x6f_a8_a0_ff,
    info_fg: 0x00_00_00_ff,
    link: 0xd9_a4_41_ff,
};

/// Coffee light: kraft-paper tan bg, latte surfaces, roast accent.
const PALETTE_COFFEE_LIGHT: Palette = Palette {
    bg: 0xe9_d2_ac_ff,
    surface: 0xf6_ea_d6_ff,
    fg: 0x2b_21_18_ff,
    border: 0xc9_b8_a3_ff,
    accent: 0x7c_4f_22_ff,
    accent_fg: 0xff_ff_ff_ff,
    banner: 0xdc_c1_98_ff,
    muted_fg: 0x67_54_3d_ff,
    danger: 0xa8_32_26_ff,
    danger_fg: 0xff_ff_ff_ff,
    success: 0x5d_7d_2a_ff,
    success_fg: 0xff_ff_ff_ff,
    warning: 0x9a_6b_14_ff,
    warning_fg: 0xff_ff_ff_ff,
    info: 0x2f_6f_6a_ff,
    info_fg: 0xff_ff_ff_ff,
    link: 0x5f_40_18_ff,
};

fn palette_for(mode: ThemeModeExt) -> Palette {
    match mode {
        ThemeModeExt::Dark => PALETTE_DARK,
        ThemeModeExt::Light => PALETTE_LIGHT,
        ThemeModeExt::GruvboxDark => PALETTE_GRUVBOX_DARK,
        ThemeModeExt::GruvboxLight => PALETTE_GRUVBOX_LIGHT,
        ThemeModeExt::CoffeeDark => PALETTE_COFFEE_DARK,
        ThemeModeExt::CoffeeLight => PALETTE_COFFEE_LIGHT,
    }
}

/// Convert u32 ARGB to Hsla.
fn u32_to_hsla(c: u32) -> Hsla {
    let r = ((c >> 24) & 0xFF) as f32 / 255.0;
    let g = ((c >> 16) & 0xFF) as f32 / 255.0;
    let b = ((c >> 8) & 0xFF) as f32 / 255.0;
    let a = (c & 0xFF) as f32 / 255.0;
    // Simple RGB to HSL conversion
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let (h, s) = if max == min {
        (0.0, 0.0)
    } else {
        let d = max - min;
        let s = if l > 0.5 {
            d / (2.0 - max - min)
        } else {
            d / (max + min)
        };
        let h = if max == r {
            (g - b) / d + (if g < b { 6.0 } else { 0.0 })
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        (h / 6.0, s)
    };
    Hsla { h, s, l, a }
}

/// Hover/active derivates: lighten steps on dark bases, darken steps on
/// light ones (matches the kit's own hover conventions).
fn shift(c: Hsla, light_base: bool, amt: f32) -> Hsla {
    if light_base {
        c.darken(amt)
    } else {
        c.lighten(amt)
    }
}

/// Apply the theme mode to kit widgets, then derive the FULL component
/// token surface from the palette. `Theme::change` resets every token to
/// kit defaults, so anything left untouched (buttons, inputs, progress,
/// sliders, banner nav buttons, …) would render stock colors on top of our
/// surfaces — the old "glitch layer" look. Every theme switch (Settings,
/// Ctrl-T cycle, boot) must call this — setting `Store::theme_mode` alone
/// leaves kit globals stale (the old invisible-text-in-light-mode bug).
/// Call on boot (after `init`) and on every toggle.
pub fn apply_theme(theme_mode: &str, high_contrast: bool, cx: &mut App) {
    let mode = ThemeModeExt::from_str(theme_mode);
    let kit_mode = mode.to_kit_mode();

    Theme::change(kit_mode, None, cx);

    let colors = &mut Theme::global_mut(cx).colors;
    let mut pal = palette_for(mode);
    if high_contrast {
        maximize_contrast(&mut pal, mode.is_light());
    }
    let light = mode.is_light();

    let bg = u32_to_hsla(pal.bg);
    let surface = u32_to_hsla(pal.surface);
    let fg = u32_to_hsla(pal.fg);
    let border = u32_to_hsla(pal.border);
    let accent = u32_to_hsla(pal.accent);
    let accent_fg = u32_to_hsla(pal.accent_fg);
    let accent_hover = shift(accent, light, 0.08);
    let accent_active = shift(accent, light, 0.16);
    let surface_hover = shift(surface, light, 0.06);
    let surface_active = shift(surface, light, 0.12);

    // Base chrome.
    colors.background = bg;
    colors.foreground = fg;
    colors.border = border;
    colors.ring = accent;
    colors.caret = accent;
    colors.window_border = border;
    colors.title_bar = bg;
    colors.title_bar_border = border;
    colors.status_bar = surface;
    colors.status_bar_border = border;

    // Accent + primary family (active nav button, primary buttons, rings).
    colors.primary = accent;
    colors.primary_foreground = accent_fg;
    colors.primary_hover = accent_hover;
    colors.primary_active = accent_active;
    colors.accent = accent;
    colors.accent_foreground = accent_fg;
    colors.link = u32_to_hsla(pal.link);
    colors.link_hover = shift(u32_to_hsla(pal.link), light, 0.08);
    colors.link_active = shift(u32_to_hsla(pal.link), light, 0.16);
    colors.drag_border = accent;
    colors.drop_target = accent.opacity(0.25);
    colors.selection = accent.opacity(0.30);

    // Default + secondary buttons (banner nav buttons live here).
    colors.secondary = surface;
    colors.secondary_foreground = fg;
    colors.secondary_hover = surface_hover;
    colors.secondary_active = surface_active;
    colors.button = surface;
    colors.button_foreground = fg;
    colors.button_hover = surface_hover;
    colors.button_active = surface_active;
    colors.button_primary = accent;
    colors.button_primary_foreground = accent_fg;
    colors.button_primary_hover = accent_hover;
    colors.button_primary_active = accent_active;
    colors.button_secondary = surface;
    colors.button_secondary_foreground = fg;
    colors.button_secondary_hover = surface_hover;
    colors.button_secondary_active = surface_active;

    // Muted surfaces + popover + lists + tables. Muted text is dimmer
    // than fg (hints, captions) but stays AA-readable — enforced below.
    colors.muted = surface;
    colors.muted_foreground = u32_to_hsla(pal.muted_fg);
    colors.popover = surface;
    colors.popover_foreground = fg;
    colors.list = surface;
    colors.list_hover = surface_hover;
    colors.list_active = accent.opacity(0.25);
    colors.list_active_border = accent;
    colors.list_head = surface;
    colors.list_even = surface;
    colors.table = surface;
    colors.table_hover = surface_hover;
    colors.table_active = accent.opacity(0.25);
    colors.table_active_border = accent;
    colors.table_head = surface;
    colors.table_head_foreground = fg;
    colors.table_foot = surface;
    colors.table_foot_foreground = fg;
    colors.table_even = surface;
    colors.table_row_border = border;

    // Inputs, sliders, progress, scrollbars, tabs, sidebar.
    colors.input = surface;
    colors.slider_bar = border;
    colors.slider_thumb = accent;
    colors.progress_bar = accent;
    colors.scrollbar = bg;
    colors.scrollbar_thumb = border;
    colors.scrollbar_thumb_hover = fg;
    colors.tab_bar = bg;
    colors.tab_bar_segmented = surface;
    colors.tab = bg;
    colors.tab_foreground = fg;
    colors.tab_active = surface;
    colors.tab_active_foreground = fg;
    colors.sidebar = bg;
    colors.sidebar_foreground = fg;
    colors.sidebar_border = border;
    colors.sidebar_accent = surface;
    colors.sidebar_accent_foreground = fg;
    colors.sidebar_primary = accent;
    colors.sidebar_primary_foreground = accent_fg;
    colors.switch = border;
    colors.switch_thumb = fg;
    colors.skeleton = border;
    colors.tiles = surface;
    colors.group_box = surface;
    colors.group_box_foreground = fg;
    colors.accordion = fg;
    colors.description_list_label = fg;
    colors.description_list_label_foreground = fg;
    colors.overlay = bg.opacity(0.70);

    // Semantic families (destructive buttons, badges, meters).
    let sem = [
        (pal.danger, pal.danger_fg),
        (pal.success, pal.success_fg),
        (pal.warning, pal.warning_fg),
        (pal.info, pal.info_fg),
    ];
    let (danger, danger_fg) = (u32_to_hsla(sem[0].0), u32_to_hsla(sem[0].1));
    let (success, success_fg) = (u32_to_hsla(sem[1].0), u32_to_hsla(sem[1].1));
    let (warning, warning_fg) = (u32_to_hsla(sem[2].0), u32_to_hsla(sem[2].1));
    let (info, info_fg) = (u32_to_hsla(sem[3].0), u32_to_hsla(sem[3].1));
    colors.danger = danger;
    colors.danger_foreground = danger_fg;
    colors.danger_hover = shift(danger, light, 0.08);
    colors.danger_active = shift(danger, light, 0.16);
    colors.button_danger = danger;
    colors.button_danger_foreground = danger_fg;
    colors.button_danger_hover = shift(danger, light, 0.08);
    colors.button_danger_active = shift(danger, light, 0.16);
    colors.success = success;
    colors.success_foreground = success_fg;
    colors.success_hover = shift(success, light, 0.08);
    colors.success_active = shift(success, light, 0.16);
    colors.button_success = success;
    colors.button_success_foreground = success_fg;
    colors.button_success_hover = shift(success, light, 0.08);
    colors.button_success_active = shift(success, light, 0.16);
    colors.warning = warning;
    colors.warning_foreground = warning_fg;
    colors.warning_hover = shift(warning, light, 0.08);
    colors.warning_active = shift(warning, light, 0.16);
    colors.button_warning = warning;
    colors.button_warning_foreground = warning_fg;
    colors.button_warning_hover = shift(warning, light, 0.08);
    colors.button_warning_active = shift(warning, light, 0.16);
    colors.info = info;
    colors.info_foreground = info_fg;
    colors.info_hover = shift(info, light, 0.08);
    colors.info_active = shift(info, light, 0.16);
    colors.button_info = info;
    colors.button_info_foreground = info_fg;
    colors.button_info_hover = shift(info, light, 0.08);
    colors.button_info_active = shift(info, light, 0.16);

    // Shape + text defaults: calm 6/10px radii, 14px body text.
    // Scrollbars stay visible whenever content overflows (auto-fade hides
    // the only affordance that content continues below the fold).
    let theme = Theme::global_mut(cx);
    theme.radius = px(6.0);
    theme.radius_lg = px(10.0);
    theme.font_size = px(14.0);
    Theme::set_scrollbar_mode(gpui_kit::base::ScrollbarMode::Always, cx);
}

/// High-contrast overlay: pure black/white text + matching borders on the
/// base background, and guaranteed-readable label colors on every filled
/// hue. The base `bg`/`surface`/accent/semantic hues are kept so the theme
/// stays recognizable. Selection/overlay/scrim translucency is functional
/// (keeps selected text readable) and stays as `apply_theme` sets it.
fn maximize_contrast(pal: &mut Palette, light_base: bool) {
    pal.fg = if light_base {
        0x00_00_00_ff
    } else {
        0xff_ff_ff_ff
    };
    pal.border = pal.fg;
    pal.muted_fg = pal.fg;
    // Pick each filled-hue label (black or white) with the better ratio.
    let best_on = |c: u32| {
        if contrast_ratio(0x00_00_00_ff, c) >= contrast_ratio(0xff_ff_ff_ff, c) {
            0x00_00_00_ff
        } else {
            0xff_ff_ff_ff
        }
    };
    pal.accent_fg = best_on(pal.accent);
    pal.danger_fg = best_on(pal.danger);
    pal.success_fg = best_on(pal.success);
    pal.warning_fg = best_on(pal.warning);
    pal.info_fg = best_on(pal.info);
}

/// Relative luminance of an sRGB color (WCAG 2.1 definition). Pure.
fn luminance(c: u32) -> f32 {
    let chan = |v: u32| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let r = chan((c >> 24) & 0xFF);
    let g = chan((c >> 16) & 0xFF);
    let b = chan((c >> 8) & 0xFF);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// WCAG contrast ratio between two palette colors (1.0–21.0). Pure.
pub fn contrast_ratio(a: u32, b: u32) -> f32 {
    let (hi, lo) = {
        let (la, lb) = (luminance(a), luminance(b));
        if la >= lb {
            (la, lb)
        } else {
            (lb, la)
        }
    };
    (hi + 0.05) / (lo + 0.05)
}

/// Effective palette for a mode + overlay (what `apply_theme` installs).
/// Exposed for tests and for views that need contrast-safe hardcoded colors.
pub fn effective_palette(theme_mode: &str, high_contrast: bool) -> Palette {
    let mode = ThemeModeExt::from_str(theme_mode);
    let mut pal = palette_for(mode);
    if high_contrast {
        maximize_contrast(&mut pal, mode.is_light());
    }
    pal
}

/// Manual-surface helpers. These MUST be driven by the active `theme_mode`
/// (+ overlay), never by the legacy `dark_mode` bool — that staleness caused
/// the old invisible-text-in-light-mode bug (kit globals flipped while these
/// didn't, or vice versa). Every view reads both from the Store snapshot.
pub fn fg(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).fg
}

pub fn surface(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).surface
}

/// Nav banner shade (darker bar on light themes, equals `bg` on darks).
pub fn banner(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).banner
}

/// Secondary text color (hints, paths, captions). AA-checked per theme.
pub fn muted(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).muted_fg
}

/// 1px edge color for cards and tiles (pairs with `.border_1()`).
pub fn border(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).border
}

/// Semantic fills (buttons, badges, meters). Labels go on `*_fg` twins.
pub fn danger(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).danger
}
pub fn success(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).success
}
pub fn warning(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).warning
}
pub fn info(theme_mode: &str, high_contrast: bool) -> u32 {
    effective_palette(theme_mode, high_contrast).info
}

/// Type scale: regular-weight body text by default, semibold reserved for
/// headings and key values. These replace hand-rolled
/// `.font_weight(SEMIBOLD)`-on-everything (the old shouty look).
/// `theme`/`hc` come from the Store snapshot (see other helpers).
pub fn title(s: impl Into<SharedString>, theme: &str, hc: bool) -> Div {
    div()
        .text_size(px(18.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgba(fg(theme, hc)))
        .child(s.into())
}

/// Quiet field label ("Output folder"): 13px muted.
pub fn label(s: impl Into<SharedString>, theme: &str, hc: bool) -> Div {
    div()
        .text_size(px(13.0))
        .text_color(rgba(muted(theme, hc)))
        .child(s.into())
}

/// Field value (the path itself): 14px regular, full contrast.
pub fn value(s: impl Into<SharedString>, theme: &str, hc: bool) -> Div {
    div()
        .text_size(px(14.0))
        .text_color(rgba(fg(theme, hc)))
        .child(s.into())
}

/// Card container: surface fill, 1px border, 6px radius, 16px padding.
/// Tiles, file cards, and settings groups share this so edges never look
/// accidental (the old flat-`bg(surface)` look).
pub fn card(theme: &str, hc: bool) -> Div {
    div()
        .bg(rgba(surface(theme, hc)))
        .border_1()
        .border_color(rgba(effective_palette(theme, hc).border))
        .rounded(px(6.0))
        .p(px(SPACE_LG))
}

/// Helper/hint line: 12px muted. Long explanations live here, not bold.
pub fn hint(s: impl Into<SharedString>, theme: &str, hc: bool) -> Div {
    div()
        .text_size(px(12.0))
        .text_color(rgba(muted(theme, hc)))
        .child(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_helpers_split() {
        assert_ne!(fg("dark", false), fg("light", false));
        assert_ne!(surface("dark", false), surface("light", false));
        assert_ne!(banner("dark", false), banner("gruvbox_dark", false));
        // Overlay never changes background hues, only the text side.
        let base = effective_palette("gruvbox_dark", false);
        let hc = effective_palette("gruvbox_dark", true);
        assert_eq!(base.bg, hc.bg);
        assert_eq!(base.banner, hc.banner);
        assert_ne!(base.fg, hc.fg);
    }

    #[test]
    fn theme_mode_ext_roundtrip() {
        assert_eq!(
            ThemeModeExt::from_str("dark").to_kit_mode(),
            ThemeMode::Dark
        );
        assert_eq!(
            ThemeModeExt::from_str("light").to_kit_mode(),
            ThemeMode::Light
        );
        assert_eq!(
            ThemeModeExt::from_str("gruvbox_dark").to_kit_mode(),
            ThemeMode::Dark
        );
        assert_eq!(
            ThemeModeExt::from_str("gruvbox_light").to_kit_mode(),
            ThemeMode::Light
        );
        assert_eq!(
            ThemeModeExt::from_str("coffee_dark").to_kit_mode(),
            ThemeMode::Dark
        );
        assert_eq!(
            ThemeModeExt::from_str("coffee_light").to_kit_mode(),
            ThemeMode::Light
        );
        // Unknown + legacy values fall back to dark, never panic.
        assert_eq!(
            ThemeModeExt::from_str("unknown").to_kit_mode(),
            ThemeMode::Dark
        );
        assert_eq!(
            ThemeModeExt::from_str("high_contrast").to_kit_mode(),
            ThemeMode::Dark
        );
    }

    #[test]
    fn theme_cycle_covers_all_bases_exactly_once() {
        assert_eq!(THEME_ORDER.len(), 6);
        let mut seen = std::collections::HashSet::new();
        let mut cur = "dark";
        for _ in 0..THEME_ORDER.len() {
            assert!(seen.insert(cur), "cycle repeats {cur}");
            cur = next_theme(cur);
        }
        assert_eq!(cur, "dark");
        assert_eq!(next_theme("bogus"), "light");
    }

    /// Every base theme keeps body text at WCAG AA (4.5:1), every filled
    /// button/semantic label readable, and links legible on the background —
    /// no theme can ship invisible text again.
    #[test]
    fn every_base_theme_meets_aa_contrast() {
        for mode in THEME_ORDER {
            let pal = effective_palette(mode, false);
            let body = contrast_ratio(pal.fg, pal.bg);
            assert!(body >= 4.5, "{mode}: body text ratio {body:.2} < 4.5");
            let pairs = [
                ("accent", pal.accent_fg, pal.accent),
                ("danger", pal.danger_fg, pal.danger),
                ("success", pal.success_fg, pal.success),
                ("warning", pal.warning_fg, pal.warning),
                ("info", pal.info_fg, pal.info),
            ];
            for (name, label, fill) in pairs {
                let r = contrast_ratio(label, fill);
                assert!(r >= 4.5, "{mode}: {name} label ratio {r:.2} < 4.5");
            }
            let link = contrast_ratio(pal.link, pal.bg);
            assert!(link >= 4.5, "{mode}: link ratio {link:.2} < 4.5");
            // Ghost nav buttons sit transparent on the banner: body text
            // must read there too. Muted hint text stays AA as well.
            let on_banner = contrast_ratio(pal.fg, pal.banner);
            assert!(
                on_banner >= 4.5,
                "{mode}: fg-on-banner ratio {on_banner:.2} < 4.5"
            );
            let hint = contrast_ratio(pal.muted_fg, pal.bg);
            assert!(hint >= 4.5, "{mode}: muted ratio {hint:.2} < 4.5");
        }
    }

    /// The overlay keeps the base background hue but maxes out text to
    /// pure black/white with AAA (7:1+) body contrast on every base, and
    /// every filled-hue label stays AA.
    #[test]
    fn high_contrast_overlay_maxes_text_on_every_base() {
        for mode in THEME_ORDER {
            let base = effective_palette(mode, false);
            let hc = effective_palette(mode, true);
            assert_eq!(hc.bg, base.bg, "{mode}: overlay must keep base bg");
            assert_eq!(
                hc.banner, base.banner,
                "{mode}: overlay must keep banner hue"
            );
            assert_eq!(
                hc.surface, base.surface,
                "{mode}: overlay must keep surface"
            );
            assert_eq!(hc.accent, base.accent, "{mode}: overlay must keep accent");
            assert_eq!(hc.danger, base.danger, "{mode}: overlay must keep danger");
            let light = ThemeModeExt::from_str(mode).is_light();
            assert_eq!(
                hc.fg,
                if light { 0x00_00_00_ff } else { 0xff_ff_ff_ff },
                "{mode}: overlay fg must be pure"
            );
            assert_eq!(hc.border, hc.fg, "{mode}: overlay border must match fg");
            let body = contrast_ratio(hc.fg, hc.bg);
            assert!(body >= 7.0, "{mode}: overlay body ratio {body:.2} < 7.0");
            for (name, label, fill) in [
                ("accent", hc.accent_fg, hc.accent),
                ("danger", hc.danger_fg, hc.danger),
                ("success", hc.success_fg, hc.success),
                ("warning", hc.warning_fg, hc.warning),
                ("info", hc.info_fg, hc.info),
            ] {
                assert!(
                    label == 0x00_00_00_ff || label == 0xff_ff_ff_ff,
                    "{mode}: overlay {name} label must be pure"
                );
                let r = contrast_ratio(label, fill);
                assert!(r >= 4.5, "{mode}: overlay {name} ratio {r:.2} < 4.5");
            }
        }
    }

    /// Dark bases stay dark-family, lights stay light-family, and no two
    /// bases share a background (each theme must read as its own theme).
    /// Banners match bg on darks and step darker on lights.
    #[test]
    fn bases_are_mutually_distinct() {
        let mut bgs = std::collections::HashSet::new();
        for mode in THEME_ORDER {
            let pal = effective_palette(mode, false);
            assert!(
                bgs.insert(pal.bg),
                "{mode}: background hue collides with another base"
            );
            let lum = luminance(pal.bg);
            assert_eq!(
                lum > 0.18,
                ThemeModeExt::from_str(mode).is_light(),
                "{mode}: lightness does not match its kit mode"
            );
            if ThemeModeExt::from_str(mode).is_light() {
                assert_ne!(
                    pal.banner, pal.bg,
                    "{mode}: light banner must step darker than bg"
                );
                assert!(
                    luminance(pal.banner) < lum,
                    "{mode}: light banner must be darker than bg"
                );
            } else {
                assert_eq!(pal.banner, pal.bg, "{mode}: dark banner must equal bg");
            }
        }
    }
}
