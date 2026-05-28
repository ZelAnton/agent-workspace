// ===========================================================================
// ws config - Read / write project-config settings
// ===========================================================================
//
// Generic key/value interface over `.agent-workspace.toml` in the
// current repo. Modelled loosely on `git config` (get / set / list)
// but with an explicit allow-list of keys so typos surface as errors
// instead of silently writing dead settings into the TOML.
//
// Initial supported keys (more land as we add settings):
//   - `workspace.alias`           string — override repo basename
//                                          in `$AGENT_WORKSPACE_DIR/...`
//   - `workspace.use_path_hash`   bool   — append the 6-hex
//                                          disambiguation suffix
//                                          (pre-v0.13.16 behaviour)
//
// Usage:
//   ws config set workspace.alias my-cargowise
//   ws config set workspace.use_path_hash true
//   ws config get workspace.alias
//   ws config unset workspace.alias
//   ws config list

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use toml_edit::{value, DocumentMut};

use crate::cli::{Error, Result};
use crate::vcs;

/// Filename in the source repo's root that holds project config.
/// Mirrors `Config::load_project`'s search target — we read AND write
/// to the same file.
const PROJECT_CONFIG_FILENAME: &str = ".agent-workspace.toml";

/// Allow-listed dotted keys. Anything not in this list is rejected
/// with a hint so the user doesn't accidentally write
/// `workspace.alais` (typo) into the config and have it silently
/// ignored on every load. Each entry records:
///   - the dotted key (`section.key`),
///   - the value kind for parsing,
///   - a short description for `ws config list`.
const KNOWN_KEYS: &[KnownKey] = &[
    KnownKey {
        key: "workspace.alias",
        kind: ValueKind::String,
        description: "Override the repo basename in `$AGENT_WORKSPACE_DIR/<name>/` paths.",
    },
    KnownKey {
        key: "workspace.use_path_hash",
        kind: ValueKind::Bool,
        description: "Append `-<6-hex-hash>` to the workspace dir name (pre-v0.13.16 default).",
    },
];

struct KnownKey {
    key: &'static str,
    kind: ValueKind,
    description: &'static str,
}

#[derive(Clone, Copy)]
enum ValueKind {
    String,
    Bool,
}

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Set a config value in `.agent-workspace.toml` (created if absent).
    Set {
        /// Dotted key — currently `workspace.alias` or
        /// `workspace.use_path_hash`. See `ws config list` for the
        /// authoritative roster.
        key: String,
        /// Value to assign. Strings stored as-is; booleans must be
        /// `true` / `false`.
        value: String,
    },
    /// Print the current value of a key, or "<unset>" if absent.
    Get {
        key: String,
    },
    /// Remove a key from the project config. No-op if absent.
    Unset {
        key: String,
    },
    /// Show all known keys, their descriptions, and current values.
    List,
}

pub fn run(args: ConfigArgs) -> Result<()> {
    // Anchor everything in the current repo's root, even if invoked
    // from a worktree or deeper subdirectory. Same lookup the rest
    // of the CLI uses.
    let repo_root = vcs::repo_root().map_err(|e| Error::Other(e.to_string()))?;
    let config_path = repo_root.join(PROJECT_CONFIG_FILENAME);

    match args.command {
        ConfigCommand::Set { key, value } => set(&config_path, &key, &value),
        ConfigCommand::Get { key } => get(&config_path, &key),
        ConfigCommand::Unset { key } => unset(&config_path, &key),
        ConfigCommand::List => list(&config_path),
    }
}

