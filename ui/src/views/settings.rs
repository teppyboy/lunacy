//! The settings shell: a sidebar of pages, the page body, and the save bar.
//!
//! The daemon's config is edited as the JSON it already is (`Editor::draft`), so unknown keys
//! survive a save and a rejected field is addressed by the daemon's own path.

use rune::{col, el, row, text, Cx, El, ScrollSpec, TextEdit, Tx};
use rune_kit::{Button, ButtonVariant, ControlSize, Field, Kit, TextArea, TextInput};

use crate::state::{SettingsPage, State};
use crate::theme;
use crate::views::{diagnostics, steps};
use crate::widgets;

pub fn view(s: &State, k: &Kit, page: SettingsPage, cx: &mut Cx<State>, hair: f32) -> El {
    let mut pages = col()
        .w(212.0)
        .shrink()
        .h_full()
        .gap(1.0)
        .py(10.0)
        .fill(k.c.layer_01)
        .border_sides([0.0, 0.0, 0.0, hair], k.c.border_muted);
    pages = pages.child(
        row()
            .w_full()
            .px(16.0)
            .pb(8.0)
            .child(widgets::caption(k, "CONFIG")),
    );
    for p in SettingsPage::CONFIG {
        pages = pages.child(page_row(k, s, p));
    }
    pages = pages.child(
        row()
            .w_full()
            .px(16.0)
            .pt(14.0)
            .pb(8.0)
            .child(widgets::caption(k, "THIS WINDOW")),
    );
    for p in [SettingsPage::Diagnostics, SettingsPage::Connection] {
        pages = pages.child(page_row(k, s, p));
    }

    col()
        .key("settings")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(save_bar(s, k, page, hair))
        .child(
            row().w_full().grow(1.0).min_h(0.0).child(pages).child(
                col()
                    .key("settings-body")
                    .grow(1.0)
                    .min_h(0.0)
                    .h_full()
                    .scroll(ScrollSpec::default())
                    .child(match page {
                        SettingsPage::Diagnostics => diagnostics::view(s, k, cx),
                        SettingsPage::Connection => connection_page(s, k),
                        SettingsPage::Projects if s.editor.project.is_some() => {
                            projects_detail(s, k, cx)
                        }
                        SettingsPage::Projects => projects_list(s, k),
                        other => config_page(s, k, other, cx),
                    }),
            ),
        )
}

fn page_row(k: &Kit, s: &State, page: SettingsPage) -> El {
    let selected = s.editor.page == page;
    row()
        .key(format!("settings-nav-{}", page.title()))
        .w_full()
        .h(30.0)
        .px(16.0)
        .center_y()
        .fill(if selected {
            k.c.layer_03
        } else {
            rune::Color::TRANSPARENT
        })
        .hover(k.c.hover)
        .child(text(page.title()).font(k.ty.ui).color(if selected {
            k.c.text_base
        } else {
            k.c.text_muted
        }))
        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_settings(page, tx))
        .cursor(rune::Cursor::Pointer)
}

/// The header with dirty state, Save, Discard and the ⌘S hint.
fn save_bar(s: &State, k: &Kit, page: SettingsPage, hair: f32) -> El {
    let e = &s.editor;
    let local = matches!(page, SettingsPage::Connection | SettingsPage::Diagnostics);
    let dirty = e.dirty();
    let mut bar = row()
        .key("settings-bar")
        .w_full()
        .px(22.0)
        .py(12.0)
        .gap(10.0)
        .center_y()
        .border_sides([0.0, 0.0, hair, 0.0], k.c.border_muted)
        .child(
            col()
                .gap(1.0)
                .grow(1.0)
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(text(page.title()).font(k.ty.ui_medium).color(k.c.text_base))
                        .children(
                            dirty.then(|| widgets::pill(k, "Unsaved", theme::status(k).error)),
                        ),
                )
                .child(
                    text(page.blurb())
                        .font(k.ty.small)
                        .color(k.c.text_muted)
                        .nowrap()
                        .truncate(),
                ),
        );

    if !local {
        bar = bar
            .child(
                Button::new("Discard")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .disabled(!dirty)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.discard(tx))
                    .build(k),
            )
            .child(
                Button::new(if e.saving { "Saving…" } else { "Save" })
                    .variant(ButtonVariant::Submit)
                    .control_size(ControlSize::Small)
                    .disabled(!dirty || e.saving || !e.loaded)
                    .shortcut("cmd+s")
                    .show_shortcut()
                    .on_press_tx(|s: &mut State, _tx: &mut Tx| s.save())
                    .build(k),
            );
    }
    bar
}

