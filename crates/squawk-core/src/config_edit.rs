//! Changing one key in config.toml without disturbing the rest of it.
//!
//! The file stays the source of truth and stays the user's: comments, blank
//! lines, key order and keys squawk does not know survive. When the key is
//! only there as a commented-out default (`# max_minutes = 120`, as the
//! first-run file has it), that line is the one that gets the value, so the
//! setting lands next to its explanation instead of at the end of the table.

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::error::{Error, Result};

/// `text` with `[table] key = value` set. Errors are the TOML parser's
/// message (the file is left alone when it does not parse).
pub fn set_value(
    text: &str,
    table: &str,
    key: &str,
    value: impl Into<Value>,
) -> std::result::Result<String, String> {
    let text = uncomment_default(text, table, key);
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.message().to_string())?;
    if !doc.contains_key(table) {
        doc.insert(table, Item::Table(Table::new()));
    }
    let section = doc[table]
        .as_table_like_mut()
        .ok_or_else(|| format!("[{table}] is not a table"))?;
    let mut value: Value = value.into();
    match section.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            // Keep the spacing and any trailing comment on the line.
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        }
        None => {
            section.insert(key, Item::Value(value));
        }
    }
    Ok(doc.to_string())
}

/// Read `path`, set the key, write it back atomically (temp file + rename).
/// A missing file is created with just that key.
pub fn set_in_file(path: &Path, table: &str, key: &str, value: impl Into<Value>) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let updated = set_value(&text, table, key, value).map_err(|message| Error::Config {
        path: path.to_path_buf(),
        message,
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, updated)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// If `[table]` has no live `key` but has a commented-out `# key = …` line,
/// turn that line into a live one (its old value is replaced right after).
fn uncomment_default(text: &str, table: &str, key: &str) -> String {
    let mut current: Option<String> = None;
    let mut live = false;
    let mut commented: Option<usize> = None;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = table_header(trimmed) {
            current = Some(name);
            continue;
        }
        if current.as_deref() != Some(table) {
            continue;
        }
        if key_of(trimmed) == Some(key) {
            live = true;
        } else if commented.is_none() {
            if let Some(rest) = trimmed.strip_prefix('#') {
                if key_of(rest.trim()) == Some(key) {
                    commented = Some(i);
                }
            }
        }
    }
    match (live, commented) {
        (false, Some(i)) => {
            let mut out = String::with_capacity(text.len());
            for (j, line) in lines.iter().enumerate() {
                if j == i {
                    let indent = &line[..line.len() - line.trim_start().len()];
                    let body = line.trim_start().trim_start_matches('#').trim_start();
                    out.push_str(indent);
                    out.push_str(body);
                } else {
                    out.push_str(line);
                }
            }
            out
        }
        _ => text.to_string(),
    }
}

/// `[name]` → `name` (not `[[arrays]]`).
fn table_header(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?;
    if inner.starts_with('[') {
        return None;
    }
    let end = inner.find(']')?;
    Some(inner[..end].trim().to_string())
}

/// The bare key of a `key = value` line.
fn key_of(line: &str) -> Option<&str> {
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    let bare = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    bare.then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DEFAULT_CONFIG_TOML};

    #[test]
    fn a_commented_default_becomes_the_live_line_in_place() {
        let out = set_value(DEFAULT_CONFIG_TOML, "meeting", "max_minutes", 60).unwrap();
        assert!(out.contains("\nmax_minutes = 60\n"), "{out}");
        assert!(!out.contains("# max_minutes"), "{out}");
        // The explanation above it and every other comment survive.
        assert!(out.contains("# Stop and save after this many minutes"));
        assert!(out.contains("# chunk_secs = 30"));
        assert!(out.contains("# threads = 0"));
        // Only that line changed.
        assert_eq!(out.lines().count(), DEFAULT_CONFIG_TOML.lines().count());
        let config = Config::parse(&out).unwrap();
        assert_eq!(config.meeting.max_minutes, 60);
        assert_eq!(config.model, Config::default().model);
    }

    #[test]
    fn a_live_key_is_replaced_keeping_its_comment() {
        let text = "[meeting]\ndetect_calls = true   # ask me\nchunk_secs = 20\n";
        let out = set_value(text, "meeting", "detect_calls", false).unwrap();
        assert_eq!(
            out,
            "[meeting]\ndetect_calls = false   # ask me\nchunk_secs = 20\n"
        );
    }

    #[test]
    fn a_missing_key_is_added_to_its_table_and_other_tables_are_untouched() {
        let text = "keep_audio = true\n\n[meeting]\n# notes about meetings\nchunk_secs = 20\n\n[model]\n# mine\nthreads = 4\n";
        let out = set_value(text, "meeting", "heads_up_secs", -1).unwrap();
        let config = Config::parse(&out).unwrap();
        assert_eq!(config.meeting.heads_up_secs, -1);
        assert_eq!(config.meeting.chunk_secs, 20);
        assert_eq!(config.model.threads, 4);
        assert!(config.keep_audio);
        assert!(out.contains("# notes about meetings"));
        assert!(out.contains("[model]\n# mine\nthreads = 4\n"), "{out}");
    }

    #[test]
    fn a_missing_table_is_created() {
        let out = set_value("keep_audio = true\n", "meeting", "detect_calls", false).unwrap();
        let config = Config::parse(&out).unwrap();
        assert!(!config.meeting.detect_calls);
        assert!(config.keep_audio);
        let out = set_value("", "meeting", "max_minutes", 30).unwrap();
        assert_eq!(Config::parse(&out).unwrap().meeting.max_minutes, 30);
    }

    #[test]
    fn a_commented_key_in_another_table_is_not_touched() {
        let text = "[dictation]\n# max_secs = 600\n[meeting]\n";
        let out = set_value(text, "dictation", "max_secs", 30).unwrap();
        assert!(out.contains("max_secs = 30"));
        let out = set_value(text, "meeting", "max_secs", 30).unwrap();
        assert!(out.contains("# max_secs = 600"), "{out}");
    }

    #[test]
    fn unknown_keys_survive() {
        let text = "[meeting]\nfuture_thing = \"x\"\n";
        let out = set_value(text, "meeting", "detect_calls", false).unwrap();
        assert!(out.contains("future_thing = \"x\""));
    }

    #[test]
    fn a_file_that_does_not_parse_is_an_error() {
        assert!(set_value("keep_audio = maybe", "meeting", "detect_calls", true).is_err());
    }

    #[test]
    fn writing_a_file_keeps_it_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("squawk/config.toml");
        set_in_file(&path, "meeting", "max_minutes", 240).unwrap();
        assert_eq!(Config::try_load(&path).unwrap().meeting.max_minutes, 240);
        Config::write_default_if_missing(&path).unwrap();
        set_in_file(&path, "meeting", "detect_calls", false).unwrap();
        let config = Config::try_load(&path).unwrap();
        assert!(!config.meeting.detect_calls);
        assert_eq!(config.meeting.max_minutes, 240);
        assert!(!path.with_extension("toml.tmp").exists());
    }
}
