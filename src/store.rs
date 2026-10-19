//! On-disk store: bare repos, checked out trees and the `sources.json` index.
//!
//! ```text
//! <store>/
//!   sources.json                      Db: resolutions and trees
//!   repos/github.com--serde-rs--serde bare blobless clones
//!   trees/github.com--serde-rs--serde@89c4b02b
//!   trees/npm--zod@3.23.8             registry archive, no history
//!   locks/                            one lock file per repo or tree
//! ```
//!
//! `.tmp-<name>` entries in `repos/` and `trees/` are clones and extractions in progress, renamed
//! once complete.
//!
//! Several processes may run at once: `update` serializes index writes, `lock` serializes work
//! on one repo or tree.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::checkout::{Source, Target};
use crate::git::Commit;

pub struct Store {
    root: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Db {
    /// Resolution cache: exact spec → package.
    #[serde(default)]
    pub pkgs: BTreeMap<String, Pkg>,
    #[serde(default)]
    pub trees: BTreeMap<String, Tree>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pkg {
    #[serde(flatten)]
    pub target: Target,
    /// Last tree checked out for the target.
    pub tree: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Tree {
    pub source: Source,
    /// Checked out commit, for git trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<Commit>,
    /// Whether the tree follows a branch, refreshed when stale.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub branch: bool,
    /// Unix time of the last fetch.
    pub fetched: u64,
    /// Unix time of the last `path` call.
    pub used: u64,
}

/// Age after which branch trees are refetched.
const STALE: u64 = 24 * 3600;

impl Tree {
    /// Whether the tree must be refetched, if it follows a branch.
    pub fn stale(&self, refetch: bool) -> bool {
        refetch || now().saturating_sub(self.fetched) > STALE
    }
}

impl Store {
    /// Store at `root`, else `$DEPSRC_STORE`, `$XDG_CACHE_HOME/depsrc` or `~/.cache/depsrc`.
    fn locate(root: Option<PathBuf>) -> Result<Store> {
        let root = root.or_else(|| std::env::var_os("DEPSRC_STORE").filter(|v| !v.is_empty()).map(PathBuf::from));

        let root = root.or_else(|| match std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
            Some(data) => Some(PathBuf::from(data).join("depsrc")),
            None => std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/depsrc")),
        });

        let Some(root) = root else {
            anyhow::bail!("cannot locate the store: set HOME or --store");
        };

        Ok(Store { root })
    }

    /// Located store, created if missing.
    pub fn open(root: Option<PathBuf>) -> Result<Store> {
        let store = Store::locate(root)?;
        for dir in ["repos", "trees", "locks"] {
            let dir = store.root.join(dir);
            fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        }
        Ok(store)
    }

    /// Bare repo directory.
    pub fn repo(&self, name: &str) -> PathBuf {
        self.root.join("repos").join(name)
    }

    /// Tree directory.
    pub fn tree(&self, name: &str) -> PathBuf {
        self.root.join("trees").join(name)
    }

    /// Package directory of `pkg`.
    pub fn pkg(&self, pkg: &Pkg) -> PathBuf {
        let tree = self.tree(&pkg.tree);
        pkg.target.subdir.as_ref().map_or(tree.clone(), |sub| tree.join(sub))
    }

    /// Exclusive lock on `name`, released on drop.
    pub fn lock(&self, name: &str) -> Result<File> {
        lock(&self.root.join("locks").join(name))
    }

    /// Current index, empty if missing.
    pub fn read(&self) -> Result<Db> {
        let path = self.root.join("sources.json");
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).with_context(|| format!("read {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Db::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Applies `f` to the index under an exclusive lock and saves it atomically.
    pub fn update<T>(&self, f: impl FnOnce(&mut Db) -> T) -> Result<T> {
        let _lock = lock(&self.root.join("sources.lock"))?;
        let mut db = self.read()?;
        let out = f(&mut db);

        let tmp = self.root.join(format!("sources.json.{}", std::process::id()));
        fs::write(&tmp, serde_json::to_vec_pretty(&db)?)?;
        fs::rename(&tmp, self.root.join("sources.json"))?;

        Ok(out)
    }
}

/// Unix time in seconds.
pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn lock(path: &Path) -> Result<File> {
    let file = File::create(path).with_context(|| format!("lock {}", path.display()))?;
    file.lock().with_context(|| format!("lock {}", path.display()))?;
    Ok(file)
}
