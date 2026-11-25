//! The CI and deploy step editors, including the `only_on` host-OS chips.
//!
//! A step is either a shell command or an act job. `only_on` empty means every host; the chips
//! write `["linux"|"windows"|"macos"]`, and a step excluded on the daemon's host is recorded as
//! skipped rather than failed.

use rune::{col, el, icon, row, text, El, TextEdit, Tx};
use rune_kit::{glyphs, Button, ButtonVariant, ControlSize, Field, Kit, TextArea};

use crate::state::{phase_key, step_path, State};
use crate::theme;
use crate::views::settings::{argv_field, list_field, table_field, text_field, u64_field};
use crate::widgets;

/// The hosts the daemon understands, in the order the chips show them.
pub const HOSTS: [(&str, &str); 3] = [
    ("linux", "Linux"),
    ("windows", "Windows"),
    ("macos", "macOS"),
];

/// One phase's list of steps, with an editor under the selected one.
pub fn phase(s: &State, k: &Kit, project: usize, deploy: bool, compact: bool) -> El {
    let e = &s.editor;
    let array = format!("project[{project}].{}", phase_key(deploy));
    let count = e.count(&array);
    let mut c = col().w_full().gap(10.0).child(widgets::section(
        k,
        if deploy { "Deploy steps" } else { "CI steps" },
        Some(if deploy {
            "Run after CI succeeds. A failing deploy step fails the run."
        } else {
            "Run in order on every new commit."
        }),
    ));

    if count == 0 {
        c = c.child(widgets::notice(k, k.c.text_faint, "No steps yet."));
    }
    for i in 0..count {
        c = c.child(step_row(s, k, project, deploy, i, compact));
        // The editor opens under its own row, so the step it edits is still on screen.
        if e.step == Some((project, deploy, i)) {
            c = c.child(step_form(s, k, project, deploy, i, compact));
        }
    }
    let phase_name = crate::state::phase_key(deploy);
    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .flex_wrap()
            .child(
                Button::new("Add shell step")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.add_step(project, deploy, false, tx)
                    })
                    .build(k)
                    .key(format!("add-shell-{phase_name}")),
            )
            .child(
                Button::new("Add act step")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.add_step(project, deploy, true, tx)
                    })
                    .build(k)
                    .key(format!("add-act-{phase_name}")),
            ),
    );

    c
}