// ---------------------------------------------------------------------------------------------
// Pages

fn config_page(s: &State, k: &Kit, page: SettingsPage, cx: &mut Cx<State>) -> El {
    let e = &s.editor;
    if !e.loaded {
        return widgets::empty_state(
            k,
            "Reading the config…",
            "The daemon has not answered yet. Check the connection in the banner.",
        );
    }
    let _ = cx;
    let _ = TextEdit::new("");
    let mut body = col()
        .key("config-page")
        .w_full()
        .px(22.0)
        .py(18.0)
        .gap(22.0);
    if let Some(field) = &e.field {
        let message = e.error.clone().unwrap_or_default();
        body = body.child(widgets::notice(
            k,
            theme::status(k).failure,
            &format!("{field}: {message}"),
        ));
    }
    body = body.child(match page {
        SettingsPage::General => general(s, k),
        SettingsPage::Server => server(s, k),
        SettingsPage::Github => github(s, k),
        SettingsPage::Discord => discord(s, k),
        SettingsPage::Act => act(s, k),
        _ => col(),
    });
    body
}

fn general(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(
        k,
        "General",
        Some("Where runs are recorded and how often upstream is checked."),
    ));
    c = c.child(text_field(
        s,
        k,
        "general.state_dir",
        "State directory",
        Some("Run history and logs live here. Empty uses the platform's data directory."),
    ));
    c = c.child(text_field(
        s,
        k,
        "general.poll_interval",
        "Poll interval (seconds)",
        Some("How often each project's remote is fetched. A project may override it."),
    ));
    c = c.child(text_field(
        s,
        k,
        "general.shell",
        "Default shell",
        Some("Command used to run shell steps, e.g. `bash -lc`. Empty uses the platform default."),
    ));
    let _ = e;
    c
}

fn server(s: &State, k: &Kit) -> El {
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(
        k,
        "Server",
        Some("The HTTP API this window talks to."),
    ));
    c = c.child(text_field(
        s,
        k,
        "server.listen",
        "Listen address",
        Some("`127.0.0.1:7878`. Omit the section entirely to disable the API."),
    ));
    c = c.child(secret_field(
        s,
        k,
        "server.token",
        "Bearer token",
        Some("If set, every /api request needs `Authorization: Bearer <token>`."),
    ));
    c = c.child(text_field(
        s,
        k,
        "server.public_url",
        "Public URL",
        Some("How this daemon is reached from elsewhere. Used for links in GitHub checks."),
    ));
    c = c.child(text_field(
        s,
        k,
        "server.web_root",
        "Web root",
        Some("Static files served at `/`, e.g. `ui/dist` from `build-web.sh`. Empty disables it."),
    ));
    c
}

