//! Materializes a resolved target as a tree in the store.
//!
//! ```text
//! Git { url, rev } ── clone/fetch ──> repos/<repo> ── worktree add ──> trees/<repo>@<rev>
//! Archive { url }  ── download ─────────────────────── extract ──────> trees/<kind>--<pkg>@<version>
//! ```
//!
//! Commits and tags never change. Branch trees are refetched when older than a day.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::git::{self, Commit};
use crate::store::{self, Db, Store, Tree};
use crate::{archive, registry};

/// Where a package's source lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub source: Source,
    /// Package directory inside the tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Git {
        url: String,
        rev: Rev,
    },
    /// Registry tarball, without history. `name` names the tree.
    Archive {
        url: String,
        name: String,
    },
}

impl Source {
    pub fn is_archive(&self) -> bool {
        matches!(self, Source::Archive { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rev {
    Commit(Commit),
    /// Tag, branch or commit prefix.
    Ref(String),
    /// Remote default branch.
    Head,
}

/// Checks `source` out, records its tree and returns the tree name.
pub fn checkout(store: &Store, source: &Source, refetch: bool) -> Result<String> {
    match source {
        Source::Git { url, rev } => git_tree(store, url, rev, refetch),
        Source::Archive { url, name } => archive_tree(store, url, name),
    }
}

/// Store directory name of a repo url: `github.com--serde-rs--serde`.
pub fn repo_name(url: &str) -> String {
    let url = url.split_once("://").map_or(url, |(_, rest)| rest);
    let url = url.split_once('@').filter(|(user, _)| !user.contains('/')).map_or(url, |(_, rest)| rest);
    let url = url.trim_end_matches('/');
    slug(url.strip_suffix(".git").unwrap_or(url))
}

/// Joins path segments with `--`, keeping file name safe characters. A name that does not read
/// back as `s` gets a hash of it, so that distinct inputs never share a directory.
pub fn slug(s: &str) -> String {
    let safe = |seg: &str| -> String {
        let seg: String =
            seg.chars().map(|c| if c.is_ascii_alphanumeric() || "._@+-".contains(c) { c } else { '-' }).collect();
        seg.strip_prefix('.').map_or(seg.clone(), |rest| format!("-{rest}"))
    };

    let segments: Vec<&str> = s.split('/').collect();
    let name = segments.iter().filter(|seg| !seg.is_empty()).map(|seg| safe(seg)).collect::<Vec<_>>().join("--");

    let reversible = segments.iter().all(|seg| {
        !seg.is_empty() && safe(seg) == *seg && !seg.contains("--") && !seg.starts_with('-') && !seg.ends_with('-')
    });

    if reversible { name } else { format!("{name}~{:08x}", fnv(s) >> 32) }
}

/// FNV-1a: stable across builds, unlike the std hasher.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// Checks `rev` of the repo at `url` out, cloning and fetching as needed.
fn git_tree(store: &Store, url: &str, rev: &Rev, refetch: bool) -> Result<String> {
    let repo_name = repo_name(url);
    let _lock = store.lock(&repo_name)?;
    let repo = store.repo(&repo_name);
    let cloned = !repo.exists();

    if cloned {
        let tmp = store.repo(&format!(".tmp-{repo_name}"));
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        git::clone(url, &tmp).with_context(|| format!("clone {url}"))?;
        fs::rename(&tmp, &repo)?;
    }

    let mut db = store.read()?;

    let label = match rev {
        Rev::Commit(commit) => short(&db, &repo_name, commit),
        Rev::Ref(r) => r.clone(),
        Rev::Head => git::head_branch(&repo)?,
    };

    let name = format!("{repo_name}@{}", slug(&label));
    let existing = db.trees.remove(&name);
    let now = store::now();
    let mut fetched = existing.as_ref().map_or(now, |t| t.fetched);

    let (commit, branch) = match rev {
        Rev::Commit(commit) => {
            if git::resolve(&repo, commit.as_str()).is_none() {
                git::fetch(&repo, &[commit.as_str()]).with_context(|| format!("fetch {commit} from {url}"))?;
            }

            (commit.clone(), false)
        }
        Rev::Ref(_) | Rev::Head => {
            // Refs only have to be fetched once per call.
            let mut fresh = cloned;

            let found = match find_ref(&repo, &label) {
                Some(found) => Some(found),
                None if !fresh => {
                    git::fetch(&repo, &["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"])?;
                    (fresh, fetched) = (true, now);
                    find_ref(&repo, &label)
                }
                None => None,
            };

            let (commit, branch) = found.with_context(|| format!("ref `{label}` not found in {url}"))?;

            if branch && !fresh && existing.as_ref().is_none_or(|t| t.stale(refetch)) {
                let branch = format!("refs/heads/{label}");
                match git::fetch(&repo, &[&format!("+{branch}:{branch}")]) {
                    Ok(()) => {
                        fetched = now;
                        (git::resolve(&repo, &branch).expect("branch exists"), true)
                    }
                    // Offline: the tree on disk is better than nothing.
                    Err(e) if existing.is_some() && store.tree(&name).exists() => {
                        eprintln!("note: {url}#{label}: refetch failed, serving the last fetched tree ({e:#})");
                        (commit, true)
                    }
                    Err(e) => return Err(e.context(format!("fetch {label}"))),
                }
            } else {
                (commit, branch)
            }
        }
    };

    let path = store.tree(&name);

    match &existing {
        Some(tree) if path.exists() => {
            if tree.rev.as_ref() != Some(&commit) {
                git::checkout(&path, commit.as_str())?;
            }
        }
        _ => {
            if path.exists() {
                fs::remove_dir_all(&path)?;
            }

            git::worktree_prune(&repo)?;
            git::worktree_add(&repo, &path, commit.as_str()).with_context(|| format!("check out {label}"))?;
        }
    }

    let source =
        Source::Git { url: url.to_string(), rev: if branch { Rev::Ref(label) } else { Rev::Commit(commit.clone()) } };

    let tree = Tree { source, rev: Some(commit), branch, fetched, used: now };
    store.update(|db| db.trees.insert(name.clone(), tree))?;

    Ok(name)
}

/// Commit of `label`: a branch, a tag or a commit prefix, and whether it is a branch.
fn find_ref(repo: &Path, label: &str) -> Option<(Commit, bool)> {
    if let Some(commit) = git::resolve(repo, &format!("refs/heads/{label}")) {
        return Some((commit, true));
    }
    let commit = git::resolve(repo, &format!("refs/tags/{label}")).or_else(|| git::resolve(repo, label))?;
    Some((commit, false))
}

/// Shortest prefix of `commit`, from 8 characters, not naming another commit's tree of the repo.
/// Callers hold the repo lock, so no other process can take it meanwhile.
fn short(db: &Db, repo_name: &str, commit: &Commit) -> String {
    let sha = commit.as_str();
    (8..sha.len())
        .map(|n| &sha[..n])
        .find(|p| db.trees.get(&format!("{repo_name}@{p}")).is_none_or(|t| t.rev.as_ref() == Some(commit)))
        .unwrap_or(sha)
        .to_string()
}

/// Extracts the archive at `url` as the `name` tree, unless already there.
fn archive_tree(store: &Store, url: &str, name: &str) -> Result<String> {
    let _lock = store.lock(name)?;
    let path = store.tree(name);
    let now = store::now();
    let existing = store.read()?.trees.remove(name);

    let fetched = match existing {
        Some(tree) if path.exists() => tree.fetched,
        _ => {
            let bytes = registry::download(url)?;
            let tmp = store.tree(&format!(".tmp-{name}"));

            for dir in [&tmp, &path] {
                if dir.exists() {
                    fs::remove_dir_all(dir)?;
                }
            }

            archive::extract(&bytes, &tmp).with_context(|| format!("extract {url}"))?;
            fs::rename(&tmp, &path)?;
            now
        }
    };

    let source = Source::Archive { url: url.to_string(), name: name.to_string() };
    let tree = Tree { source, rev: None, branch: false, fetched, used: now };
    store.update(|db| db.trees.insert(name.to_string(), tree))?;

    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(repo_name("https://github.com/serde-rs/serde.git"), "github.com--serde-rs--serde");
        assert_eq!(slug("@types/node@1.0"), "@types--node@1.0");
        assert_eq!(slug("feat/x"), "feat--x");

        // Names that would collide get a hash.
        for (a, b) in [("gitlab.com/x/a--b", "gitlab.com/x/a/b"), ("a-/b", "a/-b"), ("a:b", "a-b"), ("a//b", "a/b")] {
            assert_ne!(slug(a), slug(b), "{a} {b}");
        }

        assert!(repo_name("file:///tmp/../x").starts_with("tmp---.--x~"));
    }

    #[test]
    fn short_commits() {
        let commit = |c: &str| Commit::parse(&format!("abcdef12{}", c.repeat(32))).unwrap();
        let (a, b) = (commit("0"), commit("1"));
        let mut db = Db::default();
        let source = Source::Git { url: String::new(), rev: Rev::Commit(a.clone()) };
        let tree = Tree { source, rev: Some(a.clone()), branch: false, fetched: 0, used: 0 };
        db.trees.insert("r@abcdef12".to_string(), tree);

        assert_eq!(short(&db, "r", &a), "abcdef12");
        assert_eq!(short(&db, "r", &b), "abcdef121");
        assert_eq!(short(&db, "other", &b), "abcdef12");
    }
}
