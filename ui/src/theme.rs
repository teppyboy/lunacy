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
    pub pending: Color,
    pub skipped: Color,
}

/// The kit for the current appearance: the neutral tokens, light or dark.
pub fn kit(dark: bool) -> Kit {
    Kit::neutral(dark)
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
            ProjectState::Idle => self.pending,
        }
    }

    pub fn for_run(self, state: RunStatus) -> Color {
        match state {
            RunStatus::Running => self.running,
            RunStatus::Success => self.success,
            RunStatus::Failure => self.failure,
            RunStatus::Error => self.error,
            RunStatus::Other => self.pending,
        }
    }

    pub fn for_step(self, state: StepStatus) -> Color {
        match state {
            StepStatus::Running => self.running,
            StepStatus::Success => self.success,
            StepStatus::Failure | StepStatus::Timeout => self.failure,
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
