//! `Cargo.lock`: direct dependencies are those of workspace members, which have no `source`.

use super::{Entry, Locked, unquote};
use crate::git::Commit;

const CRATES_IO: [&str; 2] =
    ["registry+https://github.com/rust-lang/crates.io-index", "sparse+https://index.crates.io/"];

#[derive(Default)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    deps: Vec<String>,
}

pub(super) fn entries(text: &str, name: &str) -> Vec<Entry> {
    let pkgs = parse(text);

    // Members list `name` alone when a single version is locked, `name version` otherwise.
    let direct: Vec<Option<&str>> = pkgs
        .iter()
        .filter(|p| p.source.is_none())
        .flat_map(|p| &p.deps)
        .filter_map(|d| {
            let mut parts = d.split(' ');
            parts.next().is_some_and(|n| same_crate(n, name)).then(|| parts.next())
        })
        .collect();

    pkgs.iter()
        .filter(|p| same_crate(&p.name, name))
        .filter_map(|p| {
            let source = p.source.as_deref()?;

            let locked = match source.strip_prefix("git+") {
                Some(git) => {
                    let (url, rev) = git.split_once('#')?;
                    let url = url.split('?').next()?;
                    if !(url.starts_with("https://") || url.starts_with("ssh://")) {
                        return None;
                    }
                    Locked::Git { url: url.to_string(), rev: Commit::parse(rev)? }
                }
                None if CRATES_IO.contains(&source) => Locked::Version(p.version.clone()),
                None => Locked::Foreign(source.to_string()),
            };

            let direct = direct.iter().any(|v| v.is_none_or(|v| v == p.version));
            Some(Entry { locked, direct })
        })
        .collect()
}

/// Whether two crate names are the same: crates.io treats `-` and `_` alike.
fn same_crate(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes().zip(b.bytes()).all(|(a, b)| a == b || (a, b) == (b'-', b'_') || (a, b) == (b'_', b'-'))
}

fn parse(text: &str) -> Vec<Package> {
    let mut pkgs: Vec<Package> = Vec::new();
    let mut in_deps = false;

    for line in text.lines().map(str::trim) {
        if line == "[[package]]" {
            pkgs.push(Package::default());
            in_deps = false;
            continue;
        }

        let Some(pkg) = pkgs.last_mut() else { continue };

        if in_deps {
            match line {
                "]" => in_deps = false,
                dep => pkg.deps.push(unquote(dep.trim_end_matches(',')).to_string()),
            }
            continue;
        }

        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };

        match key {
            "name" => pkg.name = unquote(value).to_string(),
            "version" => pkg.version = unquote(value).to_string(),
            "source" => pkg.source = Some(unquote(value).to_string()),
            "dependencies" if value == "[" => in_deps = true,
            "dependencies" => {
                let items = value.trim_start_matches('[').trim_end_matches(']');
                pkg.deps.extend(items.split(',').map(|d| unquote(d.trim()).to_string()).filter(|d| !d.is_empty()));
            }
            _ => {}
        }
    }

    pkgs
}
