//! depsrc: fetches dependency sources with their git history, for agents to read.
//!
//! ```text
//! path: spec ─ resolve ─> targets ─ checkout ─> tree path, recorded in the store
//! list: store ─ project's specs ─> specs as requested
//! clean: store ─ unused trees ─> removed, with orphan repos
//! ```

mod archive;
mod checkout;
mod clean;
mod git;
mod lockfile;
mod registry;
mod resolve;
mod spec;
mod store;

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::checkout::Target;
use crate::resolve::Pinned;
use crate::spec::Spec;
use crate::store::{Db, Pkg, Store};

const ABOUT: &str = "Fetch dependency sources with their git history, for agents to read.";

const LONG_ABOUT: &str = "\
Fetch dependency sources with their git history, for agents to read.

`depsrc path <spec>` prints the directory of a dependency's source, at the version locked by the
current project. Read it to learn how the dependency works; use `git log` and `git blame` there to
understand why. Git checkouts are blobless clones: file contents of old commits are fetched on
demand, so history commands need network.

Specs: crate:serde, npm:@types/node, gh:owner/repo, git:https://host/repo. Registry packages take an
optional @<version> (crate:serde@1.0.210, npm:zod@next), else use the project lockfile (Cargo.lock,
pnpm-lock.yaml, package-lock.json, yarn.lock, bun.lock), else the latest release. Any spec takes a
#<ref> instead: a git tag, branch or commit of its repository (gh:jj-vcs/jj#v0.30.0, npm:zod#main).
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
    /// Run as if started in this directory
    #[arg(short = 'C', long, global = true)]
    cwd: Option<PathBuf>,
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
    /// List dependencies fetched from the current project, as requested. `depsrc path $(depsrc list)`
    /// prints their current directories
    List {
        /// List dependencies of all projects
        #[arg(long)]
        all: bool,
    },
    /// Remove sources unused for a while
    Clean {
        /// Remove this dependency only, regardless of --since
        #[arg(value_name = "SPEC")]
        spec: Option<Spec>,
        /// Unused for this long: 30d, 12h, 0s…
        #[arg(long, default_value = "30d", value_parser = clean::parse_duration)]
        since: u64,
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
    let cwd = match cli.cwd {
        Some(dir) => dir,
        None => std::env::current_dir()?,
    };

    let cwd = cwd.canonicalize().with_context(|| format!("{}", cwd.display()))?;
    let store = Store::open(cli.store)?;
    let mut out = std::io::stdout().lock();

    match cli.command {
        Command::Path { specs, refetch } => {
            let mut code = ExitCode::SUCCESS;
            for spec in &specs {
                match path(&store, &cwd, spec, refetch) {
                    Ok(path) => writeln!(out, "{}", path.display())?,
                    Err(e) => {
                        eprintln!("error: {e:#}");
                        code = ExitCode::FAILURE;
                    }
                }
            }

            Ok(code)
        }
        Command::List { all } => {
            let db = store.read()?;
            let project = lockfile::project(&cwd);

            let specs: BTreeSet<&String> =
                db.projects.iter().filter(|(p, _)| all || **p == project).flat_map(|(_, specs)| specs.keys()).collect();

            for spec in specs {
                writeln!(out, "{spec}")?;
            }

            Ok(ExitCode::SUCCESS)
        }
        Command::Clean { spec, since } => {
            let (trees, repos) = clean::clean(&store, spec.as_ref(), since)?;
            writeln!(out, "removed {trees} trees, {repos} repos")?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Fetches `spec` and records it for the project around `cwd`.
fn path(store: &Store, cwd: &Path, spec: &Spec, refetch: bool) -> Result<PathBuf> {
    let pinned = resolve::pin(spec, cwd)?;
    let cached = store.read()?.pkgs.remove(&pinned.spec.to_string()).map(|p| p.target);
    let (key, pkg, failures) = fetch(store, pinned, cached, refetch)?;
    let project = lockfile::project(cwd);
    store.update(|db| record(db, project, spec, key.clone(), pkg.clone()))?;
    if pkg.target.source.is_archive() {
        let why = if failures.is_empty() { String::new() } else { format!(" ({})", failures.join("; ")) };
        eprintln!("note: {key}: registry archive, no history{why}");
    }

    Ok(store.pkg(&pkg))
}

/// Checks out the first target of `pinned` that works. Returns its exact spec, and the failures of
/// better targets.
fn fetch(store: &Store, pinned: Pinned, cached: Option<Target>, refetch: bool) -> Result<(String, Pkg, Vec<String>)> {
    let (spec, targets) = match cached.or(pinned.target) {
        Some(target) => (pinned.spec, vec![target]),
        None => resolve::targets(&pinned.spec)?,
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

/// Records `pkg` as used by `project`.
fn record(db: &mut Db, project: PathBuf, requested: &Spec, key: String, pkg: Pkg) {
    if let Some(tree) = db.trees.get_mut(&pkg.tree) {
        tree.used = store::now();
    }

    db.projects.entry(project).or_default().insert(requested.to_string(), key.clone());
    db.pkgs.insert(key, pkg);
}