fn github(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(k, "GitHub", Some("How a run is reported back to GitHub. A token is enough for commit statuses; an App gives check runs with per-step output.")));

    let mode = e.s("github.mode");
    let mode_index = if mode == "checks" {
        Some(0)
    } else if mode == "status" {
        Some(1)
    } else {
        None
    };
    c = c.child(widgets::labelled(
        k,
        "Reporting",
        Some("Check runs need an App; commit statuses work with any token that can write them."),
        rune_kit::SegmentedControl::new(&["Checks (App)", "Status (token)"], mode_index)
            .fit()
            .on_pick(|s: &mut State, picked: usize, tx: &mut Tx| {
                s.editor
                    .set_str("github.mode", if picked == 0 { "checks" } else { "status" });
                tx.instant();
            })
            .build(k),
    ));
    c = c.child(secret_field(
        s,
        k,
        "github.token",
        "Token",
        Some("A PAT with `repo` scope for commit statuses. Masked once stored."),
    ));

    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(k, "GitHub App", Some("Optional. The App's installation writes check runs, which the UI links as \"View on GitHub\".")));
    c = c.child(text_field(s, k, "github.app.app_id", "App ID", None));
    c = c.child(secret_field(
        s,
        k,
        "github.app.private_key",
        "Private key",
        Some("The multi-line PEM from the App's settings page."),
    ));
    c = c.child(text_field(
        s,
        k,
        "github.app.installation_id",
        "Installation ID",
        None,
    ));
    c = c.child(secret_field(
        s,
        k,
        "github.app.webhook_secret",
        "Webhook secret",
        Some("Verifies `POST /api/github/webhook`; a re-requested check reruns that commit."),
    ));
    c
}

fn discord(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(
        k,
        "Discord",
        Some("Post a message when a pipeline starts and when it finishes."),
    ));
    c = c.child(secret_field(
        s,
        k,
        "discord.webhook_url",
        "Webhook URL",
        Some("A channel webhook. Masked once stored; projects can override it."),
    ));
    c = c.child(toggle_row(
        k,
        "Notify on start",
        Some("Also post when a run begins, not only when it ends."),
        crate::paths::get_bool(&e.draft, "discord.notify_on_start"),
        |s: &mut State, tx: &mut Tx| {
            let on = crate::paths::get_bool(&s.editor.draft, "discord.notify_on_start");
            crate::paths::set_bool(&mut s.editor.draft, "discord.notify_on_start", !on);
            tx.instant();
        },
    ));
    c
}

fn act(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().gap(18.0);
    c = c.child(widgets::section(k, "act", Some("The runner for GitHub Actions steps: how it is invoked and which image each `runs-on` maps to.")));
    c = c.child(text_field(
        s,
        k,
        "act.command",
        "Command",
        Some("Usually just `act`. Use `wsl.exe -e act` on Windows to run it inside WSL."),
    ));
    c = c.child(text_field(
        s,
        k,
        "act.args",
        "Extra arguments",
        Some("One per line, e.g. `--pull=false`."),
    ));
    c = c.child(text_field(
        s,
        k,
        "act.platforms",
        "Platforms",
        Some("`runs-on: image`, one per line. Unlisted labels use act's own default image."),
    ));
    c = c.child(text_field(
        s,
        k,
        "act.lock_file",
        "Lock file",
        Some(
            "The `flock` every act run takes, shared with scripts that call `flock` on it. \
             Defaults to `act.lock` in the state dir; `~/` is expanded.",
        ),
    ));
    let _ = e;
    c
}

/// A text field bound to a config path, with inline error and help.
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
    let input = TextInput::new(&edit)
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
            let was_empty = text.trim().is_empty();
            s.editor.set_str(&owner, &text);
            let optional = is_optional(&owner);
            if optional && was_empty {
                crate::paths::set(&mut s.editor.draft, &owner, serde_json::Value::Null);
            }
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

/// A masked secret field: `********` means "keep what is stored".
fn secret_field(s: &State, k: &Kit, path: &str, label: &str, help: Option<&str>) -> El {
    let seeded = crate::paths::get_str(&s.editor.draft, path);
    let stored = seeded == crate::config::MASK;
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
    let input = TextInput::new(&edit)
        .label(label)
        .placeholder(if stored {
            "Stored — leave to keep"
        } else {
            label
        })
        .width(None)
        .secure()
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
    let _ = stored;
    let mut f = Field::new().label(label).control(input);
    let note = match (stored, help) {
        (true, Some(h)) => format!(
            "{h} A stored secret shows as `********`; leave it to keep it, or type a new one."
        ),
        (true, None) => {
            "A stored secret shows as `********`; leave it to keep it, or type a new one."
                .to_string()
        }
        (false, Some(h)) => h.to_string(),
        (false, None) => String::new(),
    };
    if !note.is_empty() {
        f = f.prefix(note);
    }
    if let Some(error) = error {
        f = f.suffix(error);
    }
    f.build(k)
}

