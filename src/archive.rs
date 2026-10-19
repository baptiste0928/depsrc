//! Registry tarballs (`.crate`, npm `.tgz`), handled by the tar CLI.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Extracts a gzipped tarball into `dest`, dropping its single top-level directory.
/// Repository metadata is skipped: a crafted `.git` would run its hooks and config in agents' git
/// commands.
pub fn extract(bytes: &[u8], dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;

    let mut cmd = Command::new("tar");
    cmd.args(["-xzf", "-", "--strip-components=1", "--no-same-owner", "--exclude=.git", "--exclude=.jj", "-C"])
        .arg(dest);

    tar(cmd, bytes)?;
    Ok(())
}

/// Content of the `member` file in a gzipped tarball.
pub fn read(bytes: &[u8], member: &str) -> Option<Vec<u8>> {
    let mut cmd = Command::new("tar");
    cmd.args(["-xzOf", "-", "--", member]);
    tar(cmd, bytes).ok()
}

fn tar(mut cmd: Command, input: &[u8]) -> Result<Vec<u8>> {
    let mut child =
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("run tar")?;

    let mut stdin = child.stdin.take().expect("piped stdin");

    let out = std::thread::scope(|s| {
        s.spawn(move || stdin.write_all(input));
        child.wait_with_output()
    })?;

    if !out.status.success() {
        bail!("tar: {}", String::from_utf8_lossy(&out.stderr).trim());
    }

    Ok(out.stdout)
}
