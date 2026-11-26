//! Colours beyond the kit's semantic tokens: status colours, links, and the monospace log ink.
//!
//! Everything else comes from `rune_kit::Kit::neutral`, so the app follows the system light/dark
//! preference and the kit's own tokens with no palette of its own.

use rune::{oklch, Color};
use rune_kit::{ButtonMetrics, BySize, FieldMetrics, Kit};

use crate::model::{ProjectState, RunStatus, StepStatus};

/// Per-status inks and surfaces, resolved from the kit so they track light/dark and the kit's
/// own contrast floors.
#[derive(Clone, Copy)]
pub struct Status {
    pub success: Color,
    pub failure: Color,
    pub error: Color,
    pub running: Color,
    pub cancelled: Color,
    pub pending: Color,
    pub skipped: Color,
}

/// Theme mode chosen by the user for this window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
    Oled,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 4] = [
        ThemeMode::System,
        ThemeMode::Light,
        ThemeMode::Dark,
        ThemeMode::Oled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
            ThemeMode::Oled => "oled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Some(ThemeMode::System),
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            "oled" => Some(ThemeMode::Oled),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            ThemeMode::System => "System",
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::Oled => "OLED",
        }
    }
}

/// Crisp 1pt divider used for major section boundaries.
pub const SECTION_BORDER: f32 = 1.0;

/// The kit for the current appearance: stepped surfaces and borders over the neutral palette.
pub fn kit(dark: bool) -> Kit {
    let mut k = Kit::neutral(dark);
    if dark {
        apply_dark(&mut k);
    } else {
        apply_light(&mut k);
    }
    k
}

/// The kit for the resolved window theme mode and responsive density (`compact` => ≥ 44 pt touch targets).
pub fn kit_mode(mode: ThemeMode, system_dark: bool, compact: bool) -> Kit {
    let (dark, oled) = match mode {
        ThemeMode::System => (system_dark, false),
        ThemeMode::Light => (false, false),
        ThemeMode::Dark => (true, false),
        ThemeMode::Oled => (true, true),
    };
    let mut k = kit_for(dark, compact);
    if oled {
        apply_oled(&mut k);
    }
    k
}

