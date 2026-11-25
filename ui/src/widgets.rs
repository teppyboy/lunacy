//! Small pieces every screen draws: status dots and pills, section cards, key/value rows,
//! timestamps, durations, and the log body.

use rune::{col, el, icon, row, text, Align, Color, El, Tx};
use rune_kit::{Badge, Kit};

use crate::model::{ProjectState, RunStatus, StepStatus};
use crate::theme::{self, Status};

/// A status dot: a filled circle in the status ink.
pub fn dot(ink: Color, size: f32) -> El {
    el().size(size, size).radius(size * 0.5).fill(ink)
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
        .child(text(label).font(k.ty.small_medium).color(ink))
}

/// A run's status pill.
pub fn run_pill(k: &Kit, status: RunStatus) -> El {
    let s = theme::status(k);
    let (label, ink) = match status {
        RunStatus::Running => ("Running", s.running),
        RunStatus::Success => ("Success", s.success),
        RunStatus::Failure => ("Failed", s.failure),
        RunStatus::Error => ("Error", s.error),
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
        ProjectState::Idle => ("Idle", s.pending),
    };
    pill(k, label, ink)
}

/// Always-visible draggable scrollbar styling for scrollable panels.
pub fn scroll_bar(viewport: El, k: &Kit) -> El {
    use rune_kit::ScrollBar;
    viewport.scroll_bar_styled(rune_kit::ScrollBarStyle::from_kit(k).always_visible())
}

/// A card: a rounded, bordered surface with an optional title and body.
pub fn card(k: &Kit) -> El {
    col()
        .w_full()
        .gap(0.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_01)
        .border(theme::hair(1.0), k.c.border_base)
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

/// A simple 3-line hamburger icon for the compact navigation drawer toggle.
pub fn hamburger(ink: Color) -> El {
    let bar = || el().w(16.0).h(1.5).radius(0.75).fill(ink);
    col()
        .gap(3.5)
        .center()
        .a11y_hidden()
        .child(bar())
        .child(bar())
        .child(bar())
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

/// A single-line read-only value, for diagnostics.
pub fn value(k: &Kit, v: &str) -> El {
    value_for(k, false, v)
}

/// Read-only diagnostic value: single-line truncated on desktop, wrapped on compact viewports.
pub fn value_for(k: &Kit, compact: bool, v: &str) -> El {
    let t = text(v.to_string()).font(k.ty.mono).color(k.c.text_base);
    if compact {
        t.wrap()
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
                .color(k.c.text_muted),
        )
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

/// A failure banner inside a panel: the message, and optionally a retry button.
pub fn notice(k: &Kit, ink: Color, message: &str) -> El {
    row()
        .w_full()
        .gap(8.0)
        .padding(10.0)
        .radius(k.radius.md)
        .fill(theme::status(k).tint(ink))
        .items_start()
        .child(dot(ink, 7.0).mt(5.0))
        .child(text(message.to_string()).font(k.ty.small).color(ink).wrap())
}

/// A badge whose label the caller reads.
pub fn badge(k: &Kit, label: impl Into<String>) -> El {
    Badge::new(label).standard().build(k)
}

/// The log body: monospace, selectable, no wrapping surprises, one line per row.
pub fn log_body(k: &Kit, text: &str) -> El {
    let style = k.ty.mono;
    col().w_full().gap(0.0).px(12.0).py(8.0).child(
        rune::text(text.to_string())
            .font(style)
            .color(k.c.text_base)
            .nowrap()
            .selectable(true)
            .elide_head(),
    )
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
    let mut r = row().w_full().gap(6.0).flex_wrap();
    if options.is_empty() {
        return r.child(
            text(placeholder.unwrap_or("Nothing to pick.").to_string())
                .font(k.ty.small)
                .color(k.c.text_faint),
        );
    }
    let chip_h = if compact { 44.0 } else { 26.0 };
    for (value, label) in options {
        let on = current == Some(value.as_str());
        let fill = if on { k.c.layer_04 } else { k.c.layer_01 };
        let pick = on_pick.clone();
        let value = value.clone();
        r = r.child(
            el().key(format!("choice-{value}"))
                .role("radio")
                .label(label.clone())
                .a11y_value(if on { "selected" } else { "not selected" })
                .min_h(chip_h)
                .min_w(if compact { 44.0 } else { 0.0 })
                .px(if compact { 14.0 } else { 10.0 })
                .py(if compact { 8.0 } else { 0.0 })
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
                .on_press_tx(move |s: &mut S, tx: &mut Tx| pick(s, &value, tx))
                .child(
                    text(label.clone())
                        .font(k.ty.small_medium)
                        .color(if on { k.c.text_base } else { k.c.text_muted })
                        .wrap(),
                ),
        );
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
    if !compact {
        return rune_kit::SegmentedControl::new(labels, selected)
            .fit()
            .on_pick(on_pick)
            .build(k);
    }
    let mut r = row().role("radiogroup").gap(6.0).flex_wrap();
    for (i, label) in labels.iter().enumerate() {
        let on = selected == Some(i);
        let fill = if on { k.c.layer_04 } else { k.c.layer_01 };
        let pick = on_pick.clone();
        r = r.child(
            el().key(format!("segment{i}"))
                .role("radio")
                .label(*label)
                .a11y_value(if on { "selected" } else { "not selected" })
                .min_h(44.0)
                .min_w(44.0)
                .px(14.0)
                .py(8.0)
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
                .on_press_tx(move |s: &mut S, tx: &mut Tx| pick(s, i, tx))
                .child(text(*label).font(k.ty.small_medium).color(if on {
                    k.c.text_base
                } else {
                    k.c.text_muted
                })),
        );
    }
    r
}
