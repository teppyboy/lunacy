//! Cohesive Lucide-style stroke icon catalog for `lunacy-ui`.
//!
//! Every icon uses a uniform `16.0` square viewBox, `1.4` stroke width, round caps/joins, and only
//! Rune-supported SVG primitives (`<g>`, `<path>`, `<rect>`, `<circle>`, `<ellipse>`, `<line>`,
//! `<polyline>`, `<polygon>`).

use rune::Icon;

macro_rules! stroke_icon {
    ($ident:ident, $name:literal, $inner:expr) => {
        pub const $ident: Icon = Icon {
            name: $name,
            view: 16.0,
            body: concat!(
                r#"<g fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">"#,
                $inner,
                r#"</g>"#,
            ),
        };
    };
}

// ---------------------------------------------------------------------------------------------
// Navigation & window chrome

stroke_icon!(
    LAYERS,
    "lunacy-layers",
    r#"<polygon points="8,2 14,5.25 8,8.5 2,5.25"/><polyline points="2,8.5 8,11.75 14,8.5"/><polyline points="2,11.5 8,14.5 14,11.5"/>"#
);

stroke_icon!(
    SETTINGS,
    "lunacy-settings",
    r#"<circle cx="8" cy="8" r="2.1"/><path d="M6.8 2h2.4l.4 1.5a4.6 4.6 0 0 1 1.4.8l1.5-.5 1.2 2.1-1.2 1.2a4.6 4.6 0 0 1 0 1.8l1.2 1.2-1.2 2.1-1.5-.5a4.6 4.6 0 0 1-1.4.8L9.2 14H6.8l-.4-1.5a4.6 4.6 0 0 1-1.4-.8l-1.5.5-1.2-2.1 1.2-1.2a4.6 4.6 0 0 1 0-1.8L2.3 5.9l1.2-2.1 1.5.5a4.6 4.6 0 0 1 1.4-.8L6.8 2Z"/>"#
);

stroke_icon!(
    MENU,
    "lunacy-menu",
    r#"<line x1="2.5" y1="4.5" x2="13.5" y2="4.5"/><line x1="2.5" y1="8" x2="13.5" y2="8"/><line x1="2.5" y1="11.5" x2="13.5" y2="11.5"/>"#
);

stroke_icon!(
    PLUG,
    "lunacy-plug",
    r#"<path d="M9.5 2.5v3M6.5 2.5v3M4.5 5.5h7V8a3.5 3.5 0 0 1-7 0V5.5Z"/><path d="M8 11.5V14"/>"#
);

stroke_icon!(
    WIFI,
    "lunacy-wifi",
    r#"<path d="M2 6.2a9 9 0 0 1 12 0"/><path d="M4.2 8.7a5.8 5.8 0 0 1 7.6 0"/><path d="M6.4 11.2a2.5 2.5 0 0 1 3.2 0"/><circle cx="8" cy="13.2" r="0.8" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    WIFI_OFF,
    "lunacy-wifi-off",
    r#"<line x1="2" y1="2" x2="14" y2="14"/><path d="M6.2 4.3a9 9 0 0 1 7.8 1.9"/><path d="M2 6.2a9 9 0 0 1 2-1.3"/><path d="M8.8 7.5a5.8 5.8 0 0 1 3 1.2"/><path d="M4.2 8.7a5.8 5.8 0 0 1 2.1-1.1"/><path d="M6.4 11.2a2.5 2.5 0 0 1 3.2 0"/><circle cx="8" cy="13.2" r="0.8" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    LOCK,
    "lunacy-lock",
    r#"<rect x="3.2" y="7.2" width="9.6" height="6.8" rx="1.5"/><path d="M5.4 7.2V5a2.6 2.6 0 0 1 5.2 0v2.2"/>"#
);