fn step_row(s: &State, k: &Kit, project: usize, deploy: bool, i: usize, compact: bool) -> El {
    let e = &s.editor;
    let path = step_path(project, deploy, i);
    let name = e.s(&format!("{path}.name"));
    let kind = e.s(&format!("{path}.kind"));
    let is_act = kind == "act";
    let summary = if is_act {
        let workflow = e.s(&format!("{path}.workflow"));
        let job = e.s(&format!("{path}.job"));
        match (workflow.is_empty(), job.is_empty()) {
            (true, _) => "act · pick a workflow".to_string(),
            (false, true) => format!("act · {workflow}"),
            (false, false) => format!("act · {workflow} · job {job}"),
        }
    } else {
        let run = e.s(&format!("{path}.run"));
        first_line(&run)
    };
    let selected = e.step == Some((project, deploy, i));
    let invalid = e.invalid(&path);
    let only_on = host_list(e, &path);

    let mut chips = row().gap(4.0).flex_wrap();
    for (value, label) in HOSTS {
        if only_on.iter().any(|h| h == value) {
            chips = chips.child(widgets::pill(k, label, theme::status(k).running));
        }
    }

    let phase_name = crate::state::phase_key(deploy);
    let badge = el()
        .size(20.0, 20.0)
        .center()
        .radius(k.radius.xs)
        .fill(k.c.layer_04)
        .child(
            icon(
                if is_act {
                    &glyphs::TRIANGLE_DOWN
                } else {
                    &glyphs::CARET_RIGHT
                },
                12.0,
            )
            .color(k.c.icon_muted),
        );

    let blank = name.is_empty();
    let title_text = text(if blank {
        "(unnamed step)".to_string()
    } else {
        name
    })
    .font(k.ty.small_medium)
    .color(if blank { k.c.text_faint } else { k.c.text_base });
    let summary_text = text(summary).font(k.ty.mono).color(k.c.text_muted);
    let info = col()
        .gap(2.0)
        .grow(1.0)
        .min_w(0.0)
        .child(if compact {
            title_text.wrap()
        } else {
            title_text.nowrap().truncate()
        })
        .child(if compact {
            summary_text.wrap()
        } else {
            summary_text.nowrap().truncate()
        });

    let actions = row()
        .gap(if compact { 4.0 } else { 2.0 })
        .child(widgets::compact_icon(
            rune_kit::IconButton::new(&glyphs::CHEVRON_UP)
                .label("Move up")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    s.move_step(project, deploy, i, false, tx)
                })
                .build(k)
                .key(format!("step-up-{phase_name}-{i}")),
            compact,
        ))
        .child(widgets::compact_icon(
            rune_kit::IconButton::new(&glyphs::CHEVRON_DOWN)
                .label("Move down")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    s.move_step(project, deploy, i, true, tx)
                })
                .build(k)
                .key(format!("step-down-{phase_name}-{i}")),
            compact,
        ))
        .child(widgets::compact_icon(
            rune_kit::IconButton::new(&glyphs::CLOSE)
                .label("Remove step")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    s.remove_step(project, deploy, i, tx)
                })
                .build(k)
                .key(format!("step-remove-{phase_name}-{i}")),
            compact,
        ));

    let border_color = if invalid {
        theme::status(k).failure
    } else {
        k.c.border_muted
    };
    let fill_color = if selected { k.c.layer_03 } else { k.c.layer_01 };

    if compact {
        return col()
            .key(format!("step-row-{phase_name}-{i}"))
            .w_full()
            .min_h(44.0)
            .padding(10.0)
            .gap(8.0)
            .radius(k.radius.md)
            .fill(fill_color)
            .border(1.0, border_color)
            .hover(k.c.hover)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.select_step(project, deploy, i, tx))
            .cursor(rune::Cursor::Pointer)
            .child(row().w_full().gap(10.0).center_y().child(badge).child(info))
            .child(
                row()
                    .w_full()
                    .gap(6.0)
                    .center_y()
                    .flex_wrap()
                    .children((!only_on.is_empty()).then_some(chips))
                    .child(el().grow(1.0))
                    .child(actions),
            );
    }

    row()
        .key(format!("step-row-{phase_name}-{i}"))
        .w_full()
        .padding(10.0)
        .gap(10.0)
        .center_y()
        .radius(k.radius.md)
        .fill(fill_color)
        .border(1.0, border_color)
        .hover(k.c.hover)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.select_step(project, deploy, i, tx))
        .cursor(rune::Cursor::Pointer)
        .child(badge)
        .child(info)
        .children((!only_on.is_empty()).then_some(chips))
        .child(actions)
}

fn first_line(text_value: &str) -> String {
    text_value.lines().next().unwrap_or("").trim().to_string()
}

/// The hosts a step is limited to, as path values.
fn host_list(e: &crate::state::Editor, path: &str) -> Vec<String> {
    crate::paths::array(&e.draft, &format!("{path}.only_on"))
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect()
}

