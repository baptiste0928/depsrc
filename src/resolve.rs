//! Resolves a spec to the targets to try, best first.
//!
//! ```text
//! crate:/npm: ── pin: lockfile, else latest ──> @version ── registry ──> commit, else archive
//!          └─ #ref ── package repository ──> ref       (a git dependency in Cargo.lock pins its commit)
//! gh:/git: ── their repository ──> #ref, else default branch
//! ```

use std::path::Path;

use anyhow::{Result, bail};

use crate::checkout::{Rev, Source, Target, slug};
use crate::git::Commit;
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
    if let (Some(url), Some(rev)) = (release.repo, release.rev) {
        out.push(Target { source: Source::Git { url, rev: Rev::Commit(rev) }, subdir: release.subdir });
    }

    let name = slug(&format!("{}/{}@{}", spec.kind.as_str(), spec.name, release.version));
    out.push(Target { source: Source::Archive { url: release.tarball, name }, subdir: None });

    Ok((spec.with(At::Version(release.version)), out))
}