stroke_icon!(
    KEY,
    "lunacy-key",
    r#"<circle cx="5.5" cy="10.5" r="3"/><path d="M7.8 8.2L13.5 2.5M11.5 4.5l1.7 1.7M9.7 6.3l1.5 1.5"/>"#
);

stroke_icon!(
    GLOBE,
    "lunacy-globe",
    r#"<circle cx="8" cy="8" r="6"/><ellipse cx="8" cy="8" rx="2.7" ry="6"/><line x1="2" y1="8" x2="14" y2="8"/>"#
);

// ---------------------------------------------------------------------------------------------
// Status, banners & toasts

stroke_icon!(
    CHECK_CIRCLE,
    "lunacy-check-circle",
    r#"<circle cx="8" cy="8" r="6"/><polyline points="5.4,8.2 7.2,10 10.8,6.2"/>"#
);

stroke_icon!(
    ALERT_CIRCLE,
    "lunacy-alert-circle",
    r#"<circle cx="8" cy="8" r="6"/><line x1="8" y1="5" x2="8" y2="8.4"/><circle cx="8" cy="11" r="0.75" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    ALERT_TRIANGLE,
    "lunacy-alert-triangle",
    r#"<path d="M7 2.8L2.1 11.8a1.1 1.1 0 0 0 1 1.7h9.8a1.1 1.1 0 0 0 1-1.7L9 2.8a1.1 1.1 0 0 0-2 0Z"/><line x1="8" y1="6.2" x2="8" y2="9.1"/><circle cx="8" cy="11.3" r="0.75" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    INFO,
    "lunacy-info",
    r#"<circle cx="8" cy="8" r="6"/><line x1="8" y1="7.4" x2="8" y2="11.1"/><circle cx="8" cy="5.2" r="0.75" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    DOWNLOAD,
    "lunacy-download",
    r#"<path d="M8 2.5v7.5"/><polyline points="5,7.2 8,10.2 11,7.2"/><path d="M2.5 11.2v1.5a1.3 1.3 0 0 0 1.3 1.3h8.4a1.3 1.3 0 0 0 1.3-1.3v-1.5"/>"#
);

stroke_icon!(
    ARROW_UP_CIRCLE,
    "lunacy-arrow-up-circle",
    r#"<circle cx="8" cy="8" r="6"/><polyline points="5.5,7.8 8,5.3 10.5,7.8"/><line x1="8" y1="10.7" x2="8" y2="5.3"/>"#
);

// ---------------------------------------------------------------------------------------------
// Settings pages

stroke_icon!(
    SLIDERS,
    "lunacy-sliders",
    r#"<line x1="2.5" y1="4.2" x2="6" y2="4.2"/><line x1="8.8" y1="4.2" x2="13.5" y2="4.2"/><circle cx="7.4" cy="4.2" r="1.4"/><line x1="2.5" y1="8" x2="9.6" y2="8"/><line x1="12.4" y1="8" x2="13.5" y2="8"/><circle cx="11" cy="8" r="1.4"/><line x1="2.5" y1="11.8" x2="4.2" y2="11.8"/><line x1="7" y1="11.8" x2="13.5" y2="11.8"/><circle cx="5.6" cy="11.8" r="1.4"/>"#
);

stroke_icon!(
    SERVER,
    "lunacy-server",
    r#"<rect x="2.2" y="2.5" width="11.6" height="4.5" rx="1.2"/><rect x="2.2" y="9" width="11.6" height="4.5" rx="1.2"/><circle cx="4.8" cy="4.75" r="0.75" fill="currentColor" stroke="none"/><circle cx="4.8" cy="11.25" r="0.75" fill="currentColor" stroke="none"/>"#
);

