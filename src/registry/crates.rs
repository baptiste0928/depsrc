//! crates.io: the API gives the repository, the `.crate` archive gives the commit.

use anyhow::Result;
use serde::Deserialize;

use super::{Package, Release, base, download, get_json, normalize_repo};
use crate::archive;
use crate::git::Commit;

#[derive(Deserialize)]
struct CrateRes {
    #[serde(rename = "crate")]
    krate: Crate,
}

#[derive(Deserialize)]
struct Crate {
    /// Highest stable release, else the highest one.
    default_version: String,
    repository: Option<String>,
}

#[derive(Deserialize)]
struct VersionRes {
    version: Version,
}

#[derive(Deserialize)]
struct Version {
    /// Canonical crate name, which prefixes archive paths.
    #[serde(rename = "crate")]
    krate: String,
    num: String,
    repository: Option<String>,
    dl_path: String,
}

/// Written by `cargo publish` in the archive.
#[derive(Deserialize)]
struct VcsInfo {
    git: VcsGit,
    #[serde(default)]
    path_in_vcs: String,
}

#[derive(Deserialize)]
struct VcsGit {
    sha1: String,
}

fn api() -> String {
    format!("{}/api/v1/crates", base("DEPSRC_CRATES_URL", "https://crates.io"))
}

pub fn package(name: &str) -> Result<Option<Package>> {
    // Skips the list of every version.
    Ok(get_json::<CrateRes>(&format!("{}/{name}?include=default_version", api()))?.map(|res| Package {
        latest: res.krate.default_version,
        repo: res.krate.repository.as_deref().and_then(normalize_repo),
        subdir: None,
    }))
}

pub fn release(name: &str, version: &str) -> Result<Option<Release>> {
    let Some(VersionRes { version: v }) = get_json(&format!("{}/{name}/{version}", api()))? else {
        return Ok(None);
    };

    let tarball = format!("{}{}", base("DEPSRC_CRATES_URL", "https://crates.io"), v.dl_path);
    let repo = v.repository.as_deref().and_then(normalize_repo);

    // Only worth downloading when there is a repository to fetch the commit from.
    let vcs = match repo {
        Some(_) => {
            let member = format!("{}-{}/.cargo_vcs_info.json", v.krate, v.num);
            archive::read(&download(&tarball)?, &member).and_then(|b| serde_json::from_slice::<VcsInfo>(&b).ok())
        }
        None => None,
    };

    Ok(Some(Release {
        version: v.num,
        repo,
        rev: vcs.as_ref().and_then(|i| Commit::parse(&i.git.sha1)),
        subdir: vcs.and_then(|i| super::subdir(&i.path_in_vcs)),
        tarball,
    }))
}
