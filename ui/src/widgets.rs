//! Small pieces every screen draws: status dots and pills, section cards, key/value rows,
//! timestamps, durations, and the log body.

use rune::{col, el, icon, row, text, Align, Color, El, Icon, Tx};
use rune_kit::{Badge, Kit};

use crate::icons;
use crate::model::{ProjectState, RunStatus, StepStatus};
use crate::theme::{self, Status};

/// The Lunacy brand mark (128×128 viewBox), matching `assets/logo.svg`.
pub const LUNACY_LOGO: Icon = Icon {
    name: "lunacy-logo",
    view: 128.0,
    body: concat!(
        r#"<g fill="currentColor" fill-opacity="0.45">"#,
        r#"<path d="M98.93 44.21A40.74 32.49 -13 0 1 89.27 78.47L86.89 72.73A37.74 30.09 -13 0 0 98.93 44.21Z"/>"#,
        r#"<path d="M77.89 85.93A40.74 32.49 -13 0 1 25.16 76.2L33.28 73.82A37.74 30.09 -13 0 0 71.43 80.7Z"/>"#,
        r#"<path d="M20.98 66.99A40.74 32.49 -13 0 1 30.44 38.38A37.74 30.09 -13 0 0 26.1 63.39Z"/>"#,
        r#"</g>"#,
        r#"<g fill="currentColor">"#,
        r#"<path d="M71.71 17.65A57.24 45.89 -13 0 1 105.45 28.28L100.25 30.08A49.99 40.08 -13 0 0 71.71 17.65Z"/>"#,
        r#"<path d="M111.98 34.18A57.24 45.89 -13 0 1 100.48 97.69L93.33 86.18A49.99 40.08 -13 0 0 106.14 37.53Z"/>"#,
        r#"<path d="M93.63 102.04A57.24 45.89 -13 0 1 10.87 81.6L16.62 79.29A49.99 40.08 -13 0 0 84.23 91.95Z"/>"#,
        r#"<path d="M8.99 74.35A57.24 45.89 -13 0 1 15.44 46.05A49.99 40.08 -13 0 0 13.16 71.9Z"/>"#,
        r#"<path fill-rule="evenodd" d="M33.22 17.29C56.29 42.59 70.3 46.01 117.46 27.38C73.58 50.25 70.81 61.34 103.85 108.68C65.96 63.63 52.13 60.37 6 79.95C48.84 56.13 51.44 44.87 33.22 17.29ZM48.3 36.6C58.44 50.55 57.62 54.09 35.78 65.42C58.38 55.07 62.73 56.1 80.79 78.64C63.85 55.57 64.72 52.08 87.06 41.24C63.96 51.1 59.56 50.02 48.3 36.6Z"/>"#,
        r#"</g>"#,
    ),
};

/// Minimal monochrome brand logo mark that adapts cleanly across Dark, Light, and OLED themes.
pub fn brand_logo(k: &Kit, size: f32) -> El {
    el().key("lunacy-logo")
        .size(size, size)
        .center()
        .a11y_hidden()
        .child(icon(&LUNACY_LOGO, size).color(k.c.text_base))
}

/// A status dot: a filled circle in the status ink.
pub fn dot(ink: Color, size: f32) -> El {
    el().size(size, size).radius(size * 0.5).fill(ink)
}

/// A pulsing status dot for active/connecting indicators.
pub fn live_dot(ink: Color, size: f32) -> El {
    dot(ink, size).looping(rune::LoopSpec::opacity(1.4, 0.0, 0.35, 1.0))
}

/// A ring for a running step, so "in progress" reads differently from "done".
pub fn ring(ink: Color, size: f32, width: f32) -> El {
    el().size(size, size).radius(size * 0.5).border(width, ink)
}

/// A pill: coloured text on a tinted rounded surface.
pub fn pill(k: &Kit, label: impl Into<String>, ink: Color) -> El {
    el().h(20.0)
        .px(7.0)
        .center()
        .radius(k.radius.sm)
        .fill(theme::status(k).tint(ink))
        .child(text(label).font(k.ty.small_medium).color(ink).nowrap())
}

