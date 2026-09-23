//! Safe config edits (used by the admin API / dashboard): refuse env-locked keys, validate the
//! candidate before writing, write atomically with a `.bak`, keep `secrets.toml` at 0600.

use crate::loader::{ConfigDir, ConfigLoader, Issue, Loaded};
use std::{io::Write, path::Path};
use toml_edit::{Array, DocumentMut, Item, Table, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    Config,
    Secrets,
}

/// Set (`Some`) or remove (`None`) the value at `path` (segments, e.g.
/// `["routing", "chains", "eip155:4663", "evm_rpc"]`). Paths under `keys` go to `secrets.toml`.
#[derive(Debug, Clone)]
pub struct Edit {
    pub path: Vec<String>,
    pub value: Option<serde_json::Value>,
}

impl Edit {
    pub fn target(&self) -> EditTarget {
        if self.path.first().is_some_and(|p| p == "keys") {
            EditTarget::Secrets
        } else {
            EditTarget::Config
        }
    }
}

pub fn apply_edits(
    loader: &ConfigLoader,
    current: &Loaded,
    edits: &[Edit],
) -> Result<Loaded, Vec<Issue>> {
    let locked: Vec<Issue> = edits
        .iter()
        .filter_map(|e| {
            current.locked_by(&e.path).map(|var| {
                Issue::error(
                    e.path.join("."),
                    format!("locked by env ({var}); change it in the environment"),
                )
            })
        })
        .collect();
    if !locked.is_empty() {
        return Err(locked);
    }
    if let Some(e) = edits.iter().find(|e| e.path.is_empty()) {
        return Err(vec![Issue::error(
            format!("{:?}", e.path),
            "empty edit path",
        )]);
    }

    let dir = &loader.dir;
    let mut config: DocumentMut = parse(&dir.config_path())?;
    let mut secrets: DocumentMut = parse(&dir.secrets_path())?;
    for e in edits {
        let doc = match e.target() {
            EditTarget::Config => &mut config,
            EditTarget::Secrets => &mut secrets,
        };
        set_path(doc.as_table_mut(), &e.path, e.value.as_ref())
            .map_err(|m| vec![Issue::error(e.path.join("."), m)])?;
    }

    let (config_s, secrets_s) = (config.to_string(), secrets.to_string());
    let next = loader.load_texts(&config_s, &secrets_s)?;
    if edits.iter().any(|e| e.target() == EditTarget::Config) {
        write_atomic(&dir.config_path(), &config_s, None)
            .map_err(|m| vec![Issue::error("config.toml", m)])?;
    }
    if edits.iter().any(|e| e.target() == EditTarget::Secrets) {
        write_atomic(&dir.secrets_path(), &secrets_s, Some(0o600))
            .map_err(|m| vec![Issue::error("secrets.toml", m)])?;
    }
    Ok(next)
}

fn parse(path: &Path) -> Result<DocumentMut, Vec<Issue>> {
    ConfigDir::read(path)
        .map_err(|e| vec![e])?
        .parse::<DocumentMut>()
        .map_err(|e| vec![Issue::error(path.display().to_string(), e.to_string())])
}

fn set_path(
    table: &mut Table,
    path: &[String],
    value: Option<&serde_json::Value>,
) -> Result<(), String> {
    let (last, parents) = path.split_last().ok_or("empty path")?;
    let mut cur = table;
    for seg in parents {
        let entry = cur.entry(seg).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        if let Some(inline) = entry.as_inline_table().cloned() {
            *entry = Item::Table(inline.into_table());
        }
        cur = entry
            .as_table_mut()
            .ok_or_else(|| format!("'{seg}' is not a table"))?;
    }
    match value {
        None => {
            cur.remove(last);
        }
        Some(v) => {
            cur.insert(last, to_item(v)?);
        }
    }
    Ok(())
}

fn to_item(v: &serde_json::Value) -> Result<Item, String> {
    Ok(match v {
        serde_json::Value::Object(map) => {
            let mut t = Table::new();
            for (k, v) in map {
                t.insert(k, to_item(v)?);
            }
            Item::Table(t)
        }
        other => Item::Value(to_value(other)?),
    })
}

fn to_value(v: &serde_json::Value) -> Result<Value, String> {
    Ok(match v {
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => return Err(format!("unsupported number {n}")),
        },
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(items) => {
            let mut a = Array::new();
            for i in items {
                a.push(to_value(i)?);
            }
            Value::Array(a)
        }
        serde_json::Value::Object(map) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in map {
                t.insert(k, to_value(v)?);
            }
            Value::InlineTable(t)
        }
        serde_json::Value::Null => return Err("null is not a TOML value; use removal".into()),
    })
}

/// Write via temp file + rename in the same directory; keep the previous file as `<name>.bak`.
pub fn write_atomic(path: &Path, contents: &str, mode: Option<u32>) -> Result<(), String> {
    let dir = path.parent().ok_or("path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file_name = path
        .file_name()
        .ok_or("path has no file name")?
        .to_string_lossy();
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        set_mode(&tmp, mode)?;
        f.write_all(contents.as_bytes())
            .map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    if path.exists() {
        let bak = dir.join(format!("{file_name}.bak"));
        std::fs::copy(path, &bak).map_err(|e| e.to_string())?;
        set_mode(&bak, mode)?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(m) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(m))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: Option<u32>) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::EnvSource;
    use bdm_ports::Capability;
    use serde_json::json;

    fn p(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn edits_write_validate_and_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "# keep me\n[server]\ntool_profile = \"payments\"\n",
        )
        .unwrap();
        let loader = ConfigLoader::new(
            ConfigDir::new(dir.path()),
            EnvSource::from_pairs([("BDM__VENDORS__ALCHEMY__CAP__MONTHLY", "100")]),
        )
        .unwrap();
        let cur = loader.load().unwrap();

        // 1. valid edit: reorder Robinhood RPC + add a key
        let next = apply_edits(
            &loader,
            &cur,
            &[
                Edit {
                    path: p(&["routing", "chains", "eip155:4663", "evm_rpc"]),
                    value: Some(json!(["quicknode", "public"])),
                },
                Edit {
                    path: p(&["keys", "helius", "api_key"]),
                    value: Some(json!("hel_secret_xyz")),
                },
            ],
        )
        .unwrap();
        let rh: bdm_domain::ChainId = "eip155:4663".parse().unwrap();
        assert_eq!(
            next.order(Capability::EvmRpc, Some(&rh), None).vendors,
            ["quicknode", "public"]
        );
        let written = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(written.contains("# keep me"), "comments preserved");
        assert!(
            !written.contains("hel_secret_xyz"),
            "secret never in config.toml"
        );
        assert!(dir.path().join("config.toml.bak").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("secrets.toml"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        // 2. locked by env
        let err = apply_edits(
            &loader,
            &next,
            &[Edit {
                path: p(&["vendors", "alchemy", "cap", "monthly"]),
                value: Some(json!(5)),
            }],
        )
        .unwrap_err();
        assert!(err[0]
            .message
            .contains("BDM__VENDORS__ALCHEMY__CAP__MONTHLY"));

        // 3. invalid edit is not written
        let before = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        let err = apply_edits(
            &loader,
            &next,
            &[Edit {
                path: p(&["routing", "defaults", "price"]),
                value: Some(json!(["nope"])),
            }],
        )
        .unwrap_err();
        assert!(err.iter().any(|i| i.message.contains("unknown vendor")));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml")).unwrap(),
            before
        );
    }
}
