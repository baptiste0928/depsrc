//! Thin wrappers over the git CLI.

use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

fn git(dir: Option<&Path>, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");

    if let Some(dir) = dir {
        cmd.arg("-C").arg(dir);
    }

    // Agents cannot answer prompts. An empty GIT_ASKPASS also disables core.askPass and SSH_ASKPASS.
    cmd.args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GCM_INTERACTIVE", "never");

    if std::env::var_os("GIT_SSH_COMMAND").is_none() && std::env::var_os("GIT_SSH").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }

    let out = cmd.stdin(Stdio::null()).output().context("run git")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The cause, not the hints git prints after it.
        let lines = || stderr.lines().map(str::trim);

        let msg = lines()
            .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
            .or_else(|| lines().rfind(|l| !l.is_empty()))
            .unwrap_or("failed");

        bail!("git {}: {}", args.first().unwrap_or(&""), msg.trim());
    }

    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Full commit id, validated: safe to pass to git.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Commit(String);

impl Commit {
    pub fn parse(s: &str) -> Option<Commit> {
        (s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Commit(s.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Commit {
    type Error = String;

    fn try_from(s: String) -> Result<Commit, String> {
        Commit::parse(&s).ok_or_else(|| format!("invalid commit `{s}`"))
    }
}

impl From<Commit> for String {
    fn from(commit: Commit) -> String {
        commit.0
    }
}

impl fmt::Display for Commit {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Blobless bare clone: full history, file contents fetched on demand.
pub fn clone(url: &str, dir: &Path) -> Result<()> {
    let dir = dir.to_str().context("non UTF-8 store path")?;
    git(None, &["clone", "-q", "--bare", "--filter=blob:none", "--", url, dir])?;
    Ok(())
}

/// Fetches `refspecs` from origin.
pub fn fetch(repo: &Path, refspecs: &[&str]) -> Result<()> {
    let mut args = vec!["fetch", "-q", "origin"];
    args.extend(refspecs);
    git(Some(repo), &args)?;
    Ok(())
}

/// Commit of `rev`, if it exists locally.
pub fn resolve(repo: &Path, rev: &str) -> Option<Commit> {
    let out = git(Some(repo), &["rev-parse", "-q", "--verify", "--end-of-options", &format!("{rev}^{{commit}}")]);
    Commit::parse(&out.ok()?)
}

/// Branch the remote HEAD pointed to at clone time.
pub fn head_branch(repo: &Path) -> Result<String> {
    git(Some(repo), &["symbolic-ref", "--short", "HEAD"])
}

/// Adds a detached worktree at `rev`.
pub fn worktree_add(repo: &Path, tree: &Path, rev: &str) -> Result<()> {
    let tree = tree.to_str().context("non UTF-8 store path")?;
    git(Some(repo), &["worktree", "add", "-q", "--detach", tree, rev])?;
    Ok(())
}

/// Drops worktree metadata whose directory is gone.
pub fn worktree_prune(repo: &Path) -> Result<()> {
    git(Some(repo), &["worktree", "prune"])?;
    Ok(())
}

/// Moves a worktree to `rev`, discarding local changes.
pub fn checkout(tree: &Path, rev: &str) -> Result<()> {
    git(Some(tree), &["checkout", "-q", "--force", "--detach", rev])?;
    Ok(())
}
