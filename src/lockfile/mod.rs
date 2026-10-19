//! Pins a package to the version locked by the project, found by walking up from the cwd.
//!
//! Each parser scans only the entries of the requested package, marking those the project
//! depends on directly. A single direct version wins over transitive ones; any other conflict is
//! an error listing the candidates.

mod bun;
mod cargo;
mod npm;
mod pnpm;
mod yarn;

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::git::Commit;
use crate::spec::Kind;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Locked {
    Version(String),
    /// Cargo git dependency.
    Git {
        url: String,
        rev: Commit,
    },
    /// Not from the default registry under this name: another registry, or an npm alias.
    Foreign(String),
}

impl fmt::Display for Locked {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Locked::Version(v) => f.write_str(v),
            Locked::Git { rev, .. } => f.write_str(rev.as_str()),
            Locked::Foreign(origin) => f.write_str(origin),
        }
    }
}

struct Entry {
    locked: Locked,
    direct: bool,
}

const CARGO: &[&str] = &["Cargo.lock"];
const NPM: &[&str] = &["pnpm-lock.yaml", "package-lock.json", "yarn.lock", "bun.lock"];

/// Version of `name` locked by the project around `cwd`.
pub fn find(cwd: &Path, kind: Kind, name: &str) -> Result<Option<Locked>> {
    let files = match kind {
        Kind::Crate => CARGO,
        Kind::Npm => NPM,
        Kind::Gh | Kind::Git => return Ok(None),
    };

    let Some(path) = cwd.ancestors().find_map(|dir| files.iter().map(|f| dir.join(f)).find(|p| p.is_file())) else {
        return Ok(None);
    };

    let text = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let root = path.parent().unwrap_or(cwd);

    let entries = match path.file_name().and_then(|f| f.to_str()) {
        Some("Cargo.lock") => cargo::entries(&text, name),
        Some("package-lock.json") => {
            npm::entries(&text, name, &member(cwd, root)).with_context(|| format!("parse {}", path.display()))?
        }
        Some("pnpm-lock.yaml") => pnpm::entries(&text, name, &member(cwd, root)),
        Some("yarn.lock") => yarn::entries(&text, name, &yarn::wanted(cwd, root, name)),
        _ => bun::entries(&text, name, &member(cwd, root)),
    };

    pick(name, &entries)
}

/// npm workspace member owning `cwd`, whose own locked versions win over the root ones.
struct Member {
    /// Directory relative to the lockfile's, empty for the root.
    dir: String,
    /// Package name, which prefixes its entries in `bun.lock`.
    name: Option<String>,
}

fn member(cwd: &Path, root: &Path) -> Member {
    let dir = cwd.ancestors().take_while(|d| d.starts_with(root)).find(|d| d.join("package.json").is_file());
    let dir = dir.unwrap_or(root);

    let name = std::fs::read(dir.join("package.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["name"].as_str().map(str::to_string));

    let dir = dir.strip_prefix(root).ok().and_then(Path::to_str).unwrap_or("").to_string();
    Member { dir, name }
}

/// Version among `entries`: the only one, else the only direct one.
fn pick(name: &str, entries: &[Entry]) -> Result<Option<Locked>> {
    let distinct = |direct_only: bool| {
        let mut out: Vec<&Locked> = Vec::new();

        for e in entries.iter().filter(|e| e.direct || !direct_only) {
            if !out.contains(&&e.locked) {
                out.push(&e.locked);
            }
        }

        out
    };

    let all = distinct(false);

    if all.len() <= 1 {
        return Ok(all.first().map(|&l| l.clone()));
    }

    if let [direct] = distinct(true)[..] {
        return Ok(Some(direct.clone()));
    }

    let list: Vec<String> = all.iter().map(ToString::to_string).collect();
    bail!("{name} has several locked versions ({}), pass one with @<version>", list.join(", "))
}

fn unquote(s: &str) -> &str {
    s.trim_matches(['"', '\''])
}

/// Splits `name@version`, the name possibly scoped (`@types/node@20.0.0`).
fn split_version(s: &str) -> Option<(&str, &str)> {
    let i = s.get(1..)?.find('@')? + 1;
    Some((&s[..i], &s[i + 1..]))
}

/// Registry version: drops pnpm peer suffixes, rejects `link:`, `file:`, `workspace:`…
fn version(v: &str) -> Option<String> {
    let v = v.trim().trim_matches(['"', '\'']);
    let v = v.split('(').next()?;
    v.starts_with(|c: char| c.is_ascii_digit()).then(|| v.to_string())
}

fn entry(v: &str, direct: bool) -> Option<Entry> {
    Some(Entry { locked: Locked::Version(version(v)?), direct })
}