/// A pill with a crisp leading 12pt stroke icon.
pub fn pill_with_icon(k: &Kit, ic: &'static Icon, label: impl Into<String>, ink: Color) -> El {
    row()
        .h(20.0)
        .px(7.0)
        .gap(4.0)
        .center_y()
        .radius(k.radius.sm)
        .fill(theme::status(k).tint(ink))
        .child(icon(ic, 12.0).color(ink).a11y_hidden())
        .child(text(label).font(k.ty.small_medium).color(ink).nowrap())
}

/// A deploy-only badge (`Rocket` + "Deploy only").
pub fn deploy_only_pill(k: &Kit) -> El {
    pill_with_icon(k, &icons::ROCKET, "Deploy only", theme::status(k).running)
}

/// A paused project badge (`Pause` + "Paused").
pub fn paused_pill(k: &Kit) -> El {
    pill_with_icon(k, &icons::PAUSE, "Paused", k.c.text_faint)
}

/// A run's status pill.
pub fn run_pill(k: &Kit, status: RunStatus) -> El {
    let s = theme::status(k);
    let (label, ink) = match status {
        RunStatus::Running => ("Running", s.running),
        RunStatus::Success => ("Success", s.success),
        RunStatus::Failure => ("Failed", s.failure),
        RunStatus::Error => ("Error", s.error),
        RunStatus::Cancelled => ("Cancelled", s.cancelled),
        RunStatus::Other => ("Unknown", s.pending),
    };
    pill(k, label, ink)
}

/// A project's status pill.
pub fn project_pill(k: &Kit, state: ProjectState) -> El {
    let s = theme::status(k);
    let (label, ink) = match state {
        ProjectState::Running => ("Running", s.running),
        ProjectState::Success => ("Success", s.success),
        ProjectState::Failure => ("Failed", s.failure),
        ProjectState::Error => ("Error", s.error),
        ProjectState::Cancelled => ("Cancelled", s.cancelled),
        ProjectState::Idle => ("Idle", s.pending),
    };
    pill(k, label, ink)
}

/// High-contrast, always-visible scrollbar style legible across Light, Dark, and OLED themes.
pub fn scroll_bar_style(k: &Kit) -> rune_kit::ScrollBarStyle {
    rune_kit::ScrollBarStyle::from_kit(k)
        .always_visible()
        .thumb(k.c.icon_faint)
        .thumb_hover(k.c.icon_muted)
        .track(k.c.layer_02.alpha(0.65))
        .track_hover(k.c.layer_03.alpha(0.9))
        .width(7.0)
        .hover_width(10.0)
        .min_thumb(28.0)
        .track_inset(3.0)
}

/// Always-visible draggable vertical scrollbar for scrollable panels.
pub fn scroll_bar(viewport: El, k: &Kit) -> El {
    use rune_kit::ScrollBar;
    viewport.scroll_bar_styled(scroll_bar_style(k))
}

/// Always-visible draggable horizontal scrollbar for sideways strips.
pub fn scroll_bar_x(viewport: El, k: &Kit) -> El {
    use rune_kit::ScrollBar;
    viewport.scroll_bar_styled(scroll_bar_style(k).horizontal())
}

/// Always-visible vertical and horizontal scrollbars for two-axis scrollable panels.
pub fn scroll_bars_xy(viewport: El, k: &Kit) -> El {
    use rune_kit::ScrollBar;
    let style = scroll_bar_style(k);
    viewport.scroll_bars_xy_styled(style, style.horizontal())
}