/// Optional config fields: an empty value is removed rather than stored as `""`.
fn is_optional(path: &str) -> bool {
    matches!(
        path,
        "general.state_dir"
            | "general.shell"
            | "server.public_url"
            | "server.web_root"
            | "server.token"
            | "github.mode"
            | "github.token"
            | "github.app.private_key"
            | "github.app.webhook_secret"
            | "github.app.installation_id"
            | "github.app.app_id"
            | "discord.webhook_url"
            | "act.command"
            | "act.lock_file"
    )
}

fn toggle_row(
    k: &Kit,
    label: &str,
    help: Option<&str>,
    on: bool,
    flip: fn(&mut State, &mut Tx),
) -> El {
    let mut col_body = col().gap(2.0).grow(1.0).child(
        text(label.to_string())
            .font(k.ty.small_medium)
            .color(k.c.text_base),
    );
    if let Some(help) = help {
        col_body = col_body.child(
            text(help.to_string())
                .font(k.ty.small)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    row()
        .w_full()
        .min_h(34.0)
        .gap(12.0)
        .center_y()
        .child(col_body)
        .child(
            rune_kit::Switch::new(on)
                .label(label)
                .hide_label()
                .on_toggle_tx(flip)
                .build(k),
        )
}

// ---------------------------------------------------------------------------------------------
// Projects

fn projects_list(s: &State, k: &Kit) -> El {
    let e = &s.editor;
    let mut c = col().w_full().px(22.0).py(18.0).gap(12.0);
    c = c.child(widgets::section(
        k,
        "Projects",
        Some(
            "Repositories the daemon watches, in poll order. Each has its own CI and deploy steps.",
        ),
    ));
    if e.count("project") == 0 {
        c = c.child(widgets::notice(
            k,
            k.c.text_faint,
            "No projects yet. Add one below.",
        ));
    }
    for i in 0..e.count("project") {
        c = c.child(project_card(s, k, i));
    }
    c = c.child(
        row().w_full().gap(8.0).child(
            Button::new("Add project")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Regular)
                .on_press_tx(|s: &mut State, tx: &mut Tx| s.add_project(tx))
                .build(k),
        ),
    );
    c
}

fn project_card(s: &State, k: &Kit, i: usize) -> El {
    let e = &s.editor;
    let name = e.s(&format!("project[{i}].name"));
    let branch = e.s(&format!("project[{i}].branch"));
    let ci = e.count(&format!("project[{i}].ci"));
    let deploy = e.count(&format!("project[{i}].deploy"));
    let invalid = e.invalid(&format!("project[{i}].name"));
    let card = row()
        .key(format!("project-card-{i}"))
        .w_full()
        .padding(12.0)
        .gap(10.0)
        .center_y()
        .radius(k.radius.md)
        .fill(k.c.layer_02)
        .border(
            1.0,
            if invalid {
                theme::status(k).failure
            } else {
                k.c.border_muted
            },
        )
        .child(
            col()
                .gap(2.0)
                .grow(1.0)
                .child(
                    text(if name.is_empty() {
                        "(unnamed)".to_string()
                    } else {
                        name.clone()
                    })
                    .font(k.ty.small_medium)
                    .color(if name.is_empty() {
                        k.c.text_faint
                    } else {
                        k.c.text_base
                    }),
                )
                .child(
                    text(format!("{branch} · {ci} CI · {deploy} deploy"))
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        )
        .child(up_down(k, i))
        .child(
            Button::new("Edit")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.open_project_form(Some(i), tx))
                .build(k),
        );
    let _ = card;
    card
}

fn up_down(k: &Kit, i: usize) -> El {
    row()
        .gap(2.0)
        .child(
            rune_kit::IconButton::new(&rune_kit::glyphs::CHEVRON_UP)
                .label("Move up")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.move_project(i, false, tx))
                .build(k),
        )
        .child(
            rune_kit::IconButton::new(&rune_kit::glyphs::CHEVRON_DOWN)
                .label("Move down")
                .control_size(ControlSize::Small)
                .on_press_tx(move |s: &mut State, tx: &mut Tx| s.move_project(i, true, tx))
                .build(k),
        )
}

