//! Resolves a spec to the targets to try, best first.
//!
//! ```text
//! crate:/npm: ── pin: lockfile, else latest ──> @version ── registry ──> commit or release tag, else archive
//!          └─ #ref ── package repository ──> ref       (a git dependency in Cargo.lock pins its commit)
//! gh:/git: ── their repository ──> #ref, else default branch
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::checkout::{Rev, Source, Target, slug};
use crate::git::{self, Commit};
use crate::lockfile::{self, Locked};
use crate::registry;
use crate::spec::{At, Kind, Spec};

/// Spec at its lockfile version, or as given, with its target when the lockfile already gives it.
pub struct Pinned {
    pub spec: Spec,
    pub target: Option<Target>,
}

/// Pins a registry spec without version to the lockfile's, else to the latest release.
pub fn pin(spec: &Spec, cwd: &Path) -> Result<Pinned> {
    if spec.at.is_some() || matches!(spec.kind, Kind::Gh | Kind::Git) {
        return Ok(Pinned { spec: spec.clone(), target: None });
    }

    Ok(match lockfile::find(cwd, spec.kind, &spec.name)? {
        Some(Locked::Version(v)) => Pinned { spec: spec.with(At::Version(v)), target: None },
        Some(Locked::Foreign(origin)) => bail!("{spec}: locked from {origin}, pass a git: spec instead"),
        Some(Locked::Git { url, rev }) => Pinned {
            spec: spec.with(At::Ref(rev.to_string())),
            target: Some(Target { source: Source::Git { url, rev: Rev::Commit(rev) }, subdir: None }),
        },
        None => match registry::package(spec.kind, &spec.name)? {
            Some(pkg) => Pinned { spec: spec.with(At::Version(pkg.latest)), target: None },
            None => bail!("{spec}: not found"),
        },
    })
}

/// Targets of a pinned spec, best first, with the spec at the version found: a dist-tag such as
/// `npm:zod@latest` becomes `npm:zod@3.23.8`.
pub fn targets(spec: &Spec) -> Result<(Spec, Vec<Target>)> {
    let git_ref = match &spec.at {
        // A full commit id is fetched as is: it may be on no branch nor tag.
        Some(At::Ref(r)) => Commit::parse(r).map_or_else(|| Rev::Ref(r.clone()), Rev::Commit),
        _ => Rev::Head,
    };

    let git = |url: String, subdir| vec![Target { source: Source::Git { url, rev: git_ref.clone() }, subdir }];

    let version = match (spec.kind, &spec.at) {
        (Kind::Gh, _) => return Ok((spec.clone(), git(format!("https://github.com/{}", spec.name), None))),
        (Kind::Git, _) => return Ok((spec.clone(), git(spec.name.clone(), None))),
        (_, Some(At::Version(v))) => v,
        (_, Some(At::Ref(_))) => {
            return match registry::package(spec.kind, &spec.name)? {
                Some(registry::Package { repo: Some(url), subdir, .. }) => Ok((spec.clone(), git(url, subdir))),
                Some(_) => bail!("{spec}: no repository to look the ref up"),
                None => bail!("{spec}: not found"),
            };
        }
        (_, None) => unreachable!("registry specs are pinned"),
    };

    let Some(release) = registry::release(spec.kind, &spec.name, version)? else {
        bail!("{spec}: version not found, use #<ref> for git refs");
    };

    let mut out = Vec::new();

    if let Some(url) = release.repo {
        // No published commit: guess it from a release tag.
        let rev = release.rev.or_else(|| match release_tag(&url, &spec.name, &release.version) {
            Ok(Some((tag, commit))) => {
                eprintln!("note: {spec}: no published commit, using tag {tag}");
                Some(commit)
            }
            Ok(None) => None,
            Err(e) => {
                eprintln!("note: {spec}: list tags: {e:#}");
                None
            }
        });

        if let Some(rev) = rev {
            out.push(Target { source: Source::Git { url, rev: Rev::Commit(rev) }, subdir: release.subdir });
        }
    }

    let name = slug(&format!("{}/{}@{}", spec.kind.as_str(), spec.name, release.version));
    out.push(Target { source: Source::Archive { url: release.tarball, name }, subdir: None });

    Ok((spec.with(At::Version(release.version)), out))
}

/// Directory of the `spec` package in a checked out tree, if not at its root: the closest one whose
/// manifest declares its name.
pub fn package_dir(tree: &Path, spec: &Spec) -> Option<String> {
    let declares = |dir: &Path| match spec.kind {
        Kind::Crate => fs::read_to_string(dir.join("Cargo.toml")).is_ok_and(|text| {
            let package = text.lines().map(str::trim).skip_while(|l| *l != "[package]").skip(1);
            let fields = package.take_while(|l| !l.starts_with('['));
            fields
                .filter_map(|l| l.strip_prefix("name")?.trim_start().strip_prefix('='))
                .any(|v| lockfile::same_crate(v.trim().trim_matches('"'), &spec.name))
        }),
        Kind::Npm => fs::read(dir.join("package.json")).is_ok_and(|bytes| {
            let name = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v["name"].as_str().map(str::to_string));
            name.as_deref() == Some(&spec.name)
        }),
        Kind::Gh | Kind::Git => false,
    };

    if declares(tree) {
        return None;
    }

    // Breadth first and shallow: workspace members sit near the root.
    let mut level = vec![tree.to_path_buf()];

    for _ in 0..3 {
        let mut next = Vec::new();

        for dir in level {
            let Ok(entries) = fs::read_dir(&dir) else { continue };

            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .filter(|e| !["node_modules", "target"].contains(&&*e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect();
            dirs.sort();

            if let Some(found) = dirs.iter().find(|d| declares(d)) {
                return Some(found.strip_prefix(tree).ok()?.to_str()?.to_string());
            }

            next.extend(dirs);
        }

        level = next;
    }

    None
}

/// First tag of the repository at `url` named after a common release pattern, with its commit.
fn release_tag(url: &str, name: &str, version: &str) -> Result<Option<(String, Commit)>> {
    let mut tags = git::remote_tags(url)?;
    let unscoped = name.rsplit_once('/').map_or(name, |(_, n)| n);
    let names = std::iter::once(name).chain((unscoped != name).then_some(unscoped));

    // Package specific patterns first: in monorepos, `v1.0.0` may be another package's release.
    let candidates = names
        .flat_map(|n| [format!("{n}@{version}"), format!("{n}-v{version}"), format!("{n}-{version}")])
        .chain([format!("v{version}"), version.to_string()]);

    Ok(candidates.into_iter().find_map(|c| tags.remove_entry(&c)))
}
