//! AS-12: apply known-field edits to the original document, retaining future fields.
//!
//! Deserialize the original with the writer's schema, then patch only differences
//! between that projection and the edited value. Dynamic map deletions remain deletions;
//! omitted future fields never become deletions. TOML keeps untouched syntax and comments;
//! JSON retains values (whitespace is normalized). Callers still own validation, locking,
//! conflict checks and atomic replacement. Never use this for signed policy documents.

use std::io;
use std::path::Path;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value as Json;
use toml::Value as Toml;
use toml_edit::{DocumentMut, Item};

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Render a TOML edit against the original schema projection, preserving unknown data.
pub fn edit_toml<T: Serialize + DeserializeOwned>(
    original: &str,
    updated: &T,
) -> io::Result<String> {
    edit_toml_aliases(original, updated, &[])
}

/// Rename legacy aliases only when their canonical field is edited, avoiding duplicate fields.
pub fn edit_toml_aliases<T: Serialize + DeserializeOwned>(
    original: &str,
    updated: &T,
    aliases: &[(&str, &str)],
) -> io::Result<String> {
    let baseline: T = toml::from_str(original).map_err(invalid)?;
    let before = Toml::try_from(baseline).map_err(invalid)?;
    let after = Toml::try_from(updated).map_err(invalid)?;
    let mut document: DocumentMut = original.parse().map_err(invalid)?;
    for &(alias, canonical) in aliases {
        if before.get(canonical) != after.get(canonical)
            && let Some((mut key, item)) = document.as_table_mut().remove_entry(alias)
        {
            // Keep the original value's comments while replacing the field spelling.
            key = toml_edit::Key::new(canonical).with_leaf_decor(key.leaf_decor().clone());
            document.as_table_mut().insert_formatted(&key, item);
        }
    }
    patch_toml(document.as_item_mut(), &before, &after)?;
    let output = document.to_string();
    let roundtrip: T = toml::from_str(&output).map_err(invalid)?;
    if toml::to_string(&roundtrip).map_err(invalid)? != toml::to_string(updated).map_err(invalid)? {
        return Err(invalid("TOML edit does not match the requested values"));
    }
    Ok(output)
}

/// Read an existing TOML file before editing it; only absence starts a new document.
pub fn save_toml<T: Serialize + DeserializeOwned>(path: &Path, updated: &T) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(original) => edit_toml(&original, updated),
        Err(e) if e.kind() == io::ErrorKind::NotFound => toml::to_string(updated).map_err(invalid),
        Err(e) => Err(e),
    }
}

/// Render JSON without discarding unknown root or nested fields.
pub fn edit_json<T: Serialize + DeserializeOwned>(
    original: &str,
    updated: &T,
) -> io::Result<String> {
    let baseline: T = serde_json::from_str(original).map_err(invalid)?;
    let before = serde_json::to_value(baseline).map_err(invalid)?;
    let after = serde_json::to_value(updated).map_err(invalid)?;
    let mut document: Json = serde_json::from_str(original).map_err(invalid)?;
    patch_json(&mut document, &before, &after);
    serde_json::to_string_pretty(&document).map_err(invalid)
}

/// Empty orchestration files historically mean an empty store; retain that convention.
pub fn save_json<T: Serialize + DeserializeOwned>(path: &Path, updated: &T) -> io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(original) if !original.trim().is_empty() => edit_json(&original, updated),
        Ok(_) => serde_json::to_string_pretty(updated).map_err(invalid),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            serde_json::to_string_pretty(updated).map_err(invalid)
        }
        Err(e) => Err(e),
    }
}