/// GitHub Octocat mark on a `16.0` viewBox.
pub const GITHUB: Icon = Icon {
    name: "lunacy-github",
    view: 16.0,
    body: r#"<path fill="currentColor" d="M8 1.5a6.5 6.5 0 0 0-2.06 12.67c.33.06.45-.14.45-.31v-1.1c-1.81.39-2.19-.87-2.19-.87-.3-.75-.72-.95-.72-.95-.59-.4.04-.4.04-.4.65.05 1 .67 1 .67.58 1 1.52.71 1.9.54.06-.42.23-.71.41-.87-1.44-.16-2.96-.72-2.96-3.22 0-.71.25-1.29.67-1.75-.07-.16-.29-.83.06-1.73 0 0 .55-.18 1.79.67a6.2 6.2 0 0 1 3.26 0c1.24-.85 1.79-.67 1.79-.67.35.9.13 1.57.06 1.73.42.46.67 1.04.67 1.75 0 2.51-1.52 3.06-2.97 3.22.23.2.44.6.44 1.21v1.79c0 .17.12.37.45.31A6.5 6.5 0 0 0 8 1.5Z"/>"#,
};

stroke_icon!(
    MESSAGE_SQUARE,
    "lunacy-message-square",
    r#"<path d="M13.5 9.8a1.5 1.5 0 0 1-1.5 1.5H5.5L2.5 14V4a1.5 1.5 0 0 1 1.5-1.5h8A1.5 1.5 0 0 1 13.5 4v5.8Z"/>"#
);

stroke_icon!(
    TERMINAL,
    "lunacy-terminal",
    r#"<polyline points="3,11.5 7,7.5 3,3.5"/><line x1="8" y1="12.5" x2="13.5" y2="12.5"/>"#
);

stroke_icon!(
    BOX,
    "lunacy-box",
    r#"<path d="M13.5 5.2L8 2 2.5 5.2v5.6L8 14l5.5-3.2V5.2Z"/><polyline points="2.5,5.2 8,8.4 13.5,5.2"/><line x1="8" y1="8.4" x2="8" y2="14"/>"#
);

stroke_icon!(
    FOLDER_GIT,
    "lunacy-folder-git",
    r#"<path d="M2 4a1.5 1.5 0 0 1 1.5-1.5h2.9l1.3 1.6h4.8A1.5 1.5 0 0 1 14 5.6v6.4a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 12V4Z"/><circle cx="8" cy="8.8" r="1.3"/><line x1="4.5" y1="8.8" x2="6.7" y2="8.8"/><line x1="9.3" y1="8.8" x2="11.5" y2="8.8"/>"#
);

stroke_icon!(
    ROCKET,
    "lunacy-rocket",
    r#"<path d="M8.5 10.5l-3-3c.4-.9 1-1.9 1.7-2.6C9.1 3 11.4 2.3 13.6 2.4c.1 2.2-.6 4.5-2.5 6.4-.7.7-1.7 1.3-2.6 1.7Z"/><path d="M6.4 6.5L3.6 6.8l-1.1 1.8 2.6.8"/><path d="M9.5 9.6l-.3 2.8-1.8 1.1-.8-2.6"/><path d="M3.2 12.8l1.6-1.6"/><circle cx="10.2" cy="5.8" r="0.85" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    ACTIVITY,
    "lunacy-activity",
    r#"<polyline points="1.8,8 4.4,8 6.2,3 9.8,13 11.6,8 14.2,8"/>"#
);

stroke_icon!(
    PALETTE,
    "lunacy-palette",
    r#"<path d="M8 2a6 6 0 1 0 0 12 1.4 1.4 0 0 0 1.4-1.4c0-.35-.13-.68-.36-.93a1.3 1.3 0 0 1 .96-2.17h1.4A2.6 2.6 0 0 0 14 6.9C14 4.1 11.3 2 8 2Z"/><circle cx="5.4" cy="6.6" r="0.8" fill="currentColor" stroke="none"/><circle cx="8" cy="4.8" r="0.8" fill="currentColor" stroke="none"/><circle cx="10.6" cy="6.6" r="0.8" fill="currentColor" stroke="none"/><circle cx="5.2" cy="9.6" r="0.8" fill="currentColor" stroke="none"/>"#
);

