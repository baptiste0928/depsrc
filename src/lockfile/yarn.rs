//! `yarn.lock` (v1, berry): entries are keyed by descriptors (`zod@^3.22.0`), and direct ones
//! match a range declared in a `package.json`.

use std::path::Path;

use serde_json::Value;

use super::{DEPS, Entry, entry, split_version};

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

/// Descriptors of `name` declared by the closest `package.json` doing so, from `cwd` up to `root`.
pub(super) fn wanted(cwd: &Path, root: &Path, name: &str) -> Vec<String> {
    let declared = |dir: &Path| {
        let pkg: Value = serde_json::from_slice(&std::fs::read(dir.join("package.json")).ok()?).ok()?;
        let ranges = DEPS.into_iter().filter_map(|deps| pkg[deps][name].as_str());
        let out: Vec<String> = ranges.flat_map(|r| [format!("{name}@{r}"), format!("{name}@npm:{r}")]).collect();
        (!out.is_empty()).then_some(out)
    };

    cwd.ancestors().take_while(|dir| dir.starts_with(root)).find_map(declared).unwrap_or_default()
}