/// One project's form: identity, then its CI and deploy steps.
fn projects_detail(s: &State, k: &Kit, cx: &mut Cx<State>) -> El {
    let _ = cx;
    let e = &s.editor;
    let Some(i) = e.project else {
        return projects_list(s, k);
    };
    let base = format!("project[{i}]");
    let mut c = col()
        .key("project-form")
        .w_full()
        .px(22.0)
        .py(18.0)
        .gap(20.0);

    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(
                Button::new("All projects")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.open_project_form(None, tx))
                    .build(k),
            )
            .child(el().grow(1.0))
            .child(
                Button::new("Duplicate")
                    .variant(ButtonVariant::Ghost)
                    .control_size(ControlSize::Small)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| s.duplicate_project(i, tx))
                    .build(k),
            )
            .child(
                Button::new("Delete")
                    .variant(ButtonVariant::Danger)
                    .control_size(ControlSize::Small)
                    .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                        s.editor.confirm_delete = Some(i);
                        tx.instant();
                    })
                    .build(k),
            ),
    );

    if e.confirm_delete == Some(i) {
        let label = e.s(&format!("{base}.name"));
        c = c.child(
            row()
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .center_y()
                .radius(k.radius.md)
                .fill(theme::status(k).tint(theme::status(k).failure))
                .child(
                    text(format!(
                        "Delete {label} and its steps? The config is only written on Save."
                    ))
                    .font(k.ty.small)
                    .color(theme::status(k).failure)
                    .grow(1.0),
                )
                .child(
                    Button::new("Cancel")
                        .variant(ButtonVariant::Ghost)
                        .control_size(ControlSize::Small)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.editor.confirm_delete = None;
                            tx.instant();
                        })
                        .build(k),
                )
                .child(
                    Button::new("Delete")
                        .variant(ButtonVariant::Danger)
                        .control_size(ControlSize::Small)
                        .on_press_tx(move |s: &mut State, tx: &mut Tx| s.delete_project(i, tx))
                        .build(k),
                ),
        );
    }

    c = c.child(widgets::section(k, "Repository", None));
    c = c.child(widgets::pair(
        text_field(
            s,
            k,
            &format!("{base}.name"),
            "Name",
            Some("Unique; used in URLs and on disk."),
        ),
        text_field(
            s,
            k,
            &format!("{base}.branch"),
            "Branch",
            Some("The branch a run is built from."),
        ),
    ));
    c = c.child(text_field(
        s,
        k,
        &format!("{base}.repo_path"),
        "Repository path",
        Some("The local clone the daemon fetches into and builds from."),
    ));
    c = c.child(widgets::pair(
        text_field(
            s,
            k,
            &format!("{base}.remote"),
            "Remote",
            Some("Usually `origin`."),
        ),
        text_field(
            s,
            k,
            &format!("{base}.github_repo"),
            "GitHub repository",
            Some("`owner/name`, for commit links and checks."),
        ),
    ));
    c = c.child(widgets::pair(
        select_field(
            s,
            k,
            &format!("{base}.checkout"),
            "Checkout",
            &[
                ("none", "None (build in the repo)"),
                ("worktree", "Worktree per run"),
                ("in-place", "Reset in place"),
            ],
        ),
        text_field(
            s,
            k,
            &format!("{base}.poll_interval"),
            "Poll interval (seconds)",
            Some("Empty uses the global interval."),
        ),
    ));
    c = c.child(secret_field(
        s,
        k,
        &format!("{base}.discord_webhook_url"),
        "Discord webhook",
        Some("Overrides the global webhook for this project."),
    ));
    c = c.child(table_field(
        s,
        k,
        &format!("{base}.env"),
        "Environment",
        Some("`KEY=value`, one per line, for every step in this project."),
    ));

    c = c.child(widgets::divider(k));
    c = c.child(steps::phase(s, k, i, false));
    c = c.child(widgets::divider(k));
    c = c.child(steps::phase(s, k, i, true));
    c
}

