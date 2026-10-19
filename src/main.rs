//! depsrc: fetches dependency sources with their git history, for agents to read.
//!
//! ```text
//! path: spec ─ resolve ─> targets ─ checkout ─> tree path, recorded in the store
//! ```

mod archive;
mod checkout;
mod git;
mod registry;
mod resolve;
mod spec;
mod store;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::checkout::Target;
use crate::spec::Spec;
use crate::store::{Pkg, Store};

const ABOUT: &str = "Fetch dependency sources with their git history, for agents to read.";

const LONG_ABOUT: &str = "\
Fetch dependency sources with their git history, for agents to read.

`depsrc path <spec>` prints the directory of a dependency's source. Read it to learn how the
dependency works; use `git log` and `git blame` there to understand why. Git checkouts are blobless clones: file contents of old commits are fetched on
demand, so history commands need network.

Specs: crate:serde, npm:@types/node, gh:owner/repo, git:https://host/repo. Registry packages take an
optional @<version> (crate:serde@1.0.210, npm:zod@next), else use the latest release. Any spec
takes a #<ref> instead: a git tag, branch or commit of its repository (gh:jj-vcs/jj#v0.30.0, npm:zod#main).
Without one, gh: and git: specs use the default branch.

When the repository or published commit is unknown, depsrc falls back to the registry archive,
without history, and says so on stderr. Do not edit fetched sources: they are shared.

The git commit of a registry version is the one its publisher declared: the published archive may
differ from it.";

#[derive(Parser)]
#[command(version, about = ABOUT, long_about = LONG_ABOUT)]
struct Cli {
    /// Store directory [default: $DEPSRC_STORE, else $XDG_CACHE_HOME/depsrc]
    #[arg(short = 'S', long, global = true)]
    store: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch dependencies and print their source directories, one per line
    Path {
        #[arg(required = true, value_name = "SPEC")]
        specs: Vec<Spec>,
        /// Refetch branches, otherwise refreshed once a day
        #[arg(long)]
        refetch: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let store = Store::open(cli.store)?;
    let mut out = std::io::stdout().lock();

    match cli.command {
        Command::Path { specs, refetch } => {
            let mut code = ExitCode::SUCCESS;
            for spec in &specs {
                match path(&store, spec, refetch) {
                    Ok(path) => writeln!(out, "{}", path.display())?,
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        code = ExitCode::FAILURE;
                    }
                }
            }

            Ok(code)
        }
    }
}

/// Fetches `spec` and records it.
fn path(store: &Store, spec: &Spec, refetch: bool) -> Result<PathBuf> {
    let pinned = resolve::pin(spec)?;
    let cached = store.read()?.pkgs.remove(&pinned.to_string()).map(|p| p.target);
    let (key, pkg, failures) = fetch(store, pinned, cached, refetch)?;
    store.update(|db| db.pkgs.insert(key.clone(), pkg.clone()))?;
    if pkg.target.source.is_archive() {
        let why = if failures.is_empty() { String::new() } else { format!(" ({})", failures.join("; ")) };
        eprintln!("note: {key}: registry archive, no history{why}");
    }

    Ok(store.pkg(&pkg))
}

/// Checks out the first target of `pinned` that works. Returns its exact spec, and the failures of
/// better targets.
fn fetch(store: &Store, pinned: Spec, cached: Option<Target>, refetch: bool) -> Result<(String, Pkg, Vec<String>)> {
    let (spec, targets) = match cached {
        Some(target) => (pinned, vec![target]),
        None => resolve::targets(&pinned)?,
    };

    let key = spec.to_string();
    let mut errors = Vec::new();
    for target in targets {
        match checkout::checkout(store, &target.source, refetch) {
            Ok(tree) => {
                return Ok((key, Pkg { target, tree }, errors));
            }
            Err(e) => errors.push(format!("{e:#}")),
        }
    }

    anyhow::bail!("{key}: {}", errors.join("; "))
}