// ---------------------------------------------------------------------------------------------
// Actions & controls

stroke_icon!(
    PLAY,
    "lunacy-play",
    r#"<polygon points="4.5,2.8 13,8 4.5,13.2"/>"#
);

stroke_icon!(
    SQUARE,
    "lunacy-square",
    r#"<rect x="3.2" y="3.2" width="9.6" height="9.6" rx="1.4"/>"#
);

stroke_icon!(
    X_CIRCLE,
    "lunacy-x-circle",
    r#"<circle cx="8" cy="8" r="6"/><line x1="5.8" y1="5.8" x2="10.2" y2="10.2"/><line x1="10.2" y1="5.8" x2="5.8" y2="10.2"/>"#
);

stroke_icon!(
    REFRESH_CW,
    "lunacy-refresh-cw",
    r#"<path d="M13.5 7.5a5.5 5.5 0 0 0-9.6-3.2L2.5 5.7"/><polyline points="2.5,2.8 2.5,5.7 5.4,5.7"/><path d="M2.5 8.5a5.5 5.5 0 0 0 9.6 3.2l1.4-1.4"/><polyline points="13.5,13.2 13.5,10.3 10.6,10.3"/>"#
);

stroke_icon!(
    ROTATE_CW,
    "lunacy-rotate-cw",
    r#"<path d="M13.2 8a5.2 5.2 0 1 1-1.5-3.7L13.5 6"/><polyline points="13.5,2.7 13.5,6 10.2,6"/>"#
);

stroke_icon!(
    ARROW_LEFT,
    "lunacy-arrow-left",
    r#"<line x1="13" y1="8" x2="3" y2="8"/><polyline points="7.2,3.8 3,8 7.2,12.2"/>"#
);

stroke_icon!(
    EXTERNAL_LINK,
    "lunacy-external-link",
    r#"<path d="M10 2.5h3.5V6"/><line x1="6.8" y1="9.2" x2="13.5" y2="2.5"/><path d="M12 8.8v3.4a1.3 1.3 0 0 1-1.3 1.3H3.8a1.3 1.3 0 0 1-1.3-1.3V5.3A1.3 1.3 0 0 1 3.8 4h3.4"/>"#
);

stroke_icon!(
    CHECK,
    "lunacy-check",
    r#"<polyline points="3,8.5 6.4,11.8 13,4.5"/>"#
);

stroke_icon!(
    UNDO,
    "lunacy-undo",
    r#"<polyline points="5.5,4 2.5,6.8 5.5,9.6"/><path d="M2.5 6.8h7.2a3.3 3.3 0 0 1 0 6.6H7"/>"#
);

stroke_icon!(
    PLUS,
    "lunacy-plus",
    r#"<line x1="8" y1="3" x2="8" y2="13"/><line x1="3" y1="8" x2="13" y2="8"/>"#
);

stroke_icon!(
    COPY,
    "lunacy-copy",
    r#"<rect x="5.8" y="5.8" width="7.7" height="7.7" rx="1.3"/><path d="M3.8 10.2H3.5A1.3 1.3 0 0 1 2.2 8.9V3.5a1.3 1.3 0 0 1 1.3-1.3h5.4a1.3 1.3 0 0 1 1.3 1.3v.3"/>"#
);

stroke_icon!(
    TRASH,
    "lunacy-trash",
    r#"<polyline points="2.5,4.2 13.5,4.2"/><path d="M12.2 4.2v8.2a1.3 1.3 0 0 1-1.3 1.3H5.1a1.3 1.3 0 0 1-1.3-1.3V4.2"/><path d="M5.8 4.2V3a1 1 0 0 1 1-1h2.4a1 1 0 0 1 1 1v1.2"/><line x1="6.6" y1="7" x2="6.6" y2="11"/><line x1="9.4" y1="7" x2="9.4" y2="11"/>"#
);

