//! The CI and deploy step editors, including the `only_on` host-OS chips.
//!
//! A step is either a shell command or an act job. `only_on` empty means every host; the chips
//! write `["linux"|"windows"|"macos"]`, and a step excluded on the daemon's host is recorded as
//! skipped rather than failed.

use rune::{col, el, icon, row, text, El, TextEdit, Tx};
use rune_kit::{glyphs, Button, ButtonVariant, ControlSize, Field, Kit, TextArea};

use crate::state::{phase_key, step_path, State};
use crate::theme;
use crate::widgets;

/// The hosts the daemon understands, in the order the chips show them.
pub const HOSTS: [(&str, &str); 3] = [
    ("linux", "Linux"),
    ("windows", "Windows"),
    ("macos", "macOS"),
];

/// One phase's list of steps, with an editor under the selected one.
pub fn phase(s: &State, k: &Kit, project: usize, deploy: bool) -> El {
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
        c = c.child(step_row(s, k, project, deploy, i));
        // The editor opens under its own row, so the step it edits is still on screen.
        if e.step == Some((project, deploy, i)) {
            c = c.child(step_form(s, k, project, deploy, i));
        }
    }
    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .child(
                Button::new("Add shell step")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.add_step(project, deploy, false, tx)
                    })
                    .build(k),
            )
            .child(
                Button::new("Add act step")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.add_step(project, deploy, true, tx)
                    })
                    .build(k),
            ),
    );

    c
}

fn step_row(s: &State, k: &Kit, project: usize, deploy: bool, i: usize) -> El {
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

    let mut chips = row().gap(4.0);
    for (value, label) in HOSTS {
        if only_on.iter().any(|h| h == value) {
            chips = chips.child(widgets::pill(k, label, theme::status(k).running));
        }
    }

    row()
        .key(format!("step-row-{i}"))
        .w_full()
        .padding(10.0)
        .gap(10.0)
        .center_y()
        .radius(k.radius.md)
        .fill(if selected { k.c.layer_03 } else { k.c.layer_01 })
        .border(
            1.0,
            if invalid {
                theme::status(k).failure
            } else {
                k.c.border_muted
            },
        )
        .hover(k.c.hover)
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.select_step(project, deploy, i, tx))
        .cursor(rune::Cursor::Pointer)
        .child(
            el().size(20.0, 20.0)
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
                ),
        )
        .child(
            col()
                .gap(2.0)
                .grow(1.0)
                .child({
                    let blank = name.is_empty();
                    text(if blank {
                        "(unnamed step)".to_string()
                    } else {
                        name
                    })
                    .font(k.ty.small_medium)
                    .color(if blank { k.c.text_faint } else { k.c.text_base })
                    .nowrap()
                    .truncate()
                })
                .child(
                    text(summary)
                        .font(k.ty.mono)
                        .color(k.c.text_muted)
                        .nowrap()
                        .truncate(),
                ),
        )
        .children((!only_on.is_empty()).then_some(chips))
        .child(
            row()
                .gap(2.0)
                .child(
                    rune_kit::IconButton::new(&glyphs::CHEVRON_UP)
                        .label("Move up")
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                            s.move_step(project, deploy, i, false, tx)
                        })
                        .build(k),
                )
                .child(
                    rune_kit::IconButton::new(&glyphs::CHEVRON_DOWN)
                        .label("Move down")
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                            s.move_step(project, deploy, i, true, tx)
                        })
                        .build(k),
                )
                .child(
                    rune_kit::IconButton::new(&glyphs::CLOSE)
                        .label("Remove step")
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                            s.remove_step(project, deploy, i, tx)
                        })
                        .build(k),
                ),
        )
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