// Reserve identity/equality matches before positional fallback: a renamed command
// must not steal a reordered unchanged command's future data. IDs never fall back
// to position, because a replacement account/run is a different entity. Account
// auto-switch rules use their (provider, from_account_id) composite identity.
fn matches(before: &[Json], after: &[Json]) -> Vec<Option<usize>> {
    let mut used = std::collections::BTreeSet::new();
    let mut indices: Vec<_> = after
        .iter()
        .map(|next| {
            let found = before
                .iter()
                .enumerate()
                .find(|(index, old)| {
                    if used.contains(index) {
                        return false;
                    }
                    if let Some(id) = next.get("id") {
                        return old.get("id") == Some(id);
                    }
                    if let Some(account) = next.get("from_account_id") {
                        return old.get("from_account_id") == Some(account)
                            && old.get("provider") == next.get("provider");
                    }
                    if let Some(name) = next.get("name") {
                        return old.get("name") == Some(name);
                    }
                    *old == next
                })
                .map(|(index, _)| index);
            found.filter(|i| used.insert(*i))
        })
        .collect();
    if before.len() == after.len() {
        for (index, (next, found)) in after.iter().zip(&mut indices).enumerate() {
            if found.is_none() && next.get("id").is_none() && next.get("from_account_id").is_none()
            {
                let candidate = if used.contains(&index) {
                    (0..before.len()).find(|i| !used.contains(i))
                } else {
                    Some(index)
                };
                *found = candidate.filter(|i| used.insert(*i));
            }
        }
    }
    indices
}

fn patch_json(raw: &mut Json, before: &Json, after: &Json) {
    if before == after {
        return;
    }
    match (raw, before, after) {
        (Json::Object(raw), Json::Object(before), Json::Object(after)) => {
            for key in before.keys().filter(|key| !after.contains_key(*key)) {
                raw.remove(key);
            }
            for (key, next) in after {
                if let (Some(old), Some(entry)) = (before.get(key), raw.get_mut(key)) {
                    patch_json(entry, old, next);
                } else if before.get(key) != Some(next) {
                    raw.insert(key.clone(), next.clone());
                }
            }
        }
        (Json::Array(raw), Json::Array(before), Json::Array(after)) => {
            let indices = matches(before, after);
            *raw = after
                .iter()
                .zip(indices)
                .map(|(next, index)| {
                    if let Some((old, entry)) = index.and_then(|i| before.get(i).zip(raw.get(i))) {
                        let mut entry = entry.clone();
                        patch_json(&mut entry, old, next);
                        entry
                    } else {
                        next.clone()
                    }
                })
                .collect();
        }
        (raw, _, after) => *raw = after.clone(),
    }
}

fn new_item(value: &Toml) -> io::Result<Item> {
    let mut table = toml::Table::new();
    table.insert("value".into(), value.clone());
    let text = toml::to_string(&table).map_err(invalid)?;
    let mut doc: DocumentMut = text.parse().map_err(invalid)?;
    doc.as_table_mut()
        .remove("value")
        .ok_or_else(|| invalid("missing serialized value"))
}

// Parsed table positions otherwise override the new AoT order during rendering.
fn reset_positions(table: &mut toml_edit::Table) {
    table.set_position(None);
    for (_, item) in table.iter_mut() {
        if let Some(table) = item.as_table_mut() {
            reset_positions(table);
        }
        if let Some(tables) = item.as_array_of_tables_mut() {
            for table in tables.iter_mut() {
                reset_positions(table);
            }
        }
    }
}

