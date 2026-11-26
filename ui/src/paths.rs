//! Dotted paths into a `serde_json::Value`: `server.listen`, `project[0].ci[1].run`.
//!
//! The config editor keeps the daemon's JSON verbatim as its draft, so a field is addressed by
//! the same path `PUT /api/config` reports in its `field` on a validation error.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Name(String),
    Index(usize),
}

pub fn parse(path: &str) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let (digits, tail) = match after.find(']') {
                Some(end) => (&after[..end], &after[end + 1..]),
                None => (after, ""),
            };
            keys.push(Key::Index(digits.parse().unwrap_or(0)));
            rest = tail.strip_prefix('.').unwrap_or(tail);
            continue;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        keys.push(Key::Name(rest[..end].to_string()));
        rest = rest[end..].strip_prefix('.').unwrap_or(&rest[end..]);
    }
    keys
}

pub fn get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for key in parse(path) {
        cur = match key {
            Key::Name(n) => cur.get(&n)?,
            Key::Index(i) => cur.get(i)?,
        };
    }
    Some(cur)
}

/// Sets `path`, creating objects and arrays on the way. A `Null` value removes the key, so
/// optional fields can be cleared without inventing an empty string.
pub fn set(v: &mut Value, path: &str, value: Value) {
    if value.is_null() && get(v, path).is_none() {
        return;
    }
    let keys = parse(path);
    let Some((last, prefix)) = keys.split_last() else {
        return;
    };
    let mut cur = v;
    for (idx, key) in prefix.iter().enumerate() {
        let next_is_index = matches!(prefix.get(idx + 1).unwrap_or(last), Key::Index(_));
        cur = match key {
            Key::Name(n) => {
                let entry = cur
                    .as_object_mut()
                    .map(|o| o.entry(n.clone()).or_insert(Value::Null));
                let Some(entry) = entry else { return };
                if !entry.is_object() && !entry.is_array() {
                    *entry = if next_is_index {
                        Value::Array(Vec::new())
                    } else {
                        Value::Object(Map::new())
                    };
                }
                entry
            }
            Key::Index(i) => {
                let Some(arr) = cur.as_array_mut() else {
                    return;
                };
                while arr.len() <= *i {
                    arr.push(Value::Null);
                }
                let slot = &mut arr[*i];
                if !slot.is_object() && !slot.is_array() {
                    *slot = if next_is_index {
                        Value::Array(Vec::new())
                    } else {
                        Value::Object(Map::new())
                    };
                }
                slot
            }
        };
    }
    match last {
        Key::Name(n) => {
            if let Some(obj) = cur.as_object_mut() {
                if value.is_null() {
                    obj.remove(n);
                } else {
                    obj.insert(n.clone(), value);
                }
            }
        }
        Key::Index(i) => {
            if let Some(arr) = cur.as_array_mut() {
                while arr.len() <= *i {
                    arr.push(Value::Null);
                }
                arr[*i] = value;
            }
        }
    }
}

pub fn get_str(v: &Value, path: &str) -> String {
    match get(v, path) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

pub fn get_u64(v: &Value, path: &str) -> Option<u64> {
    match get(v, path)? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn get_bool(v: &Value, path: &str) -> bool {
    get_bool_or(v, path, false)
}

pub fn get_bool_or(v: &Value, path: &str, default: bool) -> bool {
    match get(v, path) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) if s == "true" => true,
        Some(Value::String(s)) if s == "false" => false,
        _ => default,
    }
}

/// Sets a string, or removes the key when `s` is empty and `optional` (so an unset optional
/// field stays unset instead of becoming `""`).
pub fn set_str(v: &mut Value, path: &str, s: &str, optional: bool) {
    if optional && s.trim().is_empty() {
        set(v, path, Value::Null);
    } else {
        set(v, path, Value::String(s.to_string()));
    }
}

pub fn set_opt_str(v: &mut Value, path: &str, s: &str) {
    set_str(v, path, s, true);
}

pub fn set_u64(v: &mut Value, path: &str, n: Option<u64>) {
    match n {
        Some(n) => set(v, path, Value::from(n)),
        None => set(v, path, Value::Null),
    }
}

/// Removes `path` if it is currently an empty JSON object (`{}`).
pub fn prune_empty_object(v: &mut Value, path: &str) {
    if get(v, path)
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty)
    {
        set(v, path, Value::Null);
    }
}

pub fn set_bool(v: &mut Value, path: &str, b: bool) {
    set(v, path, Value::Bool(b));
}