fn step_form(s: &State, k: &Kit, project: usize, deploy: bool, i: usize, compact: bool) -> El {
    let path = step_path(project, deploy, i);
    let e = &s.editor;
    let kind = e.s(&format!("{path}.kind"));
    let is_act = kind == "act";

    let mut c = col()
        .key("step-form")
        .w_full()
        .padding(14.0)
        .gap(16.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base);

    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .flex_wrap()
            .child(text("Step").font(k.ty.ui_medium).color(k.c.text_base))
            .child(widgets::pill(
                k,
                if is_act { "act" } else { "shell" },
                k.c.text_muted,
            ))
            .child(el().grow(1.0))
            .child(
                Button::new("Done")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| {
                        s.editor.step = None;
                        tx.instant();
                    })
                    .build(k)
                    .key("step-done"),
            ),
    );

    c = c.child(text_field(
        s,
        k,
        &format!("{path}.name"),
        "Name",
        Some("Shown in run history."),
    ));

    // The kind decides the rest of the form.
    let kind_path = path.clone();
    c = c.child(widgets::labelled(
        k,
        "Kind",
        Some("A shell step runs a command on this machine. An act step runs a GitHub Actions job in Docker."),
        widgets::segmented(
            k,
            compact,
            &["Shell", "act (GitHub Actions)"],
            Some(if is_act { 1 } else { 0 }),
            move |s: &mut State, picked: usize, tx: &mut Tx| {
                let want = if picked == 1 { "act" } else { "shell" };
                s.editor.set_str(&format!("{kind_path}.kind"), want);
                if want == "act" {
                    if s.editor.s(&format!("{kind_path}.event")).is_empty() {
                        s.editor.set_str(&format!("{kind_path}.event"), "push");
                    }
                    if s.editor.s(&format!("{kind_path}.workflow")).is_empty() {
                        let repo_project = s.editor.s(&format!("project[{project}].name"));
                        let repo_path = s.editor.s(&format!("project[{project}].repo_path"));
                        s.request_workflows(
                            &kind_path,
                            repo_project,
                            (!repo_path.trim().is_empty()).then_some(repo_path),
                        );
                    }
                }
                tx.instant();
            },
        )
        .key("step-kind"),
    ));

    if is_act {
        c = c.child(act_fields(s, k, project, &path, compact));
    } else {
        c = c.child(shell_fields(s, k, &path));
    }

    // Shared by both kinds.
    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(
        k,
        "Host OS",
        Some("Leave all off to run on every host. A step excluded here is recorded as skipped, not failed."),
    ));
    c = c.child(only_on_chips(s, k, project, deploy, i, &path, compact));

    c = c.child(host_note(s, k, &path));
    c = c.child(widgets::divider(k));
    c = c.child(widgets::pair_for(
        compact,
        u64_field(
            s,
            k,
            &format!("{path}.timeout"),
            "Timeout (seconds)",
            Some("Kill the step after this long. Empty waits forever."),
            false,
        ),
        text_field(
            s,
            k,
            &format!("{path}.cwd"),
            "Working directory",
            Some("Relative to the checkout. Empty uses its root."),
        ),
    ));
    if !is_act {
        c = c.child(argv_field(
            s,
            k,
            &format!("{path}.shell"),
            "Shell",
            Some("Overrides the global shell for this step, e.g. `bash -lc`."),
        ));
    }
    c = c.child(table_field(
        s,
        k,
        &format!("{path}.env"),
        "Environment",
        Some("`KEY=value`, one per line."),
    ));

    if is_act {
        c = c.child(widgets::divider(k));
        c = c.child(widgets::section(
            k,
            "act options",
            Some("Images, secrets and extra flags for this job."),
        ));
        c = c.child(table_field(
            s,
            k,
            &format!("{path}.platforms"),
            "Platform images",
            Some("`runs-on=image`, one per line. Empty uses the global mapping."),
        ));
        c = c.child(list_field(
            s,
            k,
            &format!("{path}.args"),
            "Extra arguments",
            Some("One per line, e.g. `--pull=false`."),
        ));
        c = c.child(secrets_table(s, k, &format!("{path}.secrets")));
        c = c.child(table_field(
            s,
            k,
            &format!("{path}.vars"),
            "Variables",
            Some("`KEY=value`, one per line. Passed as GitHub Actions variables."),
        ));
    }

    c
}