stroke_icon!(
    SEND,
    "lunacy-send",
    r#"<path d="M14 2L7.2 8.8"/><polygon points="14,2 9.8,14 7.2,8.8 2,6.2"/>"#
);

stroke_icon!(
    EDIT,
    "lunacy-edit",
    r#"<path d="M11.2 2.4a1.6 1.6 0 0 1 2.3 2.3l-7.7 7.7-3.1.8.8-3.1 7.7-7.7Z"/>"#
);

// ---------------------------------------------------------------------------------------------
// Logs & steps toolbar

stroke_icon!(
    LIST_CHECKS,
    "lunacy-list-checks",
    r#"<polyline points="2.2,4.8 3.5,6.1 5.8,3.6"/><line x1="8" y1="4.8" x2="13.8" y2="4.8"/><polyline points="2.2,9.8 3.5,11.1 5.8,8.6"/><line x1="8" y1="9.8" x2="13.8" y2="9.8"/><line x1="8" y1="13" x2="13.8" y2="13"/>"#
);

stroke_icon!(
    WRAP_TEXT,
    "lunacy-wrap-text",
    r#"<line x1="2.2" y1="4" x2="13.8" y2="4"/><path d="M2.2 8h9.2a2.2 2.2 0 0 1 0 4.4H8.6"/><polyline points="10.2,10.8 8.5,12.4 10.2,14"/><line x1="2.2" y1="12.4" x2="5.8" y2="12.4"/>"#
);

stroke_icon!(
    ALIGN_LEFT,
    "lunacy-align-left",
    r#"<line x1="2.2" y1="4" x2="13.8" y2="4"/><line x1="2.2" y1="8" x2="10.2" y2="8"/><line x1="2.2" y1="12" x2="12.2" y2="12"/>"#
);

stroke_icon!(
    CHEVRONS_UP_DOWN,
    "lunacy-chevrons-up-down",
    r#"<polyline points="5,6.2 8,3.2 11,6.2"/><polyline points="5,9.8 8,12.8 11,9.8"/>"#
);

stroke_icon!(
    ARROW_DOWN,
    "lunacy-arrow-down",
    r#"<line x1="8" y1="3" x2="8" y2="13"/><polyline points="3.8,8.8 8,13 12.2,8.8"/>"#
);

// ---------------------------------------------------------------------------------------------
// Triggers, Git refs, badges & appearance

stroke_icon!(
    ZAP,
    "lunacy-zap",
    r#"<polygon points="8.8,1.8 2.8,9 7.8,9 7.2,14.2 13.2,7 8.2,7"/>"#
);

stroke_icon!(
    CLOCK,
    "lunacy-clock",
    r#"<circle cx="8" cy="8" r="6"/><polyline points="8,4.6 8,8 10.4,9.5"/>"#
);

stroke_icon!(
    GIT_PULL_REQUEST,
    "lunacy-git-pull-request",
    r#"<circle cx="4.5" cy="4" r="1.7"/><circle cx="4.5" cy="12" r="1.7"/><circle cx="11.5" cy="12" r="1.7"/><line x1="4.5" y1="5.7" x2="4.5" y2="10.3"/><path d="M11.5 10.3V6a2 2 0 0 0-2-2H8.2"/><polyline points="9.6,2.5 8,4 9.6,5.5"/>"#
);

stroke_icon!(
    GIT_BRANCH,
    "lunacy-git-branch",
    r#"<circle cx="4.5" cy="4" r="1.7"/><circle cx="4.5" cy="12" r="1.7"/><circle cx="11.5" cy="5.5" r="1.7"/><line x1="4.5" y1="5.7" x2="4.5" y2="10.3"/><path d="M11.5 7.2a3.3 3.3 0 0 1-3.3 3.3H6.2"/>"#
);

