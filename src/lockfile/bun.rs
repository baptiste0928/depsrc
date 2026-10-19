//! `bun.lock` (text format): `"<path>": ["<name>@<version>", …]` under `packages`. The path is the
//! bare name when hoisted to the root, `<member package>/<name>` for a member's own copy.

use super::{Entry, Member, entry, split_version};

pub(super) fn entries(text: &str, name: &str, member: &Member) -> Vec<Entry> {
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
    found.into_iter().filter_map(|(key, v)| entry(v, key == direct)).collect()
}