/// A card: a rounded, bordered surface with an optional title and body.
pub fn card(k: &Kit) -> El {
    col()
        .w_full()
        .gap(0.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(theme::SECTION_BORDER, k.c.border_base)
}

/// A titled section inside a settings page.
pub fn section(k: &Kit, title: &str, blurb: Option<&str>) -> El {
    let mut head = col()
        .w_full()
        .gap(3.0)
        .child(text(title).font(k.ty.ui_medium).color(k.c.text_base));
    if let Some(blurb) = blurb {
        head = head.child(
            text(blurb)
                .font(k.ty.caption())
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    col().w_full().gap(14.0).child(head)
}

/// A titled section with a subtle leading 14pt icon.
pub fn section_icon(k: &Kit, ic: &'static Icon, title: &str, blurb: Option<&str>) -> El {
    let mut head = col().w_full().gap(3.0).child(
        row()
            .gap(7.0)
            .center_y()
            .child(icon(ic, 14.0).color(k.c.icon_muted).a11y_hidden())
            .child(text(title).font(k.ty.ui_medium).color(k.c.text_base)),
    );
    if let Some(blurb) = blurb {
        head = head.child(
            text(blurb)
                .font(k.ty.caption())
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    col().w_full().gap(14.0).child(head)
}

/// A label above a control, with optional help text under it.
pub fn labelled(k: &Kit, label: &str, help: Option<&str>, control: El) -> El {
    let mut c = col()
        .w_full()
        .gap(6.0)
        .items_start()
        .child(text(label).font(k.ty.small_medium).color(k.c.text_base));
    c = c.child(control);
    if let Some(help) = help {
        c = c.child(text(help).font(k.ty.caption()).color(k.c.text_muted).wrap());
    }
    c
}

/// Expands small icon buttons to a 44×44 pt touch target on compact viewports.
pub fn compact_icon(el: El, compact: bool) -> El {
    if compact {
        el.size(44.0, 44.0)
    } else {
        el
    }
}

/// A 3-line menu icon for the compact navigation drawer toggle.
pub fn hamburger(ink: Color) -> El {
    icon(&icons::MENU, 18.0).color(ink).a11y_hidden()
}

/// A row of two fields side by side on desktop, stacked on compact viewports.
pub fn pair(left: El, right: El) -> El {
    pair_for(false, left, right)
}

/// Responsive `pair`: side-by-side columns above 720 pt, stacked vertically below.
pub fn pair_for(compact: bool, left: El, right: El) -> El {
    if compact {
        col().w_full().gap(14.0).child(left).child(right)
    } else {
        row()
            .w_full()
            .gap(16.0)
            .items_start()
            .child(left.grow(1.0))
            .child(right.grow(1.0))
    }
}

/// Muted uppercase caption for a group of rows.
pub fn caption(k: &Kit, label: &str) -> El {
    text(label).font(k.ty.small_medium).color(k.c.text_faint)
}

/// Muted uppercase caption paired with a subtle 12pt leading icon.
pub fn caption_icon(k: &Kit, ic: &'static Icon, label: &str) -> El {
    row()
        .gap(5.0)
        .center_y()
        .child(icon(ic, 12.0).color(k.c.icon_faint).a11y_hidden())
        .child(caption(k, label))
}

/// A single-line read-only value, for diagnostics.
pub fn value(k: &Kit, v: &str) -> El {
    value_for(k, false, v)
}

/// Read-only diagnostic value: single-line truncated on desktop, wrapped on compact viewports.
pub fn value_for(k: &Kit, compact: bool, v: &str) -> El {
    let t = text(v.to_string()).font(k.ty.mono).color(k.c.text_base);
    if compact {
        t.wrap_anywhere()
    } else {
        t.nowrap().truncate()
    }
}

/// A key/value row for diagnostics: label on the left, value on the right.
pub fn kv(k: &Kit, label: &str, value: El) -> El {
    kv_for(k, false, label, value)
}

/// Responsive key/value row: horizontal on desktop, label above wrapped value on compact.
pub fn kv_for(k: &Kit, compact: bool, label: &str, value: El) -> El {
    if compact {
        col()
            .w_full()
            .items_start()
            .min_h(44.0)
            .py(8.0)
            .gap(4.0)
            .child(text(label).font(k.ty.small).color(k.c.text_muted))
            .child(value)
    } else {
        row()
            .w_full()
            .min_h(28.0)
            .gap(16.0)
            .center_y()
            .justify_between()
            .child(text(label).font(k.ty.small).color(k.c.text_muted).nowrap())
            .child(value)
    }
}

/// `1234 ms`, `1.2 s`, `2m 14s`.
pub fn duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        let secs = ms as f64 / 1000.0;
        if secs < 10.0 {
            format!("{secs:.1} s")
        } else {
            format!("{} s", ms / 1000)
        }
    } else {
        let total = ms / 1000;
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// A duration between two unix seconds, when the second is known.
pub fn span(started: u64, finished: Option<u64>) -> Option<u64> {
    finished.map(|end| end.saturating_sub(started) * 1000)
}

/// `2025-11-25 13:03` in local time, from unix seconds.
pub fn timestamp(secs: u64) -> String {
    // `time` is not worth a dependency for one format; this is good enough for a run list and
    // never wrong, only less pretty across DST.
    let secs = secs as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

/// `13:03:22`, for a log line's own clock.
pub fn clock(secs: u64) -> String {
    let rem = (secs as i64).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Days-from-epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Relative time: `just now`, `3m ago`, `2h ago`, `4d ago`.
pub fn ago(now: u64, then: u64) -> String {
    let delta = now.saturating_sub(then);
    match delta {
        0..=9 => "just now".into(),
        10..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86_399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86_400),
    }
}

/// The commit sha as `abc1234`, the way git prints it.
pub fn short_sha(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// A monospace chip for a sha or an id.
pub fn mono_chip(k: &Kit, value: &str) -> El {
    el().h(20.0)
        .px(6.0)
        .center()
        .radius(k.radius.xs)
        .fill(k.c.layer_03)
        .child(
            text(value.to_string())
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .nowrap(),
        )
}

/// A monospace chip with a crisp leading 12pt icon.
pub fn mono_chip_icon(k: &Kit, ic: &'static Icon, value: &str) -> El {
    row()
        .h(20.0)
        .px(6.0)
        .gap(4.0)
        .center_y()
        .radius(k.radius.xs)
        .fill(k.c.layer_03)
        .child(icon(ic, 12.0).color(k.c.icon_faint).a11y_hidden())
        .child(
            text(value.to_string())
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .nowrap(),
        )
}

/// A commit SHA chip (`GIT_COMMIT` icon + 7-character short SHA).
pub fn sha_chip(k: &Kit, sha: &str) -> El {
    mono_chip_icon(k, &icons::GIT_COMMIT, &short_sha(sha))
}

/// Distinct pull-request pill (`PR #42`).
pub fn pr_pill(k: &Kit, number: u64) -> El {
    pill_with_icon(
        k,
        &icons::GIT_PULL_REQUEST,
        format!("PR #{number}"),
        k.c.text_accent,
    )
}

/// Formats a git ref for compact UI badges (`refs/tags/v1.0` -> `tags/v1.0`, `refs/heads/main` -> `main`).
pub fn short_ref(git_ref: &str) -> &str {
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/"))
        .unwrap_or(git_ref)
}

/// Returns a `PR #N` pill or a ref chip when `run` is a PR or non-default branch/tag.
pub fn run_ref_badge(k: &Kit, run: &crate::model::Run, default_branch: Option<&str>) -> Option<El> {
    if let Some(pr) = run.pr_number {
        let mut p = pr_pill(k, pr);
        if let Some(reason) = run.trigger_display() {
            p = p.help(reason);
        }
        return Some(p);
    }
    let raw = run.git_ref.as_deref().filter(|r| !r.is_empty())?;
    let display = short_ref(raw);
    if default_branch.is_some_and(|b| b == display || b == raw) {
        return None;
    }
    let ic = if raw.starts_with("refs/tags/") || display.starts_with("tags/") {
        &icons::TAG
    } else {
        &icons::GIT_BRANCH
    };
    Some(mono_chip_icon(k, ic, display))
}

/// Subtle neutral trigger pill (`Webhook`, `Manual`, `Poll`, `PR`, `Force deploy`, `Tag`) with a
/// leading icon and tooltip showing `run.trigger_display()`. Skipped when `deploy_only` already
/// shows `"Deploy only"` and `trigger == "force_deploy"`, or when `pr_number` already shows
/// `"PR #N"` and `trigger == "pull_request"`, to avoid visual duplication.
pub fn trigger_pill(k: &Kit, run: &crate::model::Run, key: impl Into<String>) -> Option<El> {
    if (run.deploy_only && run.trigger.as_deref() == Some("force_deploy"))
        || (run.pr_number.is_some() && run.trigger.as_deref() == Some("pull_request"))
    {
        return None;
    }
    let label = run.trigger_label()?;
    let ic = match run.trigger.as_deref() {
        Some("webhook") => &icons::ZAP,
        Some("manual") => &icons::PLAY,
        Some("poll") => &icons::CLOCK,
        Some("pull_request") => &icons::GIT_PULL_REQUEST,
        Some("force_deploy") => &icons::ROCKET,
        Some("tag") => &icons::TAG,
        _ => &icons::INFO,
    };
    let mut p = pill_with_icon(k, ic, label, k.c.text_muted).key(key);
    if let Some(reason) = run.trigger_display() {
        p = p.help(reason);
    }
    Some(p)
}

/// The reason a skipped step was skipped, or `None` when it was skipped for another reason.
pub fn skipped_reason(only_on: &[String], host_os: Option<&str>) -> Option<String> {
    if only_on.is_empty() {
        return None;
    }
    match host_os {
        Some(os) if !only_on.iter().any(|o| o == os) => {
            let hosts = only_on
                .iter()
                .map(|o| os_name(o))
                .collect::<Vec<_>>()
                .join(" or ");
            Some(format!("skipped (not on this OS: {hosts} only)"))
        }
        // The daemon's OS is unknown: still say the step is host-restricted.
        None => {
            let hosts = only_on
                .iter()
                .map(|o| os_name(o))
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("only runs on {hosts}"))
        }
        _ => None,
    }
}

/// `windows` → `Windows`.
pub fn os_name(os: &str) -> String {
    match os {
        "linux" => "Linux".into(),
        "windows" => "Windows".into(),
        "macos" => "macOS".into(),
        other => other.to_string(),
    }
}

/// A step's status text, with the skip reason when there is one.
pub fn step_status_label(status: StepStatus, only_on: &[String], host_os: Option<&str>) -> String {
    match status {
        StepStatus::Skipped => skipped_reason(only_on, host_os).unwrap_or_else(|| "Skipped".into()),
        other => other.label().into(),
    }
}

/// An empty state: a title and a line of explanation, centred.
pub fn empty_state(k: &Kit, title: &str, body: &str) -> El {
    col()
        .w_full()
        .grow(1.0)
        .center()
        .gap(6.0)
        .px(24.0)
        .child(
            text(title.to_string())
                .font(k.ty.ui_medium)
                .color(k.c.text_muted),
        )
        .child(
            text(body.to_string())
                .font(k.ty.small)
                .color(k.c.text_faint)
                .wrap()
                .align(Align::Center),
        )
}

/// A failure or informational banner inside a panel.
pub fn notice(k: &Kit, ink: Color, message: &str) -> El {
    let st = theme::status(k);
    let ic = if ink == st.failure || ink == st.error {
        &icons::ALERT_CIRCLE
    } else if ink == st.success {
        &icons::CHECK_CIRCLE
    } else {
        &icons::INFO
    };
    row()
        .w_full()
        .gap(8.0)
        .padding(10.0)
        .radius(k.radius.md)
        .fill(st.tint(ink))
        .items_start()
        .child(icon(ic, 14.0).color(ink).mt(2.0).a11y_hidden())
        .child(
            text(message.to_string())
                .font(k.ty.small)
                .color(ink)
                .wrap()
                .shrink(),
        )
}

/// A badge whose label the caller reads.
pub fn badge(k: &Kit, label: impl Into<String>) -> El {
    Badge::new(label).standard().build(k)
}

/// The log body: monospace, selectable, with optional per-line word wrapping.
pub fn log_body(k: &Kit, text: &str, wrap: bool) -> El {
    let style = k.ty.mono;
    if wrap {
        col()
            .w_full()
            .min_w(0.0)
            .gap(0.0)
            .pl(12.0)
            .pr(18.0)
            .py(8.0)
            .child(
                rune::text(text.to_string())
                    .key("log-text")
                    .font(style)
                    .color(k.c.text_base)
                    .wrap_anywhere()
                    .wrap_greedy()
                    .by_line()
                    .selectable(true)
                    .selection_color(k.selection()),
            )
    } else {
        col()
            .w_full()
            .gap(0.0)
            .pl(12.0)
            .pr(18.0)
            .pt(8.0)
            .pb(18.0)
            .child(
                rune::text(text.to_string())
                    .key("log-text")
                    .font(style)
                    .color(k.c.text_base)
                    .nowrap()
                    .selectable(true)
                    .selection_color(k.selection()),
            )
    }
}

/// A separator line.
pub fn divider(k: &Kit) -> El {
    el().w_full().h(theme::hair(1.0)).fill(k.c.border_muted)
}

/// Right-aligned muted text.
pub fn muted(k: &Kit, s: impl Into<String>) -> El {
    text(s.into()).font(k.ty.small).color(k.c.text_muted)
}

/// A muted caption centred in the row's trailing edge.
pub fn trailing_muted(k: &Kit, s: impl Into<String>) -> El {
    text(s.into())
        .font(k.ty.small)
        .color(k.c.text_muted)
        .nowrap()
}

/// A status icon in the status ink.
pub fn status_glyph(k: &Kit, status: StepStatus, host_os: Option<&str>) -> El {
    let s: Status = theme::status(k);
    let ink = s.for_step(status);
    let size = 14.0;
    match status {
        StepStatus::Success => icon(&rune_kit::glyphs::CHECK_SMALL, size).color(ink),
        StepStatus::Failure | StepStatus::Timeout => {
            icon(&rune_kit::glyphs::CIRCLE_X, size).color(ink)
        }
        StepStatus::Cancelled => icon(&rune_kit::glyphs::CIRCLE_X, size).color(ink),
        StepStatus::Running => rune_kit::Loader::new().size(size).color(ink).build(k),
        StepStatus::Skipped => {
            let _ = host_os;
            icon(&rune_kit::glyphs::DASH, size).color(ink)
        }
        StepStatus::Pending | StepStatus::Other => el()
            .size(size, size)
            .center()
            .child(el().size(6.0, 6.0).radius(3.0).border(1.5, ink)),
    }
}

/// A one-line, elided identifier row.
pub fn id_line(k: &Kit, label: &str, id: &str) -> El {
    row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(text(label).font(k.ty.small).color(k.c.text_muted).nowrap())
        .child(
            text(id.to_string())
                .font(k.ty.mono)
                .color(k.c.text_base)
                .nowrap()
                .truncate(),
        )
}

/// A row of choice chips: the picker used where a dropdown would need an overlay layer.
///
/// `options` are `(value, label)`. Picking one calls `on_pick` with that value.
pub fn choice_row<S: 'static>(
    k: &Kit,
    options: &[(String, String)],
    current: Option<&str>,
    placeholder: Option<&str>,
    on_pick: impl Fn(&mut S, &str, &mut Tx) + Clone + 'static,
) -> El {
    choice_row_for(k, false, options, current, placeholder, on_pick)
}

/// Responsive `choice_row`: chips expand to ≥ 44 pt touch targets when `compact`.
pub fn choice_row_for<S: 'static>(
    k: &Kit,
    compact: bool,
    options: &[(String, String)],
    current: Option<&str>,
    placeholder: Option<&str>,
    on_pick: impl Fn(&mut S, &str, &mut Tx) + Clone + 'static,
) -> El {
    let items: Vec<(String, String, Option<&'static Icon>)> = options
        .iter()
        .map(|(v, l)| (v.clone(), l.clone(), None))
        .collect();
    choice_row_for_icons(k, compact, &items, current, placeholder, on_pick)
}

/// Responsive `choice_row` with optional leading 14pt icons per option.
pub fn choice_row_for_icons<S: 'static>(
    k: &Kit,
    compact: bool,
    options: &[(String, String, Option<&'static Icon>)],
    current: Option<&str>,
    placeholder: Option<&str>,
    on_pick: impl Fn(&mut S, &str, &mut Tx) + Clone + 'static,
) -> El {
    let mut r = row().w_full().gap(6.0).flex_wrap();
    if options.is_empty() {
        return r.child(
            text(placeholder.unwrap_or("Nothing to pick.").to_string())
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
    }
    let chip_h = if compact { 44.0 } else { 26.0 };
    for (value, label, opt_icon) in options {
        let on = current == Some(value.as_str());
        let fill = if on { k.c.layer_04 } else { k.c.layer_01 };
        let ink = if on { k.c.text_base } else { k.c.text_muted };
        let pick = on_pick.clone();
        let value = value.clone();
        let mut chip = row()
            .key(format!("choice-{value}"))
            .role("radio")
            .label(label.clone())
            .a11y_value(if on { "selected" } else { "not selected" })
            .min_h(chip_h)
            .min_w(if compact { 44.0 } else { 0.0 })
            .px(if compact { 14.0 } else { 10.0 })
            .py(if compact { 8.0 } else { 0.0 })
            .gap(6.0)
            .center_y()
            .radius(k.radius.md)
            .fill(fill)
            .border(
                1.0,
                if on {
                    k.c.border_strong
                } else {
                    k.c.border_muted
                },
            )
            .hover(k.c.hover)
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut S, tx: &mut Tx| pick(s, &value, tx));
        if let Some(ic) = opt_icon {
            chip = chip.child(icon(ic, 14.0).color(ink).a11y_hidden());
        }
        chip = chip.child(
            text(label.clone())
                .font(k.ty.small_medium)
                .color(ink)
                .wrap(),
        );
        r = r.child(chip);
    }
    r
}

/// Responsive segmented picker: kit `SegmentedControl` on desktop, ≥ 44 pt touch chips on compact.
pub fn segmented<S: 'static>(
    k: &Kit,
    compact: bool,
    labels: &[&str],
    selected: Option<usize>,
    on_pick: impl Fn(&mut S, usize, &mut Tx) + Clone + 'static,
) -> El {
    segmented_with_icons(k, compact, labels, &[], selected, on_pick)
}

/// Responsive segmented picker with optional leading 14pt icons per segment.
pub fn segmented_with_icons<S: 'static>(
    k: &Kit,
    compact: bool,
    labels: &[&str],
    icons: &[Option<&'static Icon>],
    selected: Option<usize>,
    on_pick: impl Fn(&mut S, usize, &mut Tx) + Clone + 'static,
) -> El {
    if !compact {
        let mut ctrl = rune_kit::SegmentedControl::new(labels, selected).fit();
        if !icons.is_empty() {
            ctrl = ctrl.icons(icons);
        }
        return ctrl.on_pick(on_pick).build(k);
    }
    let mut r = row().role("radiogroup").gap(6.0).flex_wrap();
    for (i, label) in labels.iter().enumerate() {
        let on = selected == Some(i);
        let fill = if on { k.c.layer_04 } else { k.c.layer_01 };
        let ink = if on { k.c.text_base } else { k.c.text_muted };
        let pick = on_pick.clone();
        let mut seg = row()
            .key(format!("segment{i}"))
            .role("radio")
            .label(*label)
            .a11y_value(if on { "selected" } else { "not selected" })
            .min_h(44.0)
            .min_w(44.0)
            .px(14.0)
            .py(8.0)
            .gap(6.0)
            .center()
            .radius(k.radius.md)
            .fill(fill)
            .border(
                1.0,
                if on {
                    k.c.border_strong
                } else {
                    k.c.border_muted
                },
            )
            .hover(k.c.hover)
            .cursor(rune::Cursor::Pointer)
            .on_press_tx(move |s: &mut S, tx: &mut Tx| pick(s, i, tx));
        if let Some(ic) = icons.get(i).copied().flatten() {
            seg = seg.child(icon(ic, 14.0).color(ink).a11y_hidden());
        }
        seg = seg.child(text(*label).font(k.ty.small_medium).color(ink).nowrap());
        r = r.child(seg);
    }
    r
}