fn srgb_hex(rgb: u32) -> Color {
    let ch = |u: u32| {
        let c = (u & 0xff) as f32 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Color {
        r: ch(rgb >> 16),
        g: ch(rgb >> 8),
        b: ch(rgb),
        a: 1.0,
    }
}

fn sync_elevation_borders(k: &mut Kit) {
    for list in [
        &mut k.elevation.raised,
        &mut k.elevation.floating,
        &mut k.elevation.overlay,
        &mut k.elevation.button_neutral,
        &mut k.elevation.button_contrast,
        &mut k.elevation.elements,
        &mut k.elevation.md,
        &mut k.elevation.knob,
    ] {
        for sh in list.iter_mut() {
            if sh.spread > 0.0 {
                sh.color = k.c.border_base;
            } else {
                sh.color = Color::BLACK.alpha(sh.color.a);
            }
        }
    }
}

fn sync_neutral_roles(k: &mut Kit, inv_ink: Color) {
    k.c.bg_inverse = k.c.text_base;
    k.c.bg_contrast = k.c.text_base;
    k.c.bg_icon_button_contrast = k.c.text_base;
    k.c.surface_float = k.c.text_base;

    k.c.submit_fg = inv_ink;
    k.c.submit_icon = inv_ink;
    k.c.text_inverse = inv_ink;
    k.c.text_contrast = inv_ink;
    k.c.icon_inverse = inv_ink;
    k.c.icon_contrast = inv_ink;
    k.c.text_invert_strong = inv_ink;
    k.c.std_icon_invert = inv_ink;

    k.c.text_strong = k.c.text_base;
    k.c.text_weak = k.c.text_muted;
    k.c.icon_base = k.c.text_base;
    k.c.icon_muted = k.c.text_muted;
    k.c.icon_faint = k.c.text_faint;
    k.c.icon_weaker = k.c.text_muted;

    k.c.std_text = k.c.text_base;
    k.c.std_icon = k.c.text_muted;
    k.c.std_icon_strong = k.c.text_base;

    k.c.knob = Color::WHITE;
    k.c.knob_on = Color::WHITE;
}

fn apply_dark(k: &mut Kit) {
    // Crisp pure neutral achromatic grays (R == G == B, zero blue tint).
    // `bg_base` is raised above `layer_01` so `SegmentedControl` active pills stand out cleanly.
    k.c.bg_deep = srgb_hex(0x0a0a0a);
    k.c.page = srgb_hex(0x0f0f0f);
    k.c.layer_01 = srgb_hex(0x141414);
    k.c.layer_02 = srgb_hex(0x1a1a1a);
    k.c.bg_base = srgb_hex(0x242424);
    k.c.layer_03 = srgb_hex(0x242424);
    k.c.layer_04 = srgb_hex(0x2e2e2e);

    k.c.surface_base = k.c.layer_02;
    k.c.surface_weak = k.c.layer_02;
    k.c.surface_disabled = k.c.layer_02;
    k.c.surface_raised = k.c.layer_02;
    k.c.surface_raised_base = k.c.layer_02;
    k.c.bg_button_neutral = k.c.layer_02;

    k.c.hover = srgb_hex(0x202020);
    k.c.surface_hover = srgb_hex(0x222222);
    k.c.surface_raised_hover = srgb_hex(0x262626);
    k.c.pressed = srgb_hex(0x2c2c2c);

    k.c.border_muted = srgb_hex(0x262626);
    k.c.border_weak = srgb_hex(0x262626);
    k.c.border_disabled = srgb_hex(0x1f1f1f);
    k.c.border_base = srgb_hex(0x333333);
    k.c.std_border = srgb_hex(0x333333);
    k.c.border_hover = srgb_hex(0x474747);
    k.c.border_strong = srgb_hex(0x454545);

    k.c.text_base = srgb_hex(0xededed);
    k.c.text_muted = srgb_hex(0xa1a1a1);
    k.c.text_faint = srgb_hex(0x737373);

    sync_neutral_roles(k, k.c.page);
    sync_elevation_borders(k);
}

fn apply_light(k: &mut Kit) {
    // Crisp neutral grays with zero blue slate cast.
    // `bg_base` is pure white inside `#efefef` `layer_01` tracks for crisp `SegmentedControl` pills.
    k.c.bg_deep = srgb_hex(0xffffff);
    k.c.page = srgb_hex(0xfafafa);
    k.c.layer_01 = srgb_hex(0xefefef);
    k.c.bg_base = srgb_hex(0xffffff);
    k.c.layer_02 = srgb_hex(0xffffff);
    k.c.layer_03 = srgb_hex(0xe8e8e8);
    k.c.layer_04 = srgb_hex(0xffffff);

    k.c.surface_base = k.c.layer_02;
    k.c.surface_weak = k.c.layer_01;
    k.c.surface_disabled = k.c.layer_01;
    k.c.surface_raised = k.c.layer_02;
    k.c.surface_raised_base = k.c.layer_02;
    k.c.bg_button_neutral = k.c.layer_02;

    k.c.hover = srgb_hex(0xeaeaea);
    k.c.surface_hover = srgb_hex(0xf0f0f0);
    k.c.surface_raised_hover = srgb_hex(0xf7f7f7);
    k.c.pressed = srgb_hex(0xe0e0e0);

    k.c.border_muted = srgb_hex(0xe5e5e5);
    k.c.border_weak = srgb_hex(0xe5e5e5);
    k.c.border_disabled = srgb_hex(0xeeeeee);
    k.c.border_base = srgb_hex(0xd4d4d4);
    k.c.std_border = srgb_hex(0xd4d4d4);
    k.c.border_hover = srgb_hex(0xa3a3a3);
    k.c.border_strong = srgb_hex(0x999999);

    k.c.text_base = srgb_hex(0x171717);
    k.c.text_muted = srgb_hex(0x525252);
    k.c.text_faint = srgb_hex(0x737373);

    sync_neutral_roles(k, Color::WHITE);
    sync_elevation_borders(k);
}

fn apply_oled(k: &mut Kit) {
    // Vercel Geist Dark pure black: every primary surface is #000000, defined by crisp borders.
    // `bg_base` is subtly raised (`#1a1a1a`) so `SegmentedControl` active pills stand out cleanly
    // against pure-black `layer_01` tracks while all major surfaces remain `#000000`.
    k.c.page = Color::BLACK;
    k.c.bg_deep = Color::BLACK;
    k.c.layer_01 = Color::BLACK;
    k.c.layer_02 = Color::BLACK;
    k.c.bg_base = srgb_hex(0x1a1a1a);

    k.c.surface_base = Color::BLACK;
    k.c.surface_weak = Color::BLACK;
    k.c.surface_disabled = Color::BLACK;
    k.c.surface_raised = Color::BLACK;
    k.c.surface_raised_base = Color::BLACK;
    k.c.bg_button_neutral = Color::BLACK;

    k.c.layer_03 = srgb_hex(0x141414);
    k.c.layer_04 = srgb_hex(0x1c1c1c);

    k.c.hover = srgb_hex(0x111111);
    k.c.surface_hover = srgb_hex(0x121212);
    k.c.surface_raised_hover = srgb_hex(0x161616);
    k.c.pressed = srgb_hex(0x1c1c1c);

    k.c.border_muted = srgb_hex(0x1f1f1f);
    k.c.border_weak = srgb_hex(0x1f1f1f);
    k.c.border_disabled = srgb_hex(0x171717);
    k.c.border_base = srgb_hex(0x2e2e2e);
    k.c.std_border = srgb_hex(0x2e2e2e);
    k.c.border_hover = srgb_hex(0x454545);
    k.c.border_strong = srgb_hex(0x454545);

    k.c.text_base = srgb_hex(0xededed);
    k.c.text_muted = srgb_hex(0xa1a1a1);
    k.c.text_faint = srgb_hex(0x6e6e6e);

    sync_neutral_roles(k, Color::BLACK);
    sync_elevation_borders(k);
}

/// The kit adjusted for compact / touch viewports: enlarged tap heights (≥ 44 pt) on buttons
/// and text inputs while leaving desktop metrics unchanged.
pub fn kit_for(dark: bool, compact: bool) -> Kit {
    let mut k = kit(dark);
    if compact {
        let m = |height, padding_x, radius| ButtonMetrics {
            height,
            padding_x,
            radius,
            icon: 16.0,
        };
        k.metrics.button = Some(BySize {
            mini: m(24.0, 8.0, k.radius.sm),
            small: m(44.0, 12.0, k.radius.md),
            regular: m(44.0, 14.0, k.radius.md),
            large: m(44.0, 16.0, k.radius.lg),
            extra_large: m(48.0, 18.0, k.radius.xl),
        });
        k.metrics.field = Some(FieldMetrics {
            height: 44.0,
            large_height: 44.0,
            ..FieldMetrics::kit(&k)
        });
    }
    k
}

pub fn status(k: &Kit) -> Status {
    Status {
        success: if k.dark {
            oklch(0.76, 0.15, 152.0)
        } else {
            oklch(0.53, 0.14, 152.0)
        },
        failure: if k.dark {
            oklch(0.71, 0.17, 25.0)
        } else {
            oklch(0.53, 0.19, 27.0)
        },
        error: if k.dark {
            oklch(0.74, 0.15, 60.0)
        } else {
            oklch(0.52, 0.15, 55.0)
        },
        running: if k.dark {
            oklch(0.74, 0.14, 250.0)
        } else {
            oklch(0.53, 0.16, 252.0)
        },
        cancelled: if k.dark {
            oklch(0.73, 0.09, 72.0)
        } else {
            oklch(0.50, 0.10, 68.0)
        },
        pending: k.c.icon_faint,
        skipped: k.c.icon_faint,
    }
}

impl Status {
    pub fn for_project(self, state: ProjectState) -> Color {
        match state {
            ProjectState::Running => self.running,
            ProjectState::Success => self.success,
            ProjectState::Failure => self.failure,
            ProjectState::Error => self.error,
            ProjectState::Cancelled => self.cancelled,
            ProjectState::Idle => self.pending,
        }
    }

    pub fn for_run(self, state: RunStatus) -> Color {
        match state {
            RunStatus::Running => self.running,
            RunStatus::Success => self.success,
            RunStatus::Failure => self.failure,
            RunStatus::Error => self.error,
            RunStatus::Cancelled => self.cancelled,
            RunStatus::Other => self.pending,
        }
    }

    pub fn for_step(self, state: StepStatus) -> Color {
        match state {
            StepStatus::Running => self.running,
            StepStatus::Success => self.success,
            StepStatus::Failure | StepStatus::Timeout => self.failure,
            StepStatus::Cancelled => self.cancelled,
            StepStatus::Skipped => self.skipped,
            StepStatus::Pending | StepStatus::Other => self.pending,
        }
    }

    /// A status's tint for a pill: its ink at low alpha over the card.
    pub fn tint(self, ink: Color) -> Color {
        ink.alpha(0.14)
    }
}

/// A one-pixel hairline that stays one device pixel at any scale.
pub fn hair(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        1.0 / scale
    } else {
        1.0
    }
}

/// Minimum row count for `TextArea::auto_grow` so touch targets stay ≥ 44 pt on compact layouts.
pub fn area_min_rows(k: &Kit) -> usize {
    if k.metrics.field.is_some_and(|f| f.height >= 44.0) {
        3
    } else {
        2
    }
}
