//! `yarn.lock` (v1, berry): entries are keyed by descriptors (`zod@^3.22.0`), and direct ones
//! match a range declared in a `package.json`.

use std::path::Path;

use super::{Entry, entry, split_version};

pub(super) fn entries(text: &str, name: &str, wanted: &[String]) -> Vec<Entry> {
    let mut out = Vec::new();
    // Whether the current block is `name`, and is direct.
    let mut current: Option<bool> = None;

    for line in text.lines() {
        if !line.starts_with([' ', '#']) && line.ends_with(':') {
            let header = line.trim_end_matches(':').replace('"', "");
            let descriptors: Vec<&str> =
                header.split(", ").filter(|d| split_version(d).is_some_and(|(n, _)| n == name)).collect();
            current = (!descriptors.is_empty()).then(|| descriptors.iter().any(|d| wanted.iter().any(|w| w == d)));
        } else if let Some(direct) = current
            && let Some(v) = line.trim().strip_prefix("version")
        {
            out.extend(entry(v.trim_start_matches(':'), direct));
        }
    }

    out
}

/// Descriptors of `name` declared by the `package.json` files from `cwd` up to `root`.
pub(super) fn wanted(cwd: &Path, root: &Path, name: &str) -> Vec<String> {
    let mut out = Vec::new();

    for dir in cwd.ancestors() {
        if let Ok(text) = std::fs::read_to_string(dir.join("package.json"))
            && let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&text)
        {
            for deps in ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"] {
                if let Some(range) = pkg.get(deps).and_then(|d| d.get(name)).and_then(|r| r.as_str()) {
                    out.push(format!("{name}@{range}"));
                    out.push(format!("{name}@npm:{range}"));
                }
            }
        }

        if dir == root {
            break;
        }
    }

    out
}
