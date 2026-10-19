//! `package-lock.json` (v2, v3): direct means in the member's `node_modules`, else hoisted to the
//! root one.

use anyhow::Result;
use serde_json::Value;

use super::{Entry, Locked, Member, entry};

pub(super) fn entries(text: &str, name: &str, member: &Member) -> Result<Vec<Entry>> {
    let lock: Value = serde_json::from_str(text)?;

    let Some(pkgs) = lock.get("packages").and_then(Value::as_object) else {
        anyhow::bail!("unsupported lockfile version, expected v2 or v3");
    };

    let top = format!("node_modules/{name}");
    let nested = format!("/node_modules/{name}");
    // Node resolves the member's own copy first, then the hoisted one.
    let own = format!("{}{nested}", member.dir);
    let direct = if !member.dir.is_empty() && pkgs.contains_key(&own) { own } else { top.clone() };

    Ok(pkgs
        .iter()
        .filter(|(key, _)| *key == &top || key.ends_with(&nested))
        .filter_map(|(key, pkg)| match pkg.get("name").and_then(Value::as_str) {
            Some(alias) if alias != name => {
                Some(Entry { locked: Locked::Foreign(format!("alias of {alias}")), direct: **key == direct })
            }
            _ => entry(pkg.get("version")?.as_str()?, **key == direct),
        })
        .collect())
}