stroke_icon!(
    GIT_COMMIT,
    "lunacy-git-commit",
    r#"<circle cx="8" cy="8" r="2.4"/><line x1="2" y1="8" x2="5.6" y2="8"/><line x1="10.4" y1="8" x2="14" y2="8"/>"#
);

stroke_icon!(
    TAG,
    "lunacy-tag",
    r#"<path d="M8.4 2.2H3.5A1.3 1.3 0 0 0 2.2 3.5v4.9a1.3 1.3 0 0 0 .38.92l4.8 4.8a1.3 1.3 0 0 0 1.84 0l4.1-4.1a1.3 1.3 0 0 0 0-1.84l-4.8-4.8a1.3 1.3 0 0 0-.92-.38Z"/><circle cx="5.4" cy="5.4" r="0.85" fill="currentColor" stroke="none"/>"#
);

stroke_icon!(
    PAUSE,
    "lunacy-pause",
    r#"<rect x="4" y="3" width="2.8" height="10" rx="0.8"/><rect x="9.2" y="3" width="2.8" height="10" rx="0.8"/>"#
);

stroke_icon!(
    MONITOR,
    "lunacy-monitor",
    r#"<rect x="2" y="2.5" width="12" height="8.5" rx="1.3"/><line x1="5.2" y1="13.5" x2="10.8" y2="13.5"/><line x1="8" y1="11" x2="8" y2="13.5"/>"#
);

stroke_icon!(
    SUN,
    "lunacy-sun",
    r#"<circle cx="8" cy="8" r="2.7"/><line x1="8" y1="1.8" x2="8" y2="3.2"/><line x1="8" y1="12.8" x2="8" y2="14.2"/><line x1="1.8" y1="8" x2="3.2" y2="8"/><line x1="12.8" y1="8" x2="14.2" y2="8"/><line x1="3.6" y1="3.6" x2="4.6" y2="4.6"/><line x1="11.4" y1="11.4" x2="12.4" y2="12.4"/><line x1="12.4" y1="3.6" x2="11.4" y2="4.6"/><line x1="4.6" y1="11.4" x2="3.6" y2="12.4"/>"#
);

stroke_icon!(
    MOON,
    "lunacy-moon",
    r#"<path d="M13.2 9.4A5.5 5.5 0 0 1 6.6 2.8a5.5 5.5 0 1 0 6.6 6.6Z"/>"#
);

stroke_icon!(
    SPARKLE,
    "lunacy-sparkle",
    r#"<path d="M8 1.8Q9.6 6.4 14.2 8Q9.6 9.6 8 14.2Q6.4 9.6 1.8 8Q6.4 6.4 8 1.8Z"/>"#
);

stroke_icon!(
    CPU,
    "lunacy-cpu",
    r#"<rect x="4" y="4" width="8" height="8" rx="1.2"/><rect x="6.5" y="6.5" width="3" height="3" rx="0.4"/><line x1="6" y1="1.8" x2="6" y2="4"/><line x1="10" y1="1.8" x2="10" y2="4"/><line x1="6" y1="12" x2="6" y2="14.2"/><line x1="10" y1="12" x2="10" y2="14.2"/><line x1="1.8" y1="6" x2="4" y2="6"/><line x1="1.8" y1="10" x2="4" y2="10"/><line x1="12" y1="6" x2="14.2" y2="6"/><line x1="12" y1="10" x2="14.2" y2="10"/>"#
);

stroke_icon!(
    HISTORY,
    "lunacy-history",
    r#"<path d="M2.5 8a5.5 5.5 0 1 0 1.4-3.7L2.5 5.7"/><polyline points="2.5,2.5 2.5,5.7 5.7,5.7"/><polyline points="8,5.2 8,8 10.2,9.4"/>"#
);