fn patch_toml(raw: &mut Item, before: &Toml, after: &Toml) -> io::Result<()> {
    if before == after {
        return Ok(());
    }
    if let (Some(raw), Toml::Table(before), Toml::Table(after)) =
        (raw.as_table_like_mut(), before, after)
    {
        for key in before.keys().filter(|key| !after.contains_key(*key)) {
            raw.remove(key);
        }
        for (key, next) in after {
            if let (Some(old), Some(entry)) = (before.get(key), raw.get_mut(key)) {
                patch_toml(entry, old, next)?;
            } else if before.get(key) != Some(next) {
                raw.insert(key, new_item(next)?);
            }
        }
        return Ok(());
    }
    if let (Toml::Array(before), Toml::Array(after)) = (before, after) {
        let before_json = before
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        let after_json = after
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        let indices = matches(&before_json, &after_json);
        let mut entries = Vec::new();
        for (next, index) in after.iter().zip(indices) {
            let old = index.and_then(|i| {
                let entry = if let Some(tables) = raw.as_array_of_tables() {
                    tables.get(i).cloned().map(Item::Table)
                } else {
                    raw.as_array()?.get(i).cloned().map(Item::Value)
                };
                before.get(i).zip(entry)
            });
            let entry = if let Some((old, mut entry)) = old {
                patch_toml(&mut entry, old, next)?;
                entry
            } else {
                new_item(next)?
            };
            entries.push(entry);
        }
        if let Some(tables) = raw.as_array_of_tables_mut() {
            tables.clear();
            for entry in entries {
                let mut table = entry.into_table().map_err(|_| invalid("expected table"))?;
                reset_positions(&mut table);
                tables.push(table);
            }
        } else {
            let mut array = raw.as_array().cloned().unwrap_or_default();
            array.clear();
            for entry in entries {
                array.push_formatted(entry.into_value().map_err(|_| invalid("expected value"))?);
            }
            *raw = Item::Value(toml_edit::Value::Array(array));
        }
        return Ok(());
    }
    let mut next = new_item(after)?;
    if let (Some(old), Some(new)) = (raw.as_value(), next.as_value_mut()) {
        *new.decor_mut() = old.decor().clone();
    }
    *raw = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize)]
    struct Entry {
        id: String,
        label: String,
    }
    #[derive(Debug, Serialize, Deserialize)]
    struct File {
        entries: Vec<Entry>,
    }

    #[test]
    fn identified_arrays_reorder_delete_and_add_without_moving_future_data() {
        let raw = "# keep\n[[entries]]\nid = 'one'\nlabel = 'old'\nfuture = 'first'\n[[entries]]\nid = 'two'\nlabel = 'old'\nfuture = 'second'\n[[entries]]\nid = 'remove'\nlabel = 'old'\nfuture = 'removed'\n";
        let updated = File {
            entries: vec![
                Entry {
                    id: "two".into(),
                    label: "edited".into(),
                },
                Entry {
                    id: "one".into(),
                    label: "old".into(),
                },
                Entry {
                    id: "new".into(),
                    label: "new".into(),
                },
            ],
        };
        let output = edit_toml(raw, &updated).unwrap();
        let value: Toml = toml::from_str(&output).unwrap();
        let entries = value.get("entries").unwrap().as_array().unwrap();
        assert_eq!(
            entries.first().unwrap().get("future").unwrap().as_str(),
            Some("second")
        );
        assert_eq!(
            entries.get(1).unwrap().get("future").unwrap().as_str(),
            Some("first")
        );
        assert!(entries.get(2).unwrap().get("future").is_none());
        assert!(!output.contains("removed"));
    }

    #[test]
    fn inline_arrays_keep_future_values_and_map_deletions() {
        let raw = "entries = [{ id = 'one', label = 'old', future = { keep = true } }] # retain\n";
        let mut file: File = toml::from_str(raw).unwrap();
        file.entries.first_mut().unwrap().label = "edited".into();
        let output = edit_toml(raw, &file).unwrap();
        let value: Toml = toml::from_str(&output).unwrap();
        assert!(output.contains("# retain"));
        assert!(
            value
                .get("entries")
                .unwrap()
                .as_array()
                .unwrap()
                .first()
                .unwrap()
                .get("future")
                .is_some()
        );
    }

    #[test]
    fn aliases_do_not_become_duplicate_canonical_fields() {
        #[derive(Serialize, Deserialize)]
        struct Legacy {
            #[serde(alias = "old")]
            current: bool,
        }
        let output = edit_toml_aliases(
            "old = false # keep\nfuture = 42\n",
            &Legacy { current: true },
            &[("old", "current")],
        )
        .unwrap();
        assert!(toml::from_str::<Legacy>(&output).unwrap().current);
        assert!(output.contains("# keep"));
        assert!(output.contains("future = 42"));
        assert!(!output.contains("old ="));
    }

    proptest::proptest! {
        #[test]
        fn unknown_json_values_survive_edits(future in proptest::prelude::any::<i64>()) {
            let raw = serde_json::json!({"entries": [{"id": "one", "label": "old", "future": future}], "future": {"nested": future}}).to_string();
            let mut file: File = serde_json::from_str(&raw).unwrap();
            file.entries.first_mut().unwrap().label = "edited".into();
            let output = edit_json(&raw, &file).unwrap();
            let value: Json = serde_json::from_str(&output).unwrap();
            proptest::prop_assert_eq!(value.get("future").unwrap().get("nested").unwrap(), &Json::from(future));
            proptest::prop_assert_eq!(value.get("entries").unwrap().as_array().unwrap().first().unwrap().get("future").unwrap(), &Json::from(future));
        }
    }
}
