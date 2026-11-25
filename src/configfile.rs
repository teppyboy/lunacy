//! Writing `config.toml` back from the API while keeping comments where possible.

use crate::config::Config;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Table};

/// Render `cfg` over `existing` text.
///
/// `toml_edit` keeps the existing formatting for keys that survive; the result
/// is then re-parsed as a plain document so the output is valid TOML.
pub fn render(existing: &str, cfg: &Config) -> Result<String> {
    let mut doc: DocumentMut = if existing.trim().is_empty() {
        DocumentMut::new()
    } else {
        existing.parse().unwrap_or_else(|_| DocumentMut::new())
    };

    let fresh: DocumentMut = toml::to_string(cfg)?.parse()?;

    // Clone the old top-level items first so the document is not borrowed
    // while it is mutated.
    let mut old_items: BTreeMap<String, Item> = BTreeMap::new();
    for (key, item) in doc.iter() {
        old_items.insert(key.to_string(), item.clone());
    }

    // Sections that are plain tables: replace the content, keep the comments
    // that live on the table header and on surviving keys.
    for key in ["general", "server", "github", "discord", "act"] {
        match (old_items.get_mut(key), fresh.get(key)) {
            (Some(_), None) => {
                doc.remove(key);
            }
            (Some(old_item), Some(new_item)) => {
                let mut merged = new_item.clone();
                merge_table(old_item, &mut merged);
                doc.insert(key, merged);
            }
            (None, Some(new)) => {
                doc.insert(key, new.clone());
            }
            (None, None) => {}
        }
    }

    // Projects: match by name so comments on existing entries survive
    // reordering, and new projects append.
    let old_projects: Vec<Table> = old_items
        .remove("project")
        .and_then(|i| i.as_array_of_tables().cloned())
        .map(|a| a.iter().cloned().collect())
        .unwrap_or_default();

    let mut new_projects = toml_edit::ArrayOfTables::new();
    if let Some(tables) = fresh.get("project").and_then(|i| i.as_array_of_tables()) {
        for table in tables.iter() {
            let name = table
                .get("name")
                .and_then(|i| i.as_str())
                .unwrap_or_default()
                .to_string();
            let mut merged = table.clone();
            if let Some(old_table) = old_projects
                .iter()
                .find(|t| t.get("name").and_then(|i| i.as_str()) == Some(name.as_str()))
            {
                let mut old_item = Item::Table(old_table.clone());
                let mut new_item = Item::Table(table.clone());
                merge_table(&mut old_item, &mut new_item);
                if let Item::Table(t) = new_item {
                    merged = t;
                }
            }
            new_projects.push(merged);
        }
    }
    if new_projects.is_empty() {
        doc.remove("project");
    } else {
        doc["project"] = Item::ArrayOfTables(new_projects);
    }

    Ok(doc.to_string())
}

/// Move comments from `old` onto `new` where the keys still exist.
fn merge_table(old: &mut Item, new: &mut Item) {
    if let (Some(old_table), Some(new_table)) = (old.as_table_mut(), new.as_table_mut()) {
        new_table.set_implicit(old_table.is_implicit());
        if let Some(prefix) = old_table.decor().prefix().and_then(|r| r.as_str()) {
            new_table.decor_mut().set_prefix(prefix);
        }
        if let Some(suffix) = old_table.decor().suffix().and_then(|r| r.as_str()) {
            new_table.decor_mut().set_suffix(suffix);
        }
        let keys: Vec<String> = old_table.iter().map(|(k, _)| k.to_string()).collect();
        for key in keys {
            if new_table.contains_key(&key) {
                // The comment above a key lives on the *key's* leaf decor, and
                // trailing comments on the value's suffix.
                let old_key_decor = old_table
                    .get_key_value(&key)
                    .map(|(k, _)| k.leaf_decor().clone());
                if let Some(decor) = old_key_decor {
                    if let Some((mut new_key, _)) = new_table.get_key_value_mut(&key) {
                        *new_key.leaf_decor_mut() = decor;
                    }
                }
                let old_item = old_table.remove(&key);
                if let (Some(mut old_item), Some(new_item)) = (old_item, new_table.get_mut(&key)) {
                    if old_item.is_table() && new_item.is_table() {
                        merge_table(&mut old_item, new_item);
                    } else if old_item.is_array_of_tables() && new_item.is_array_of_tables() {
                        merge_array_of_tables(&mut old_item, new_item);
                    } else {
                        copy_decor(&mut old_item, new_item);
                    }
                }
            }
        }
    }
}