/// A `BTreeMap<String, String>` config table as a `KEY=VALUE`-per-line text, and back.
pub fn get_table_text(v: &Value, path: &str) -> String {
    let Some(Value::Object(map)) = get(v, path) else {
        return String::new();
    };
    map.iter()
        .map(|(k, v)| match v {
            Value::String(s) => format!("{k}={s}"),
            other => format!("{k}={}", other.to_string().trim_matches('"')),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn get_list_text(v: &Value, path: &str) -> String {
    let Some(Value::Array(items)) = get(v, path) else {
        return String::new();
    };
    items
        .iter()
        .map(|i| {
            i.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| i.to_string().trim_matches('"').to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The array at `path` as a slice, or an empty one.
pub fn array<'a>(v: &'a Value, path: &str) -> &'a [Value] {
    get(v, path)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn array_len(v: &Value, path: &str) -> usize {
    array(v, path).len()
}

/// The array at `path` for editing, created (and its parents repaired) when absent.
pub fn get_mut_array<'a>(v: &'a mut Value, path: &str) -> Option<&'a mut Vec<Value>> {
    ensure_array(v, path);
    let mut cur = v;
    for key in parse(path) {
        cur = match key {
            Key::Name(n) => cur.get_mut(&n)?,
            Key::Index(i) => cur.get_mut(i)?,
        };
    }
    cur.as_array_mut()
}

pub fn ensure_array(v: &mut Value, path: &str) {
    let is_array = matches!(get(v, path), Some(Value::Array(_)));
    if !is_array {
        set(v, path, Value::Array(Vec::new()));
    }
}

/// The index of the last path segment when it is `[i]`, e.g. `project[1]` → `1`.
pub fn index_of(path: &str) -> Option<usize> {
    match parse(path).into_iter().next_back() {
        Some(Key::Index(i)) => Some(i),
        _ => None,
    }
}

/// The last key's name, e.g. `project[0].ci[1].run` → `run`.
pub fn leaf(path: &str) -> &str {
    match path.rfind(['.', '[']) {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

/// Pre-order scan over `items` whose `label(v)` starts with `field`: the form marks that one
/// field invalid. `field` paths from the daemon may end in a leaf the form doesn't draw
/// (`project[0].repo_path`), so a prefix match keeps the message reachable.
pub fn same_field(field: &str, path: &str) -> bool {
    field == path
        || field.starts_with(&format!("{path}."))
        || path.starts_with(&format!("{field}."))
}

/// Writes a `KEY=VALUE`-per-line text back into a map at `path`.
pub fn set_table(v: &mut Value, path: &str, text: &str) {
    let mut map = Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once('=') {
            Some((k, val)) => {
                let k = k.trim();
                if !k.is_empty() {
                    map.insert(k.to_string(), Value::String(val.trim().to_string()));
                }
            }
            None => {
                map.insert(line.to_string(), Value::String(String::new()));
            }
        }
    }
    set(v, path, Value::Object(map));
}

/// Writes a line-per-item text back into an array at `path`, clearing it when empty.
pub fn set_list(v: &mut Value, path: &str, text: &str) {
    let items: Vec<Value> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| Value::String(l.to_string()))
        .collect();
    if items.is_empty() {
        set(v, path, Value::Null);
    } else {
        set(v, path, Value::Array(items));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_address_nested_json() {
        let mut v = json!({"server": {"listen": "127.0.0.1:1"}, "project": [{"ci": []}]});
        assert_eq!(get_str(&v, "server.listen"), "127.0.0.1:1");
        set_str(&mut v, "project[0].ci[1].run", "make", false);
        assert_eq!(get_str(&v, "project[0].ci[1].run"), "make");
        assert_eq!(array_len(&v, "project[0].ci"), 2);
        set(&mut v, "project[0].ci[1].run", Value::Null);
        assert!(get(&v, "project[0].ci[1].run").is_none());
        assert_eq!(
            parse("project[0].ci[1].run"),
            vec![
                Key::Name("project".into()),
                Key::Index(0),
                Key::Name("ci".into()),
                Key::Index(1),
                Key::Name("run".into()),
            ]
        );
        assert_eq!(index_of("project[3].ci[0]"), Some(0));
        assert_eq!(leaf("project[3].ci[0].run"), "run");
    }

    #[test]
    fn tables_become_text() {
        let v = json!({"env": {"A": "1", "B": "x y"}});
        assert_eq!(get_table_text(&v, "env"), "A=1\nB=x y");
    }

    #[test]
    fn field_paths_match_by_prefix() {
        assert!(same_field("server.listen", "server.listen"));
        assert!(same_field("project[0].ci", "project[0].ci"));
        assert!(!same_field("project[0].ci", "project[1].ci"));
    }

    #[test]
    fn null_set_skips_missing_and_prunes_empty_objects() {
        let mut v = json!({"general": {"poll_interval": 300}});
        set(&mut v, "github.app.private_key", Value::Null);
        assert!(v.get("github").is_none());

        set_str(&mut v, "github.app.private_key", "pem", true);
        assert!(v["github"]["app"]["private_key"] == "pem");
        set_str(&mut v, "github.app.private_key", "", true);
        prune_empty_object(&mut v, "github.app");
        prune_empty_object(&mut v, "github");
        assert!(v.get("github").is_none());

        // Required strings keep empty string instead of deleting key.
        set_str(&mut v, "project[0].name", "", false);
        assert_eq!(v["project"][0]["name"], "");
    }
}
