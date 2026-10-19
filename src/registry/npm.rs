//! npm registry: the version document gives the repository, directory and `gitHead`.

use anyhow::Result;
use serde::Deserialize;

use super::{Package, Release, base, get_json, normalize_repo};
use crate::git::Commit;

#[derive(Deserialize)]
struct Doc {
    version: String,
    repository: Option<Repository>,
    #[serde(rename = "gitHead")]
    git_head: Option<String>,
    dist: Dist,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Repository {
    Url(String),
    Object { url: String, directory: Option<String> },
}

#[derive(Deserialize)]
struct Dist {
    tarball: String,
}

pub fn package(name: &str) -> Result<Option<Package>> {
    Ok(release(name, "latest")?.map(|r| Package { latest: r.version, repo: r.repo, subdir: r.subdir }))
}

/// `version` may also be a dist-tag.
pub fn release(name: &str, version: &str) -> Result<Option<Release>> {
    let base = base("DEPSRC_NPM_URL", "https://registry.npmjs.org");
    let url = format!("{base}/{name}/{version}");

    let Some(doc) = get_json::<Doc>(&url)? else {
        return Ok(None);
    };

    let (repo, subdir) = match doc.repository {
        Some(Repository::Url(url)) => (normalize_repo(&url), None),
        Some(Repository::Object { url, directory }) => (normalize_repo(&url), directory),
        None => (None, None),
    };

    let subdir = subdir.as_deref().and_then(super::subdir);
    let rev = doc.git_head.as_deref().and_then(Commit::parse);

    // Metadata must not downgrade the download to plain http, unless the registry itself uses it.
    let origin = base.find("://").and_then(|i| base[i + 3..].find('/').map(|j| &base[..i + 3 + j])).unwrap_or(&base);
    if !doc.dist.tarball.starts_with("https://") && !doc.dist.tarball.starts_with(&format!("{origin}/")) {
        anyhow::bail!("insecure tarball url {}", doc.dist.tarball);
    }

    Ok(Some(Release { version: doc.version, repo, rev, subdir, tarball: doc.dist.tarball }))
}
