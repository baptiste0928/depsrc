//! Removes trees unused for a while, then repos left without trees.

use std::fs;

use anyhow::Result;

use crate::checkout::{Source, repo_name};
use crate::spec::Spec;
use crate::store::{self, Db, Store};

/// Removes trees of `spec`, or unused for `since` seconds. Returns the removed trees and repos.
pub fn clean(store: &Store, spec: Option<&Spec>, since: u64) -> Result<(usize, usize)> {
    let cutoff = store::now().saturating_sub(since);

    let doomed = |db: &Db, name: &str| match spec {
        Some(spec) => db.pkgs.iter().any(|(key, pkg)| pkg.tree == name && matches(spec, key)),
        None => db.trees.get(name).is_some_and(|t| t.used <= cutoff),
    };

    let mut trees = 0;
    let db = store.read()?;

    for (name, tree) in &db.trees {
        if !doomed(&db, name) {
            continue;
        }

        let _lock = match &tree.source {
            Source::Git { url, .. } => store.lock(&repo_name(url))?,
            Source::Archive { .. } => store.lock(name)?,
        };
        // Unlist it before removal, and only if still doomed: another process may have used it meanwhile.
        let unlisted = store.update(|db| {
            if !doomed(db, name) {
                return false;
            }

            db.trees.remove(name);
            db.pkgs.retain(|_, pkg| pkg.tree != *name);

            for specs in db.projects.values_mut() {
                specs.retain(|_, key| db.pkgs.contains_key(key));
            }
            db.projects.retain(|_, specs| !specs.is_empty());

            true
        })?;

        if !unlisted {
            continue;
        }

        let path = store.tree(name);
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }

        if let Source::Git { url, .. } = &tree.source {
            let repo = store.repo(&repo_name(url));
            if repo.exists() {
                crate::git::worktree_prune(&repo)?;
            }
        }

        trees += 1;
    }

    let mut repos = 0;
    for name in store.repos()? {
        let _lock = store.lock(&name)?;

        let used = store
            .read()?
            .trees
            .values()
            .any(|t| matches!(&t.source, Source::Git { url, .. } if repo_name(url) == name));

        if !used {
            fs::remove_dir_all(store.repo(&name))?;
            repos += 1;
        }
    }

    Ok((trees, repos))
}

/// Whether the recorded spec `key` is `spec`, or one of its versions if it has none.
fn matches(spec: &Spec, key: &str) -> bool {
    key.parse::<Spec>()
        .is_ok_and(|k| k.kind == spec.kind && k.name == spec.name && (spec.at.is_none() || k.at == spec.at))
}

/// Parses `30d`, `12h`, `5m`, `0s` into seconds.
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().map_err(|_| format!("invalid duration `{s}`"))?;

    let unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 7 * 86400,
        _ => {
            return Err(format!("invalid duration `{s}`, expected a number with s, m, h, d or w"));
        }
    };

    Ok(n * unit)
}