/// Copy table decor (and, by name, per-entry comments) for `[[x]]` lists. The
/// entries are positional, so they are matched by the `name` key when present.
fn merge_array_of_tables(old: &mut Item, new: &mut Item) {
    let (Some(old_tables), Some(new_tables)) =
        (old.as_array_of_tables(), new.as_array_of_tables_mut())
    else {
        return;
    };
    let olds: Vec<Table> = old_tables.iter().cloned().collect();
    for new_table in new_tables.iter_mut() {
        let name = new_table
            .get("name")
            .and_then(|i| i.as_str())
            .map(str::to_string);
        let Some(old_table) = olds.iter().find(|t| match &name {
            Some(n) => t.get("name").and_then(|i| i.as_str()) == Some(n.as_str()),
            // No `name` key: only a single-entry list can be matched safely.
            None => olds.len() == 1,
        }) else {
            continue;
        };
        if let Some(prefix) = old_table.decor().prefix().and_then(|r| r.as_str()) {
            new_table.decor_mut().set_prefix(prefix);
        }
        let mut old_item = Item::Table(old_table.clone());
        let mut new_item = Item::Table(new_table.clone());
        merge_table(&mut old_item, &mut new_item);
        if let Item::Table(merged) = new_item {
            *new_table = merged;
        }
    }
}

/// Copy the trailing comment and inline whitespace of one value onto its replacement.
fn copy_decor(old: &mut Item, new: &mut Item) {
    if let (Some(old_value), Some(new_value)) = (old.as_value_mut(), new.as_value_mut()) {
        let suffix = old_value
            .decor()
            .suffix()
            .and_then(|r| r.as_str())
            .map(str::to_string);
        if let Some(suffix) = suffix {
            new_value.decor_mut().set_suffix(suffix);
        }
    }
}

/// Render then re-parse, for the round-trip test.
#[cfg(test)]
pub fn render_and_parse(existing: &str, cfg: &Config) -> Result<Config> {
    let text = render(existing, cfg)?;
    Ok(toml::from_str(&text)?)
}

/// Write `text` to `path` atomically, keeping the previous content as `.bak`.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    if path.is_file() {
        let backup = path.with_extension("toml.bak");
        std::fs::copy(path, &backup)
            .with_context(|| format!("backing up to {}", backup.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn comments_survive_a_write() {
        let existing = r#"# lunacy config
[general]
# how often to poll
poll_interval = 300

[[project]]
name = "a"
repo_path = "/tmp/a"
# the CI steps
[[project.ci]]
name = "fmt"
run = "true"
timeout = 60
"#;
        let mut cfg = Config::load(Path::new("config.example.toml")).expect("example");
        cfg.projects.truncate(1);
        cfg.projects[0].name = "a".into();
        cfg.projects[0].repo_path = "/tmp/a".into();
        cfg.projects[0].ci.truncate(1);
        cfg.projects[0].ci[0].name = "fmt".into();
        cfg.projects[0].ci[0].run = Some("true".into());
        cfg.projects[0].ci[0].timeout = Some(60);
        cfg.projects[0].deploy.clear();
        cfg.general.poll_interval = 300;

        let text = render(existing, &cfg).unwrap();
        assert!(text.contains("# lunacy config"), "{text}");
        assert!(text.contains("# how often to poll"), "{text}");
        assert!(text.contains("# the CI steps"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.projects[0].ci[0].name, "fmt");
    }

    #[test]
    fn new_sections_are_appended() {
        let existing = "[general]\npoll_interval = 60\n";
        let mut cfg = Config::load(Path::new("config.example.toml")).expect("example");
        cfg.general.poll_interval = 60;
        cfg.server = Some(crate::config::ServerCfg {
            listen: "127.0.0.1:9999".into(),
            token: "t".into(),
            token_file: None,
            web_root: None,
            public_url: None,
        });
        let text = render(existing, &cfg).unwrap();
        // The config wins for values; the file only supplies formatting.
        assert!(text.contains("poll_interval = 60"), "{text}");
        assert!(text.contains("127.0.0.1:9999"), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.server.unwrap().listen, "127.0.0.1:9999");
    }

    #[test]
    fn the_config_value_wins_over_the_stale_file() {
        let existing = "[general]\npoll_interval = 60\nlog_level = \"debug\"\n";
        let mut cfg = Config::load(Path::new("config.example.toml")).expect("example");
        cfg.general.poll_interval = 120;
        let text = render(existing, &cfg).unwrap();
        assert!(text.contains("poll_interval = 120"), "{text}");
        assert!(!text.contains("poll_interval = 60"), "{text}");
    }

    #[test]
    fn write_is_atomic_with_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old = 1\n").unwrap();
        write_atomic(&path, "new = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new = 2\n");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.bak")).unwrap(),
            "old = 1\n"
        );
    }

    #[test]
    fn roundtrip_of_the_example_config() {
        let existing = std::fs::read_to_string("config.example.toml").unwrap();
        let mut cfg = Config::load(Path::new("config.example.toml")).unwrap();
        cfg.general.poll_interval = 120;
        let back = render_and_parse(&existing, &cfg).unwrap();
        assert_eq!(back.general.poll_interval, 120);
        assert_eq!(back.projects.len(), cfg.projects.len());
    }
}
