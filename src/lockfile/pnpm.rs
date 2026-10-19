//! `pnpm-lock.yaml` (v6, v9): direct versions are under the member's `importers` entry (or top-level
//! `dependencies` in v6), all versions are `packages` keys.
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

use super::{Entry, Member, entry, split_version, unquote};

pub(super) fn entries(text: &str, name: &str, member: &Member) -> Vec<Entry> {
    let want = if member.dir.is_empty() { "." } else { &member.dir };
    let mut out = Vec::new();
    let mut section = "";
    // v6 single projects list their dependencies at the top level, as the root importer.
    let mut importer = ".";
    // Indent of a matched dependency key awaiting its `version:` line.
    let mut pending: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();

        if indent == 0 {
            section = trimmed.trim_end_matches(':');
            (importer, pending) = (".", None);
            continue;
        }

        match section {
            "packages" if indent == 2 => {
                let key = unquote(trimmed.trim_end_matches(':')).trim_start_matches('/');
                if let Some((n, v)) = split_version(key)
                    && n == name
                {
                    out.extend(entry(v, false));
                }
            }
            "packages" | "snapshots" => {}
            "importers" if indent == 2 => {
                (importer, pending) = (unquote(trimmed.trim_end_matches(':')), None);
            }
            _ => {
                if let Some(at) = pending {
                    if indent > at {
                        if let Some(v) = trimmed.strip_prefix("version:") {
                            out.extend(entry(v, importer == want));
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
                        v => out.extend(entry(v, importer == want)),
                    }
                }
            }
        }
    }

    out
}
