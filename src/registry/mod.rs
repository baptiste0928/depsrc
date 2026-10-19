//! Package registries: published versions and their source repository.
//!
//! Base urls can point to mirrors with `DEPSRC_CRATES_URL` and `DEPSRC_NPM_URL`.

mod crates;
mod npm;

use std::sync::LazyLock;

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;

use crate::git::Commit;
use crate::spec::Kind;

/// A package, at its latest version.
pub struct Package {
    pub latest: String,
    /// Normalized https url of the source repository.
    pub repo: Option<String>,
    /// Package directory inside the repository.
    pub subdir: Option<String>,
}

/// A published version.
pub struct Release {
    pub version: String,
    pub repo: Option<String>,
    /// Commit the version was published from.
    pub rev: Option<Commit>,
    pub subdir: Option<String>,
    pub tarball: String,
}

/// `None` if the package does not exist.
pub fn package(kind: Kind, name: &str) -> Result<Option<Package>> {
    match kind {
        Kind::Crate => crates::package(name),
        Kind::Npm => npm::package(name),
        Kind::Gh | Kind::Git => unreachable!("not a registry"),
    }
    .with_context(|| format!("{}:{name}", kind.as_str()))
}

/// `None` if the version does not exist.
pub fn release(kind: Kind, name: &str, version: &str) -> Result<Option<Release>> {
    // Keeps arbitrary input out of registry urls.
    if !version.chars().all(|c| c.is_ascii_alphanumeric() || ".+-_".contains(c)) {
        return Ok(None);
    }

    match kind {
        Kind::Crate => crates::release(name, version),
        Kind::Npm => npm::release(name, version),
        Kind::Gh | Kind::Git => unreachable!("not a registry"),
    }
    .with_context(|| format!("{}:{name}@{version}", kind.as_str()))
}

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .user_agent(concat!("depsrc/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent()
});

fn base(var: &str, default: &str) -> String {
    std::env::var(var).unwrap_or_else(|_| default.to_string()).trim_end_matches('/').to_string()
}

/// Decoded JSON at `url`, `None` on 404.
fn get_json<T: DeserializeOwned>(url: &str) -> Result<Option<T>> {
    let mut res = AGENT.get(url).call().with_context(|| format!("GET {url}"))?;
    match res.status().as_u16() {
        404 => Ok(None),
        200 => Ok(Some(res.body_mut().read_json().with_context(|| format!("GET {url}"))?)),
        status => anyhow::bail!("GET {url}: HTTP {status}"),
    }
}

/// Body at `url`.
pub fn download(url: &str) -> Result<Vec<u8>> {
    let mut res = AGENT.get(url).call().with_context(|| format!("GET {url}"))?;
    if res.status() != 200 {
        anyhow::bail!("GET {url}: HTTP {}", res.status().as_u16());
    }
    res.body_mut().with_config().limit(1 << 30).read_to_vec().with_context(|| format!("GET {url}"))
}

/// Relative package directory, without traversal.
fn subdir(dir: &str) -> Option<String> {
    let dir = dir.trim_matches('/');
    (!dir.is_empty() && !dir.split('/').any(|s| s == ".." || s == ".")).then(|| dir.to_string())
}

/// Https url of a repository field (`git+https://…/x.git`, `github:o/r`, `git@host:o/r`, …).
/// Other schemes are dropped: registry metadata must not choose the git transport.
pub fn normalize_repo(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("git+").unwrap_or(raw);

    let url = if let Some((scheme, rest)) = raw.split_once("://") {
        let rest = rest.split_once('@').filter(|(user, _)| !user.contains('/')).map_or(rest, |(_, r)| r);
        match scheme {
            "https" | "http" | "git" | "ssh" => format!("https://{rest}"),
            _ => return None,
        }
    } else if let Some((host, path)) = raw.strip_prefix("git@").and_then(|r| r.split_once(':')) {
        format!("https://{host}/{path}")
    } else {
        let (host, path) = match raw.split_once(':') {
            Some(("github", path)) => ("github.com", path),
            Some(("gitlab", path)) => ("gitlab.com", path),
            Some(("bitbucket", path)) => ("bitbucket.org", path),
            None if raw.split('/').count() == 2 => ("github.com", raw),
            _ => return None,
        };
        format!("https://{host}/{path}")
    };

    let url = url.split(['#', '?']).next()?;

    // Strip web UI paths: github `/tree/<ref>/…`, gitlab `/-/tree/<ref>/…`.
    let url = url.split("/-/").next()?;
    let url = match url.find("/tree/").or_else(|| url.find("/blob/")) {
        Some(i) => &url[..i],
        None => url,
    };

    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let path = url.strip_prefix("https://")?;

    let valid = path.split('/').count() >= 2
        && !path.split('/').any(|s| s.is_empty() || s == ".." || s.starts_with('-'))
        && path.chars().all(|c| c.is_ascii_alphanumeric() || "-._~/:".contains(c));

    valid.then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize() {
        for (raw, url) in [
            ("git+https://github.com/colinhacks/zod.git", Some("https://github.com/colinhacks/zod")),
            ("git://github.com/a/b.git", Some("https://github.com/a/b")),
            ("git+ssh://git@github.com/a/b.git", Some("https://github.com/a/b")),
            ("git@gitlab.com:a/b.git", Some("https://gitlab.com/a/b")),
            ("github:a/b", Some("https://github.com/a/b")),
            ("a/b", Some("https://github.com/a/b")),
            ("https://github.com/tokio-rs/tokio/tree/master/tokio-util", Some("https://github.com/tokio-rs/tokio")),
            ("https://gitlab.com/g/sub/p/-/tree/main", Some("https://gitlab.com/g/sub/p")),
            ("https://github.com/a/b#readme", Some("https://github.com/a/b")),
            ("ext::sh -c touch% /tmp/x", None),
            ("file:///etc/x/y", None),
            ("https://github.com/--upload-pack=x/b", None),
            ("https://github.com/a/b c", None),
        ] {
            assert_eq!(normalize_repo(raw).as_deref(), url, "{raw}");
        }
    }
}