/// Every icon defined in this catalog, for validation tests.
pub const ALL: &[&Icon] = &[
    &LAYERS,
    &SETTINGS,
    &MENU,
    &PLUG,
    &WIFI,
    &WIFI_OFF,
    &LOCK,
    &KEY,
    &GLOBE,
    &CHECK_CIRCLE,
    &ALERT_CIRCLE,
    &ALERT_TRIANGLE,
    &INFO,
    &DOWNLOAD,
    &ARROW_UP_CIRCLE,
    &SLIDERS,
    &SERVER,
    &GITHUB,
    &MESSAGE_SQUARE,
    &TERMINAL,
    &BOX,
    &FOLDER_GIT,
    &ROCKET,
    &ACTIVITY,
    &PALETTE,
    &PLAY,
    &SQUARE,
    &X_CIRCLE,
    &REFRESH_CW,
    &ROTATE_CW,
    &ARROW_LEFT,
    &EXTERNAL_LINK,
    &CHECK,
    &UNDO,
    &PLUS,
    &COPY,
    &TRASH,
    &SEND,
    &EDIT,
    &LIST_CHECKS,
    &WRAP_TEXT,
    &ALIGN_LEFT,
    &CHEVRONS_UP_DOWN,
    &ARROW_DOWN,
    &ZAP,
    &CLOCK,
    &GIT_PULL_REQUEST,
    &GIT_BRANCH,
    &GIT_COMMIT,
    &TAG,
    &PAUSE,
    &MONITOR,
    &SUN,
    &MOON,
    &SPARKLE,
    &CPU,
    &HISTORY,
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use rune::{row, Color, Cx, El, Env, Runtime};

    use super::*;

    const SUPPORTED_TAGS: &[&str] = &[
        "g", "path", "rect", "circle", "ellipse", "line", "polyline", "polygon",
    ];

    #[test]
    fn catalog_icons_have_unique_names_valid_viewbox_and_supported_primitives() {
        let mut names = HashSet::new();
        assert!(!ALL.is_empty());
        for ic in ALL {
            assert!(!ic.name.trim().is_empty(), "icon name must be non-empty");
            assert!(
                names.insert(ic.name),
                "duplicate icon name in catalog: {}",
                ic.name
            );
            assert!(
                ic.view.is_finite() && ic.view > 0.0,
                "icon {} must have positive finite viewBox, got {}",
                ic.name,
                ic.view
            );
            assert!(
                !ic.body.trim().is_empty(),
                "icon {} must have non-empty SVG body",
                ic.name
            );
            let mut has_primitive = false;
            for raw in ic.body.split('>') {
                let el = raw.trim();
                if el.is_empty() || el.starts_with("</") {
                    continue;
                }
                assert!(
                    el.starts_with('<'),
                    "icon {} has malformed SVG fragment: {el:?}",
                    ic.name
                );
                let tag = el
                    .trim_start_matches('<')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/');
                assert!(
                    SUPPORTED_TAGS.contains(&tag),
                    "icon {} uses unsupported Rune SVG tag <{tag}>",
                    ic.name
                );
                if tag != "g" {
                    has_primitive = true;
                }
            }
            assert!(
                has_primitive,
                "icon {} must contain at least one drawing primitive",
                ic.name
            );
        }
    }

    #[test]
    fn catalog_icons_render_in_headless_runtime_without_panicking() {
        fn gallery(_s: &(), _cx: &mut Cx<()>) -> El {
            let mut r = row().w_full().gap(4.0).flex_wrap();
            for ic in ALL {
                r = r.child(rune::icon(ic, 16.0).key(ic.name).color(Color::WHITE));
            }
            r
        }

        let mut rt = Runtime::new((), gallery, (480.0, 240.0), Env::new(true, 1.0));
        for _ in 0..3 {
            rt.settle_all();
            rt.frame(0.016);
        }
        for ic in ALL {
            assert!(
                rt.find(ic.name).is_some(),
                "icon {} missing from rendered frame",
                ic.name
            );
        }
    }
}
