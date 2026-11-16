//! `bun.lock` (text format, up to v2): `"<path>": ["<name>@<version>", …]` under `packages`. The
//! path is the bare name when hoisted to the root, `<member package>/<name>` for a member's own
//! copy.

use anyhow::{Result, bail};

use super::{Entry, Member, entry, split_version};

pub(super) fn entries(text: &str, name: &str, member: &Member) -> Result<Vec<Entry>> {
    let version = text.lines().find_map(|l| l.trim().strip_prefix("\"lockfileVersion\":"));
    let version = version.map_or("unknown", |v| v.trim().trim_end_matches(','));

    if !matches!(version, "0" | "1" | "2") {
        bail!("unsupported lockfile version {version}, expected 2 or lower");
    }

    let found: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.trim().strip_prefix('"')?.split_once("\": [\"")?;
            let (n, v) = split_version(rest.split('"').next()?)?;
            (n == name).then_some((key, v))
        })
        .collect();

    let own = member.name.as_ref().map(|m| format!("{m}/{name}")).filter(|own| found.iter().any(|(k, _)| k == own));
    let direct = own.as_deref().unwrap_or(name);
    Ok(found.into_iter().filter_map(|(key, v)| entry(v, key == direct)).collect())
}