fn shell_fields(s: &State, k: &Kit, path: &str) -> El {
    let seeded = s.editor.s(&format!("{path}.run"));
    let edit = s
        .editor
        .text
        .get(&format!("{path}.run"))
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let owner = format!("{path}.run");
    let field = owner.clone();
    let invalid = s.editor.invalid(&owner);
    let error = s.editor.error_for(&owner).map(str::to_string);
    let input = TextArea::new(&edit)
        .label("Command")
        .width(None)
        .rows(3)
        .auto_grow(crate::theme::area_min_rows(k), 14)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = s.editor.s(&field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            s.editor.set_str(&owner, &text);
            let now_seeded = s.editor.s(&owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new()
        .label("Command")
        .prefix("Run with the configured shell. Output is captured per step.")
        .control(input.key("control"));
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k).key(format!("field-{path}.run"))
}

fn act_fields(s: &State, k: &Kit, project: usize, path: &str, compact: bool) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(16.0);

    // Workflow picker: the repo's own files once loaded, plus whatever is typed.
    let workflows = &e.workflows;
    let current_workflow = e.s(&format!("{path}.workflow"));
    let mut files: Vec<String> = workflows.iter().map(|w| w.file.clone()).collect();
    if !current_workflow.is_empty() && !files.contains(&current_workflow) {
        files.insert(0, current_workflow.clone());
    }
    if files.is_empty() {
        files.push(".github/workflows/ci.yml".to_string());
    }
    let workflow_owner = format!("{path}.workflow");
    let job_owner = format!("{path}.job");
    let project_name = e.s(&format!("project[{project}].name"));
    let project_repo = e.s(&format!("project[{project}].repo_path"));
    let key = path.to_string();

    let workflow_pairs: Vec<(String, String)> = files
        .iter()
        .map(|f| (f.clone(), short_workflow(f)))
        .collect();
    let workflow_field = workflow_owner.clone();
    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .flex_wrap()
            .child(
                text("Workflow".to_string())
                    .font(k.ty.small_medium)
                    .color(k.c.text_base),
            )
            .child(el().grow(1.0))
            .child(
                Button::new(if e.workflows_loading {
                    "Reading…"
                } else {
                    "Refresh"
                })
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .disabled(e.workflows_loading)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    s.request_workflows(
                        &key,
                        project_name.clone(),
                        (!project_repo.trim().is_empty()).then_some(project_repo.clone()),
                    );
                    tx.instant();
                })
                .build(k)
                .key("workflows-refresh"),
            ),
    );
    c = c.child(
        widgets::choice_row_for(
            k,
            compact,
            &workflow_pairs,
            Some(current_workflow.as_str()),
            None,
            move |s: &mut State, picked: &str, tx: &mut Tx| {
                s.editor.set_str(&workflow_field, picked);
                tx.instant();
            },
        )
        .key("step-workflows"),
    );
    c = c.child(text_field(
        s,
        k,
        &format!("{path}.workflow"),
        "Workflow path",
        Some("`.github/workflows/ci.yml`. Picked above, or typed if the repo has no checkout yet."),
    ));

    // Jobs of the chosen workflow.
    let jobs: Vec<(String, String)> = workflows
        .iter()
        .find(|w| w.file == current_workflow)
        .map(|w| {
            w.jobs
                .iter()
                .map(|j| (j.id.clone(), j.name.clone()))
                .collect()
        })
        .unwrap_or_default();
    let current_job = e.s(&format!("{path}.job"));
    if jobs.is_empty() {
        c = c.child(text_field(
            s,
            k,
            &format!("{path}.job"),
            "Job",
            Some("The workflow's job id. Empty runs every job."),
        ));
    } else {
        let mut pairs: Vec<(String, String)> = jobs
            .iter()
            .map(|(id, name)| {
                (
                    id.clone(),
                    if name.is_empty() || name == id {
                        id.clone()
                    } else {
                        format!("{id} · {name}")
                    },
                )
            })
            .collect();
        pairs.insert(0, (String::new(), "Every job".to_string()));
        let pick_path = job_owner.clone();
        c = c.child(widgets::labelled(
            k,
            "Job",
            Some("Runs one job. Empty runs every job in the workflow."),
            widgets::choice_row_for(
                k,
                compact,
                &pairs,
                Some(current_job.as_str()),
                Some("Every job"),
                move |s: &mut State, picked: &str, tx: &mut Tx| {
                    s.editor.set_str(&pick_path, picked);
                    tx.instant();
                },
            )
            .key("step-jobs"),
        ));
    }

    c = c.child(text_field(
        s,
        k,
        &format!("{path}.event"),
        "Event",
        Some("`push`, or the event act should simulate."),
    ));
    c
}

fn secrets_table(s: &State, k: &Kit, path: &str) -> El {
    let seeded = crate::paths::get_table_text(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let owner = path.to_string();
    let field = path.to_string();
    let invalid = s.editor.invalid(path);
    let input = TextArea::new(&edit)
        .label("Secrets")
        .width(None)
        .rows(3)
        .auto_grow(crate::theme::area_min_rows(k), 8)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_table_text(&s.editor.draft, &field);
            s.editor.text_for(&field, seeded)
        })
        .on_change(move |s: &mut State, tx: &mut Tx| {
            let text = s
                .editor
                .text
                .get(&owner)
                .map(|e| e.text.clone())
                .unwrap_or_default();
            crate::paths::set_table(&mut s.editor.draft, &owner, &text);
            let now_seeded = crate::paths::get_table_text(&s.editor.draft, &owner);
            s.editor.sync_seeded(&owner, now_seeded);
            tx.instant();
        })
        .build(k);
    let mut f = Field::new()
        .label("Secrets")
        .prefix(
            "`KEY=value`, one per line. A stored value shows as `********`; leave it to keep it.",
        )
        .control(input.key("control"));
    if let Some(error) = s.editor.error_for(path) {
        f = f.suffix(error.to_string());
    }
    f.build(k).key(format!("field-{path}"))
}