fn step_form(s: &State, k: &Kit, project: usize, deploy: bool, i: usize) -> El {
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
                    .build(k),
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
        rune_kit::SegmentedControl::new(&["Shell", "act (GitHub Actions)"], Some(if is_act { 1 } else { 0 }))
            .fit()
            .on_pick(move |s: &mut State, picked: usize, tx: &mut Tx| {
                let want = if picked == 1 { "act" } else { "shell" };
                s.editor.set_str(&format!("{kind_path}.kind"), want);
                if want == "act" {
                    if s.editor.s(&format!("{kind_path}.event")).is_empty() {
                        s.editor.set_str(&format!("{kind_path}.event"), "push");
                    }
                    if s.editor.s(&format!("{kind_path}.workflow")).is_empty() {
                        let repo_project = s.editor.s(&format!("project[{project}].name"));
                        s.request_workflows(&kind_path, repo_project, None);
                    }
                }
                tx.instant();
            })
            .build(k),
    ));

    if is_act {
        c = c.child(act_fields(s, k, project, &path));
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
    c = c.child(only_on_chips(s, k, project, deploy, i, &path));

    c = c.child(host_note(s, k, &path));
    c = c.child(widgets::divider(k));
    c = c.child(widgets::pair(
        text_field(
            s,
            k,
            &format!("{path}.timeout"),
            "Timeout (seconds)",
            Some("Kill the step after this long. Empty waits forever."),
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
        c = c.child(text_field(
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
            Some("`runs-on: image`, one per line. Empty uses the global mapping."),
        ));
        c = c.child(text_field(
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
        .auto_grow(2, 14)
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
            tx.instant();
        })
        .build(k);
    let mut f = Field::new()
        .label("Command")
        .prefix("Run with the configured shell. Output is captured per step.")
        .control(input);
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k)
}

fn act_fields(s: &State, k: &Kit, project: usize, path: &str) -> El {
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
                    s.request_workflows(&key, project_name.clone(), None);
                    tx.instant();
                })
                .build(k),
            ),
    );
    c = c.child(widgets::choice_row(
        k,
        &workflow_pairs,
        Some(current_workflow.as_str()),
        None,
        move |s: &mut State, picked: &str, tx: &mut Tx| {
            s.editor.set_str(&workflow_field, picked);
            tx.instant();
        },
    ));
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
            widgets::choice_row(
                k,
                &pairs,
                Some(current_job.as_str()),
                Some("Every job"),
                move |s: &mut State, picked: &str, tx: &mut Tx| {
                    s.editor.set_str(&pick_path, picked);
                    tx.instant();
                },
            ),
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
        .auto_grow(2, 8)
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
            tx.instant();
        })
        .build(k);
    let mut f = Field::new()
        .label("Secrets")
        .prefix(
            "`KEY=value`, one per line. A stored value shows as `********`; leave it to keep it.",
        )
        .control(input);
    if let Some(error) = s.editor.error_for(path) {
        f = f.suffix(error.to_string());
    }
    f.build(k)
}

fn text_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_str(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let invalid = s.editor.invalid(path);
    let error = s.editor.error_for(path).map(str::to_string);
    let owner = path.to_string();
    let field = path.to_string();
    let input = rune_kit::TextInput::new(&edit)
        .label(label)
        .placeholder(label)
        .width(None)
        .invalid(invalid)
        .on_edit(move |s: &mut State| {
            let seeded = crate::paths::get_str(&s.editor.draft, &field);
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
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input);
    if let Some(help) = help {
        f = f.prefix(help);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k)
}

fn table_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_table_text(&s.editor.draft, path);
    let edit = s
        .editor
        .text
        .get(path)
        .cloned()
        .unwrap_or_else(|| TextEdit::new(seeded.clone()));
    let owner = path.to_string();
    let field = path.to_string();
    let input = TextArea::new(&edit)
        .label(label)
        .width(None)
        .rows(2)
        .auto_grow(2, 8)
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
            tx.instant();
        })
        .build(k);
    let mut f = Field::new().label(label).control(input);
    if let Some(help) = help {
        f = f.prefix(help);
    }
    f.build(k)
}

// ---------------------------------------------------------------------------------------------
// only_on

/// The OS chips: three toggles over `only_on`. None selected means every host.
fn only_on_chips(s: &State, k: &Kit, project: usize, deploy: bool, index: usize, path: &str) -> El {
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
                .h(26.0)
                .px(11.0)
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
