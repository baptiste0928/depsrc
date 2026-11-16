//! `pnpm-lock.yaml` (v6, v9, v13): direct versions are under the member's project, all versions are
//! `packages` keys. A project pinning pnpm has a YAML document locking pnpm itself before the
//! lockfile, split by `---` lines.
//!
//! v6 and v9 are YAML, with projects under `importers` (or top-level `dependencies` in v6):
//!
//! ```yaml
//! importers:
//!   .:
//!     dependencies:
//!       zod:
//!         specifier: ^3.23.0
//!         version: 3.23.8
//! packages:
//!   zod@3.23.8:            # v6: /zod@3.23.8, with (peer@x) suffixes
//! ```
//!
//! v13 is JSON with trailing commas, with projects under `projects`. A dependency is its specifier
//! alone when the version follows from it:
//!
//! ```json
//! "projects": {
//!     ".": {
//!         "dependencies": {
//!             "is-odd": "3.0.1",
//!             "zod": {"specifier": "^3.23.0", "version": "3.23.8",},
//!         },
//!     },
//! },
//! "packages": {
//!     "zod@3.23.8": {},
//! },
//! ```

use anyhow::{Result, bail};
use serde_json::Value;

use super::{DEPS, Entry, Member, entry, split_version, unquote};

pub(super) fn entries(text: &str, name: &str, member: &Member) -> Result<Vec<Entry>> {
    let project = if member.dir.is_empty() { "." } else { &member.dir };

    let text = text.rsplit_once("\n---\n").map_or(text, |(_, lockfile)| lockfile);
    let text = text.strip_prefix("---\n").unwrap_or(text);

    let json: Option<Value> = if text.trim_start().starts_with('{') {
        Some(serde_json::from_str(&strip_trailing_commas(text))?)
    } else {
        None
    };

    let version = match &json {
        Some(lock) => lock["lockfileVersion"].as_str(),
        None => text.lines().find_map(|l| l.strip_prefix("lockfileVersion:")).map(|v| unquote(v.trim())),
    };

    match (version.and_then(|v| v.split('.').next()), &json) {
        (Some("13"), Some(lock)) => Ok(json_entries(lock, name, project)),
        (Some("6" | "9"), None) => Ok(yaml_entries(text, name, project)),
        _ => bail!("unsupported lockfile version {}, expected 6, 9 or 13", version.unwrap_or("unknown")),
    }
}

fn yaml_entries(text: &str, name: &str, project: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut section = "";
    let mut importer = ".";
    // Field of the importer: a `DEPS` one, `dependenciesMeta`…
    let mut field = "";
    // Indent of a matched dependency key awaiting its `version:` line.
    let mut pending: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        let header = unquote(trimmed.trim_end_matches(':'));

        match (section, indent) {
            (_, 0) => {
                (section, importer, pending) = (header, ".", None);
                continue;
            }
            ("packages", 2) => {
                if let Some((n, v)) = split_version(header.trim_start_matches('/'))
                    && n == name
                {
                    out.extend(entry(v, false));
                }
                continue;
            }
            ("importers", 2) => {
                (importer, field, pending) = (header, "", None);
                continue;
            }
            ("importers", 4) => {
                (field, pending) = (header, None);
                continue;
            }
            ("importers", _) if DEPS.contains(&field) => {}
            // v6 single projects list their dependencies at the top level, as the root importer.
            (section, _) if DEPS.contains(&section) => {}
            _ => continue,
        }

        if let Some(at) = pending {
            if indent > at {
                if let Some(v) = trimmed.strip_prefix("version:") {
                    out.extend(entry(v, importer == project));
                    pending = None;
                }
                continue;
            }
            pending = None;
        }

        if let Some((key, value)) = trimmed.split_once(':')
            && unquote(key) == name
        {
            match value.trim() {
                "" => pending = Some(indent),
                v => out.extend(entry(v, importer == project)),
            }
        }
    }

    out
}

fn json_entries(lock: &Value, name: &str, project: &str) -> Vec<Entry> {
    let direct = DEPS.into_iter().filter_map(|deps| {
        let dep = &lock["projects"][project][deps][name];
        entry(dep.as_str().or_else(|| dep["version"].as_str())?, true)
    });

    let all = lock["packages"].as_object().into_iter().flatten().filter_map(|(key, _)| {
        let (n, v) = split_version(key)?;
        (n == name).then(|| entry(v, false))?
    });

    direct.chain(all).collect()
}

/// `text` without the trailing commas `serde_json` rejects.
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut chars = text.char_indices();

    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if in_string => {
                out.push(c);
                out.extend(chars.next().map(|(_, escaped)| escaped));
                continue;
            }
            '"' => in_string = !in_string,
            ',' if !in_string && text[i + 1..].trim_start().starts_with(['}', ']']) => continue,
            _ => {}
        }

        out.push(c);
    }

    out
}