// ---------------------------------------------------------------------------------------------
// only_on

/// The OS chips: three toggles over `only_on`. None selected means every host.
fn only_on_chips(
    s: &State,
    k: &Kit,
    project: usize,
    deploy: bool,
    index: usize,
    path: &str,
    compact: bool,
) -> El {
    let selected = host_list(&s.editor, path);
    let mut row_el = row().w_full().gap(8.0).flex_wrap();
    for (value, label) in HOSTS {
        let on = selected.iter().any(|h| h == value);
        let ink = if on { k.c.text_inverse } else { k.c.text_muted };
        let fill = if on {
            theme::status(k).running
        } else {
            k.c.layer_03
        };
        let only_on_path = format!("{path}.only_on");
        let value_owned = value.to_string();
        row_el = row_el.child(
            el().key(format!("only-on-{value}"))
                .role("checkbox")
                .label(label)
                .a11y_value(if on { "checked" } else { "unchecked" })
                .min_h(if compact { 44.0 } else { 26.0 })
                .min_w(if compact { 44.0 } else { 0.0 })
                .px(if compact { 14.0 } else { 11.0 })
                .gap(6.0)
                .center_y()
                .radius(k.radius.md)
                .fill(fill)
                .border(1.0, if on { fill } else { k.c.border_base })
                .hover(k.c.hover)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                    let list_path = only_on_path.clone();
                    let host = value_owned.clone();
                    toggle_host(&mut s.editor, &list_path, &host);
                    let _ = (project, deploy, index);
                    tx.instant();
                })
                .cursor(rune::Cursor::Pointer)
                .child(
                    icon(
                        if on {
                            &glyphs::CHECK_SMALL
                        } else {
                            &glyphs::DASH
                        },
                        12.0,
                    )
                    .color(if on { k.c.text_inverse } else { k.c.text_faint }),
                )
                .child(text(label).font(k.ty.small_medium).color(ink)),
        );
    }
    row_el = row_el.child(
        row().gap(6.0).center_y().pl(4.0).child(
            text(if selected.is_empty() {
                "Every host"
            } else {
                ""
            })
            .font(k.ty.small)
            .color(k.c.text_faint),
        ),
    );
    row_el
}

fn toggle_host(e: &mut crate::state::Editor, path: &str, host: &str) {
    let mut list = host_list(e, path);
    match list.iter().position(|h| h == host) {
        Some(i) => {
            list.remove(i);
        }
        None => {
            // Keep the canonical order, so the array is stable across toggles.
            list.push(host.to_string());
            let order = |h: &String| HOSTS.iter().position(|(v, _)| v == h).unwrap_or(usize::MAX);
            list.sort_by_key(order);
        }
    }
    if list.is_empty() {
        crate::paths::set(&mut e.draft, path, serde_json::Value::Null);
    } else {
        let values: Vec<serde_json::Value> =
            list.into_iter().map(serde_json::Value::String).collect();
        crate::paths::set(&mut e.draft, path, serde_json::Value::Array(values));
    }
}

/// What the chips mean for the daemon that will run this config.
fn host_note(s: &State, k: &Kit, path: &str) -> El {
    let selected = host_list(&s.editor, path);
    let host = s.system.as_ref().map(|x| x.os.clone());
    match (&host, selected.is_empty()) {
        (_, true) => text("Runs on every host.")
            .font(k.ty.small)
            .color(k.c.text_faint),
        (Some(host), false) if selected.iter().any(|h| h == host) => text(format!(
            "Runs on this daemon's host ({}).",
            widgets::os_name(host)
        ))
        .font(k.ty.small)
        .color(theme::status(k).success),
        (Some(host), false) => text(format!(
            "This daemon runs on {}, so the step is recorded as skipped here.",
            widgets::os_name(host)
        ))
        .font(k.ty.small)
        .color(k.c.warning.fg),
        (None, false) => text("Runs only on the hosts selected above.")
            .font(k.ty.small)
            .color(k.c.text_faint),
    }
}

/// `.github/workflows/ci.yml` shown as `ci.yml`, with the directory as the tooltip's hint.
fn short_workflow(file: &str) -> String {
    file.rsplit('/').next().unwrap_or(file).to_string()
}