fn lookup_known(key: &str) -> Result<&'static KnownKey> {
    KNOWN_KEYS
        .iter()
        .find(|k| k.key == key)
        .ok_or_else(|| {
            Error::Other(format!(
                "unknown config key '{key}'.\nKnown keys: {}",
                KNOWN_KEYS
                    .iter()
                    .map(|k| k.key)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

fn set(config_path: &Path, key: &str, value_str: &str) -> Result<()> {
    let known = lookup_known(key)?;
    let mut doc = load_doc(config_path)?;

    // Each known key has the form `section.field`. We split here so
    // toml_edit can place the value inside the correct sub-table; if
    // the section doesn't exist yet we create it as an inline-empty
    // table.
    let (section, field) = key.split_once('.').ok_or_else(|| {
        Error::Other(format!(
            "config key '{key}' is malformed (expected `section.field`)"
        ))
    })?;

    if doc.get(section).is_none() {
        doc[section] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let table = doc[section].as_table_mut().ok_or_else(|| {
        Error::Other(format!("`{section}` exists in config but is not a table"))
    })?;

    match known.kind {
        ValueKind::String => {
            // `toml_edit::value` is the constructor helper that wraps
            // any `Into<Value>` impl into the right `Item` variant.
            table[field] = value(value_str);
        }
        ValueKind::Bool => {
            let b: bool = match value_str.to_ascii_lowercase().as_str() {
                "true" | "yes" | "on" | "1" => true,
                "false" | "no" | "off" | "0" => false,
                _ => {
                    return Err(Error::Other(format!(
                        "value '{value_str}' is not a valid boolean for `{key}` (try true/false)"
                    )));
                }
            };
            table[field] = value(b);
        }
    }

    save_doc(config_path, &doc)?;
    println!("Set {key} = {value_str}");
    println!("(written to {})", config_path.display());
    Ok(())
}

fn get(config_path: &Path, key: &str) -> Result<()> {
    lookup_known(key)?;
    let doc = load_doc(config_path)?;
    let (section, field) = key
        .split_once('.')
        .ok_or_else(|| Error::Other(format!("malformed key '{key}'")))?;

    let val = doc
        .get(section)
        .and_then(|item| item.as_table())
        .and_then(|table| table.get(field));

    match val {
        Some(v) => println!("{}", v.to_string().trim()),
        None => println!("<unset>"),
    }
    Ok(())
}

fn unset(config_path: &Path, key: &str) -> Result<()> {
    lookup_known(key)?;
    let mut doc = load_doc(config_path)?;
    let (section, field) = key
        .split_once('.')
        .ok_or_else(|| Error::Other(format!("malformed key '{key}'")))?;

    if let Some(table) = doc.get_mut(section).and_then(|i| i.as_table_mut()) {
        if table.remove(field).is_some() {
            // If we just emptied the section, drop it too so the file
            // doesn't end up with stale section headers.
            if table.is_empty() {
                doc.remove(section);
            }
            save_doc(config_path, &doc)?;
            println!("Unset {key}");
            return Ok(());
        }
    }
    println!("{key} was not set; nothing to do");
    Ok(())
}

fn list(config_path: &Path) -> Result<()> {
    let doc = load_doc(config_path)?;
    println!("Known config keys (project config: {}):", config_path.display());
    println!();
    for known in KNOWN_KEYS {
        let (section, field) = known.key.split_once('.').unwrap_or((known.key, ""));
        let current = doc
            .get(section)
            .and_then(|i| i.as_table())
            .and_then(|t| t.get(field))
            .map(|v| v.to_string().trim().to_string())
            .unwrap_or_else(|| "<unset>".to_string());
        println!("  {key:<28} {kind:<6} = {current}", key = known.key, kind = match known.kind {
            ValueKind::String => "string",
            ValueKind::Bool => "bool",
        });
        println!("      {}", known.description);
        println!();
    }
    Ok(())
}

/// Read the project config TOML. Returns an empty document if the
/// file doesn't exist yet — `ws config set` then creates it on save.
fn load_doc(path: &Path) -> Result<DocumentMut> {
    match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<DocumentMut>().map_err(|e| {
            Error::Other(format!(
                "failed to parse {}: {e}",
                path.display()
            ))
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(Error::Other(format!(
            "failed to read {}: {e}",
            path.display()
        ))),
    }
}

fn save_doc(path: &Path, doc: &DocumentMut) -> Result<()> {
    let content = doc.to_string();
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(&PathBuf::from(".")));
    std::fs::write(path, content).map_err(|e| {
        Error::Other(format!(
            "failed to write {}: {e}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ConfigArgs,
    }

    #[test]
    fn parses_set_subcommand() {
        let cli = TestCli::try_parse_from(["test", "set", "workspace.alias", "my-name"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn parses_get_subcommand() {
        let cli = TestCli::try_parse_from(["test", "get", "workspace.alias"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn parses_unset_subcommand() {
        let cli = TestCli::try_parse_from(["test", "unset", "workspace.alias"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn parses_list_subcommand() {
        let cli = TestCli::try_parse_from(["test", "list"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn set_string_writes_to_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        set(&path, "workspace.alias", "my-name").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("[workspace]"));
        assert!(content.contains("alias = \"my-name\""));
    }

    #[test]
    fn set_bool_writes_to_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        set(&path, "workspace.use_path_hash", "true").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("use_path_hash = true"));
    }

    #[test]
    fn set_unknown_key_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        let result = set(&path, "workspace.alais", "x"); // typo
        assert!(result.is_err());
        assert!(format!("{:?}", result.unwrap_err()).contains("unknown config key"));
    }

    #[test]
    fn set_invalid_bool_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        let result = set(&path, "workspace.use_path_hash", "maybe");
        assert!(result.is_err());
    }

    #[test]
    fn unset_removes_empty_section() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        // Manually write a section with one key.
        std::fs::write(&path, "[workspace]\nalias = \"x\"\n").unwrap();
        unset(&path, "workspace.alias").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            !content.contains("[workspace]"),
            "empty section should be removed; got:\n{content}"
        );
    }

    #[test]
    fn set_preserves_existing_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".agent-workspace.toml");
        std::fs::write(&path, "[general]\ntrunk = \"main\"\n").unwrap();
        set(&path, "workspace.alias", "x").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("trunk = \"main\""), "existing general.trunk must survive");
        assert!(content.contains("alias = \"x\""));
    }
}