/// A choice row bound to a config path.
fn select_field(s: &State, k: &Kit, path: &str, label: &str, options: &[(&str, &str)]) -> El {
    let current = crate::paths::get_str(&s.editor.draft, path);
    let pairs: Vec<(String, String)> = options
        .iter()
        .map(|(v, l)| (v.to_string(), l.to_string()))
        .collect();
    let owner = path.to_string();
    let pick = widgets::choice_row(
        k,
        &pairs,
        Some(current.as_str()),
        None,
        move |s: &mut State, picked: &str, tx: &mut Tx| {
            s.editor.set_str(&owner, picked);
            tx.instant();
        },
    );
    widgets::labelled(k, label, None, pick)
}

/// A `KEY=value`-per-line table bound to a config path.
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
        .rows(3)
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
// Connection (this window, not the daemon)

fn connection_page(s: &State, k: &Kit) -> El {
    let mut c = col().w_full().px(22.0).py(18.0).gap(18.0);
    c = c.child(widgets::section(k, "Connection", Some("Where this window looks for the daemon. Not written to config.toml; in a browser the token is remembered for this site.")));
    c = c.child(widgets::pair(
        text_input_bound(k, "Server URL", &s.ui_server, |s: &mut State| {
            &mut s.ui_server
        }),
        text_input_bound(k, "Token", &s.ui_token, |s: &mut State| &mut s.ui_token),
    ));
    c = c.child(
        row()
            .w_full()
            .gap(8.0)
            .child(
                Button::new("Connect")
                    .variant(ButtonVariant::Submit)
                    .control_size(ControlSize::Regular)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| s.reconnect(tx))
                    .build(k),
            )
            .children(
                (cfg!(target_arch = "wasm32")).then_some(
                    Button::new("Use this page's origin")
                        .variant(ButtonVariant::Outline)
                        .control_size(ControlSize::Regular)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.ui_server = TextEdit::new(crate::state::default_server());
                            tx.instant();
                        })
                        .build(k),
                ),
            ),
    );
    c = c.child(widgets::divider(k));
    c = c.child(widgets::section(k, "Status", None));
    c = c.child(widgets::kv(
        k,
        "Server",
        widgets::value(k, &s.server_display()),
    ));
    c = c.child(widgets::kv(
        k,
        "State",
        match &s.link {
            crate::state::Link::Connecting => widgets::pill(k, "Connecting", k.c.text_muted),
            crate::state::Link::Ok => widgets::pill(k, "Connected", theme::status(k).success),
            crate::state::Link::Down { failures, .. } => {
                widgets::pill(k, format!("Offline ({failures})"), theme::status(k).failure)
            }
            crate::state::Link::Unauthorized => {
                widgets::pill(k, "Unauthorized", theme::status(k).error)
            }
        },
    ));
    c = c.child(widgets::kv(
        k,
        "Daemon version",
        widgets::value(
            k,
            if s.version.is_empty() {
                "—"
            } else {
                &s.version
            },
        ),
    ));
    c = c.child(widgets::kv(
        k,
        "Launch flags",
        widgets::value(k, "--server URL --token T, or LUNACY_SERVER / LUNACY_TOKEN"),
    ));
    c
}

fn text_input_bound(
    k: &Kit,
    label: &str,
    edit: &TextEdit,
    access: fn(&mut State) -> &mut TextEdit,
) -> El {
    let input = TextInput::new(edit)
        .label(label)
        .placeholder(label)
        .width(None)
        .on_edit(access)
        .build(k);
    Field::new().label(label).control(input).build(k)
}
