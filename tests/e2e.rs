//! End-to-end tests of the `depsrc` binary, offline:
//! - git remotes are local repos reached as `https://git.test/<name>` or `https://github.com/<name>`,
//!   rewritten to `file://` by a test `GIT_CONFIG_GLOBAL`;
//! - registries are canned responses from a std-only HTTP server, selected with
//!   `DEPSRC_CRATES_URL` and `DEPSRC_NPM_URL`;
//! - each test has its own store and directories under `CARGO_TARGET_TMPDIR`.
//!
//! Tests against the real registries are ignored by default: `cargo test -- --ignored`.

#![allow(clippy::too_many_lines, reason = "fixtures")]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

#[test]
fn crate_commit_in_monorepo() {
    let env = Env::new("crate_commit_in_monorepo");
    let repo = env.repo("tokio");
    repo.commit(&[("tokio-util/lib.rs", "v1")]);
    let published = repo.commit(&[("tokio-util/lib.rs", "v2")]);
    let next = repo.commit(&[("tokio-util/lib.rs", "v3")]);

    env.crate_("tokio-util", "0.7.0", Some("https://git.test/tokio.git"), Some((&published, "tokio-util")));
    env.crate_("tokio-util", "0.8.0", Some("https://git.test/tokio.git"), Some((&next, "tokio-util")));

    // Leftover of an interrupted clone.
    fs::create_dir_all(env.store.join("repos/.tmp-git.test--tokio/objects")).unwrap();
    let path = env.ok(&["path", "crate:tokio-util@0.7.0"]);
    assert!(path.ends_with(&format!("/trees/git.test--tokio@{}/tokio-util", &published[..8])), "{path}");
    assert_eq!(fs::read_to_string(Path::new(&path).join("lib.rs")).unwrap(), "v2");
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), published);
    assert_eq!(git(&path, &["rev-list", "--count", "HEAD"]), "2");
    let repo_dir = env.store.join("repos/git.test--tokio");
    assert_eq!(git(&repo_dir, &["config", "remote.origin.partialclonefilter"]), "blob:none");
    assert_eq!(env.ok(&["path", "--root", "crate:tokio-util@0.7.0"]), path.strip_suffix("/tokio-util").unwrap());

    // Another version: its own tree, sharing the repo.
    let other = env.ok(&["path", "crate:tokio-util@0.8.0"]);
    assert_eq!(fs::read_to_string(Path::new(&other).join("lib.rs")).unwrap(), "v3");
    assert_eq!(fs::read_to_string(Path::new(&path).join("lib.rs")).unwrap(), "v2");
    assert_eq!(fs::read_dir(env.store.join("repos")).unwrap().count(), 1);
}

#[test]
fn archive_fallback() {
    let env = Env::new("archive_fallback");
    let repo = env.repo("foo");
    let head = repo.commit(&[("lib.rs", "")]);

    env.crate_("foo", "1.0.0", Some("https://git.test/foo"), Some((&"a".repeat(40), "")));
    env.crate_("bad", "1.0.0", Some("https://git.test/foo"), Some(("--upload-pack=touch", "")));
    env.npm("nohead", "1.0.0", Some(&json!("https://git.test/foo")), None);
    env.npm("dotgit", "1.0.0", None, None);
    let files = [("package/package.json", "{}"), ("package/.git/config", "[core]\n\tfsmonitor = touch pwned\n")];
    let files: Vec<(String, String)> = files.iter().map(|(p, c)| (p.to_string(), c.to_string())).collect();
    env.registry.serve("/tarballs/dotgit-1.0.0.tgz", env.tarball(&files));

    for (spec, tree, file) in [
        // Published commit missing upstream.
        ("crate:foo@1.0.0", "crate--foo@1.0.0", "Cargo.toml"),
        // Invalid commit, never passed to git.
        ("crate:bad@1.0.0", "crate--bad@1.0.0", "Cargo.toml"),
        ("npm:nohead@1.0.0", "npm--nohead@1.0.0", "package.json"),
        ("npm:dotgit@1.0.0", "npm--dotgit@1.0.0", "package.json"),
    ] {
        let out = env.run(&["path", spec]);
        let path = stdout(&out);
        assert!(path.ends_with(&format!("/trees/{tree}")), "{spec}: {path}");
        assert!(Path::new(&path).join(file).is_file(), "{spec}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("no history"), "{spec}");
    }

    // Repository metadata is not extracted.
    assert!(!env.store.join("trees/npm--dotgit@1.0.0/.git").exists());
    // The failed clone is not retried: the archive is cached.
    env.registry.down.store(true, Ordering::Relaxed);
    assert!(env.ok(&["path", "crate:foo@1.0.0"]).ends_with("/trees/crate--foo@1.0.0"));
    env.registry.down.store(false, Ordering::Relaxed);

    // Package directories cannot escape the tree.
    env.npm("escape", "1.0.0", Some(&json!({"url": "https://git.test/foo", "directory": "../.."})), Some(&head));
    assert!(env.ok(&["path", "npm:escape@1.0.0"]).ends_with(&format!("/trees/git.test--foo@{}", &head[..8])));

    // Metadata cannot downgrade downloads to plain http.
    let doc = json!({"version": "1.0.0", "dist": {"tarball": "http://evil.test/x.tgz"}});
    env.registry.serve("/npm/insecure/1.0.0", doc.to_string().into_bytes());
    let out = env.run(&["path", "npm:insecure@1.0.0"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("insecure tarball url"));
}

#[test]
fn tag_fallback() {
    let env = Env::new("tag_fallback");
    let repo = env.repo("mono");
    let manifest = "[package]\nname = \"jj-lib\"";
    let generic = repo.commit(&[("lib/Cargo.toml", manifest), ("packages/pkg/package.json", "{}")]);
    repo.git(&["tag", "v0.30.0"]);
    repo.git(&["tag", "-a", "-m", "release", "v2.0.0"]);
    let scoped = repo.commit(&[("packages/pkg/package.json", r#"{"name": "@scope/pkg"}"#)]);
    repo.git(&["tag", "-a", "-m", "release", "@scope/pkg@2.0.0"]);

    env.crate_("jj-lib", "0.30.0", Some("https://git.test/mono"), None);
    // No `directory`: found from package.json.
    let repository = json!({"url": "https://git.test/mono"});
    env.npm("@scope/pkg", "2.0.0", Some(&repository), None);

    // The crate directory is found from its manifest.
    let out = env.run(&["path", "crate:jj-lib@0.30.0"]);
    assert!(stdout(&out).ends_with(&format!("/trees/git.test--mono@{}/lib", &generic[..8])), "{}", stdout(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no published commit, using tag v0.30.0"));

    // The package's own tag wins over the generic one, annotated tags give their commit.
    let path = env.ok(&["path", "npm:@scope/pkg@2.0.0"]);
    assert!(path.ends_with(&format!("/trees/git.test--mono@{}/packages/pkg", &scoped[..8])), "{path}");

    // A tag published after the clone.
    let next = repo.commit(&[("lib/Cargo.toml", &format!("{manifest}\n# 0.31"))]);
    repo.git(&["tag", "v0.31.0"]);
    env.crate_("jj-lib", "0.31.0", Some("https://git.test/mono"), None);
    let path = env.ok(&["path", "crate:jj-lib@0.31.0"]);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), next);
}

#[test]
fn git_refs() {
    let env = Env::new("git_refs");
    let repo = env.repo("lib");
    let v1 = repo.commit(&[("a", "1")]);
    repo.git(&["tag", "v1"]);
    let main = repo.commit(&[("a", "2")]);
    repo.git(&["checkout", "-q", "-b", "feat/x"]);
    let feat = repo.commit(&[("a", "3")]);
    repo.git(&["checkout", "-q", "main"]);

    let gh = env.repo("org/lib");
    let gh_main = gh.commit(&[("b", "1")]);
    env.npm("libjs", "1.0.0", Some(&json!("git+https://git.test/lib.git")), None);

    let short = &v1[..10];

    for (spec, commit, tree) in [
        ("git:https://git.test/lib".to_string(), &main, "git.test--lib@main".to_string()),
        ("git:https://git.test/lib#v1".to_string(), &v1, "git.test--lib@v1".to_string()),
        ("git:https://git.test/lib#feat/x".to_string(), &feat, "git.test--lib@feat--x".to_string()),
        (format!("git:https://git.test/lib#{short}"), &v1, format!("git.test--lib@{short}")),
        ("gh:org/lib".to_string(), &gh_main, "github.com--org--lib@main".to_string()),
        // A ref in the package repository.
        ("npm:libjs#v1".to_string(), &v1, "git.test--lib@v1".to_string()),
    ] {
        let path = env.ok(&["path", &spec]);
        assert!(path.ends_with(&format!("/trees/{tree}")), "{spec}: {path}");
        assert_eq!(&git(&path, &["rev-parse", "HEAD"]), commit, "{spec}");
    }

    // A failing spec does not stop the others, which print in order.
    let out = env.run(&["path", "git:https://git.test/lib#v1", "git:https://git.test/lib#nope", "gh:org/lib"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ref `nope` not found"));
    let paths = String::from_utf8(out.stdout).unwrap();
    let paths: Vec<&str> = paths.lines().collect();
    assert!(paths[0].ends_with("/trees/git.test--lib@v1") && paths[1].ends_with("/trees/github.com--org--lib@main"));

    // Commits made after the clone, by prefix or full id.
    let later = repo.commit(&[("a", "4")]);
    let path = env.ok(&["path", &format!("git:https://git.test/lib#{}", &later[..10])]);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), later);
    let unreachable = repo.commit(&[("a", "5")]);
    repo.git(&["reset", "-q", "--hard", "HEAD~"]);
    let path = env.ok(&["path", &format!("git:https://git.test/lib#{unreachable}")]);
    assert!(path.ends_with(&format!("/trees/git.test--lib@{}", &unreachable[..8])), "{path}");

    // Versions are only looked up in the registry.
    let out = env.run(&["path", "npm:libjs@v1"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("version not found, use #<ref> for git refs"));
}

#[test]
fn branch_refresh() {
    let env = Env::new("branch_refresh");
    let repo = env.repo("lib");
    let first = repo.commit(&[("a", "1")]);
    let path = env.ok(&["path", "git:https://git.test/lib#main"]);
    let second = repo.commit(&[("a", "2")]);

    assert_eq!(env.ok(&["path", "git:https://git.test/lib#main"]), path);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), first);
    assert_eq!(env.ok(&["path", "--refetch", "git:https://git.test/lib#main"]), path);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), second);
    assert_eq!(fs::read_to_string(Path::new(&path).join("a")).unwrap(), "2");

    // Refetched once stale.
    let third = repo.commit(&[("a", "3")]);
    let index = env.store.join("sources.json");
    let mut db: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    db["trees"]["git.test--lib@main"]["fetched"] = json!(0);
    fs::write(&index, db.to_string()).unwrap();

    assert_eq!(env.ok(&["path", "git:https://git.test/lib#main"]), path);
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), third);

    // A branch created after the clone is followed too.
    repo.git(&["checkout", "-q", "-b", "later"]);
    repo.commit(&[("b", "1")]);
    let later = env.ok(&["path", "git:https://git.test/lib#later"]);
    let next = repo.commit(&[("b", "2")]);
    repo.git(&["checkout", "-q", "main"]);

    let mut db: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    db["trees"]["git.test--lib@later"]["fetched"] = json!(0);
    fs::write(&index, db.to_string()).unwrap();
    env.ok(&["path", "git:https://git.test/lib#later"]);
    assert_eq!(git(&later, &["rev-parse", "HEAD"]), next);

    // Offline, a stale branch keeps its tree.
    fs::rename(env.dir.join("remotes/lib"), env.dir.join("remotes/gone")).unwrap();
    let out = env.run(&["path", "--refetch", "git:https://git.test/lib#main"]);
    assert_eq!(stdout(&out), path);
    assert!(String::from_utf8_lossy(&out.stderr).contains("refetch failed, serving the last fetched tree"));
}

#[test]
fn cargo_lockfile() {
    let env = Env::new("cargo_lockfile");
    let repo = env.repo("gitdep");
    let rev = repo.commit(&[("crates/gitdep/Cargo.toml", "[package]\nname = \"gitdep\"")]);

    for (name, version) in [("foo", "0.1.0"), ("foo", "0.2.0"), ("baz", "1.0.0"), ("baz", "2.0.0"), ("one", "1.0.0")] {
        env.crate_(name, version, None, None);
    }

    let project = env.dir.join("project");
    fs::create_dir_all(project.join("member/src")).unwrap();

    fs::write(
        project.join("Cargo.lock"),
        format!(
            r#"# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "foo 0.2.0",
 "gitdep",
 "one",
 "private",
 "qux",
]

[[package]]
name = "baz"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "baz"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "foo"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = ["baz 1.0.0"]

[[package]]
name = "foo"
version = "0.2.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "gitdep"
version = "0.3.0"
source = "git+https://git.test/gitdep?branch=main#{rev}"

[[package]]
name = "one"
version = "1.0.0"
source = "sparse+https://index.crates.io/"

[[package]]
name = "private"
version = "1.0.0"
source = "sparse+https://corp.example/index/"

[[package]]
name = "qux"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "baz 2.0.0",
 "foo 0.1.0",
]
"#
        ),
    )
    .unwrap();

    let cwd = project.join("member/src");

    let path = env.ok_in(&cwd, &["path", "crate:foo", "crate:one", "crate:gitdep"]);
    let paths: Vec<&str> = path.lines().collect();
    assert!(paths[0].ends_with("/trees/crate--foo@0.2.0"), "{path}");
    assert!(paths[1].ends_with("/trees/crate--one@1.0.0"), "{path}");
    assert!(paths[2].ends_with(&format!("/trees/git.test--gitdep@{}/crates/gitdep", &rev[..8])), "{path}");

    assert!(env.complete(&["path", "crate:gitdep"]).contains(&format!("crate:gitdep#{rev}")));

    // Listed as requested: `path` follows lockfile updates.
    assert_eq!(env.ok_in(&cwd, &["list"]), "crate:foo\ncrate:gitdep\ncrate:one");
    let lock = fs::read_to_string(project.join("Cargo.lock")).unwrap();
    fs::write(project.join("Cargo.lock"), lock.replacen(" \"foo 0.2.0\",", " \"foo 0.1.0\",", 1)).unwrap();
    assert!(env.ok_in(&cwd, &["path", "crate:foo"]).ends_with("/trees/crate--foo@0.1.0"));

    let out = env.run_in(&cwd, &["path", "crate:baz"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("baz has several locked versions (1.0.0, 2.0.0)"));

    // Never looked up by name on crates.io.
    env.crate_("private", "1.0.0", None, None);
    let out = env.run_in(&cwd, &["path", "crate:private"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("locked from sparse+https://corp.example/index/"));
}

#[test]
fn npm_lockfiles() {
    let env = Env::new("npm_lockfiles");
    for version in ["3.22.0", "3.23.8"] {
        env.npm("zod", version, None, None);
        env.npm("@types/node", version, None, None);
    }

    let package_json = r#"{"dependencies": {"zod": "^3.23.0"}, "devDependencies": {"@types/node": "^3.23.0"}}"#;

    let lockfiles = [
        (
            "package-lock.json",
            r#"{"lockfileVersion": 3, "packages": {
                "": {"name": "app"},
                "node_modules/zod": {"version": "3.23.8"},
                "node_modules/@types/node": {"version": "3.23.8"},
                "node_modules/other/node_modules/zod": {"version": "3.22.0"},
                "node_modules/other/node_modules/@types/node": {"version": "3.22.0"}
            }}"#,
        ),
        (
            "pnpm-lock.yaml",
            "lockfileVersion: '9.0'

importers:

  .:
    dependencies:
      zod:
        specifier: ^3.23.0
        version: 3.23.8
    devDependencies:
      '@types/node':
        specifier: ^3.23.0
        version: 3.23.8(peer@1.0.0)

packages:

  zod@3.22.0:
    resolution: {integrity: sha512-x}

  zod@3.23.8:
    resolution: {integrity: sha512-x}

  '@types/node@3.22.0':
    resolution: {integrity: sha512-x}

  '@types/node@3.23.8':
    resolution: {integrity: sha512-x}

snapshots:

  zod@3.22.0: {}
",
        ),
        (
            "pnpm-lock.yaml",
            "lockfileVersion: '6.0'

dependencies:
  zod:
    specifier: ^3.23.0
    version: 3.23.8

devDependencies:
  '@types/node':
    specifier: ^3.23.0
    version: 3.23.8

packages:

  /zod@3.22.0:
    resolution: {integrity: sha512-x}
    dev: false

  /zod@3.23.8:
    resolution: {integrity: sha512-x}
    dev: false

  /@types/node@3.22.0(peer@1.0.0):
    resolution: {integrity: sha512-x}

  /@types/node@3.23.8:
    resolution: {integrity: sha512-x}
",
        ),
        (
            "yarn.lock",
            r#"# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.
# yarn lockfile v1


"@types/node@^3.0.0":
  version "3.22.0"
  resolved "https://registry.yarnpkg.com/@types/node/-/node-3.22.0.tgz"

"@types/node@^3.23.0":
  version "3.23.8"

zod@^3.0.0:
  version "3.22.0"

zod@^3.1.0, zod@^3.23.0:
  version "3.23.8"
"#,
        ),
        (
            "yarn.lock",
            r#"# This file is generated by running "yarn install" inside your project.

__metadata:
  version: 8
  cacheKey: 10

"@types/node@npm:^3.0.0":
  version: 3.22.0
  resolution: "@types/node@npm:3.22.0"

"@types/node@npm:^3.23.0":
  version: 3.23.8

"zod@npm:^3.0.0":
  version: 3.22.0

"zod@npm:^3.1.0, zod@npm:^3.23.0":
  version: 3.23.8
"#,
        ),
        (
            "bun.lock",
            r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": {
      "dependencies": {
        "zod": "^3.23.0",
      },
    },
  },
  "packages": {
    "zod": ["zod@3.23.8", "", {}, "sha512-x"],

    "@types/node": ["@types/node@3.23.8", "", {}, "sha512-x"],

    "other/zod": ["zod@3.22.0", "", {}, "sha512-x"],

    "other/@types/node": ["@types/node@3.22.0", "", {}, "sha512-x"],
  }
}
"#,
        ),
    ];

    for (i, (file, text)) in lockfiles.iter().enumerate() {
        let project = env.dir.join(format!("project{i}"));
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(file), text).unwrap();
        fs::write(project.join("package.json"), package_json).unwrap();

        let out = env.ok_in(&project, &["path", "npm:zod", "npm:@types/node"]);
        let paths: Vec<&str> = out.lines().collect();
        assert!(paths[0].ends_with("/trees/npm--zod@3.23.8"), "{file} #{i}: {out}");
        assert!(paths[1].ends_with("/trees/npm--@types--node@3.23.8"), "{file} #{i}: {out}");
    }
}

#[test]
fn npm_workspaces() {
    let env = Env::new("npm_workspaces");
    for version in ["3.22.0", "3.23.8"] {
        env.npm("zod", version, None, None);
    }

    let lockfiles = [
        (
            "package-lock.json",
            r#"{"lockfileVersion": 3, "packages": {
                "": {"name": "root"},
                "node_modules/zod": {"version": "3.23.8"},
                "packages/a/node_modules/zod": {"version": "3.22.0"}
            }}"#,
        ),
        (
            "pnpm-lock.yaml",
            "lockfileVersion: '9.0'

importers:

  .:
    dependencies:
      zod:
        specifier: ^3.23.0
        version: 3.23.8

  packages/a:
    dependencies:
      zod:
        specifier: ~3.22.0
        version: 3.22.0

packages:

  zod@3.22.0:
    resolution: {integrity: sha512-x}

  zod@3.23.8:
    resolution: {integrity: sha512-x}
",
        ),
        (
            "bun.lock",
            r#"{
  "lockfileVersion": 1,
  "packages": {
    "zod": ["zod@3.23.8", "", {}, "sha512-x"],

    "a/zod": ["zod@3.22.0", "", {}, "sha512-x"],
  }
}
"#,
        ),
    ];

    for (i, (file, text)) in lockfiles.iter().enumerate() {
        let root = env.dir.join(format!("project{i}"));
        let member = root.join("packages/a");
        fs::create_dir_all(member.join("src")).unwrap();
        fs::write(root.join(file), text).unwrap();
        fs::write(root.join("package.json"), r#"{"name": "root"}"#).unwrap();
        fs::write(member.join("package.json"), r#"{"name": "a"}"#).unwrap();

        // The member's own version, the root's elsewhere.
        assert!(env.ok_in(&member.join("src"), &["path", "npm:zod"]).ends_with("/npm--zod@3.22.0"), "{file}");
        assert!(env.ok_in(&root, &["path", "npm:zod"]).ends_with("/npm--zod@3.23.8"), "{file}");
    }
}

#[test]
fn cache_offline() {
    let env = Env::new("cache_offline");
    let repo = env.repo("foo");
    let rev = repo.commit(&[("lib.rs", "")]);
    env.crate_("foo", "1.0.0", Some("https://git.test/foo"), Some((&rev, "")));
    env.npm("bar", "1.0.0", None, None);
    let specs = ["path", "crate:foo@1.0.0", "npm:bar@1.0.0", "git:https://git.test/foo"];
    let first = env.ok(&specs);

    // Dist-tags move: they are resolved again.
    assert!(env.ok(&["path", "npm:bar@latest"]).ends_with("/npm--bar@1.0.0"));
    env.npm("bar", "2.0.0", None, None);
    assert!(env.ok(&["path", "npm:bar@latest"]).ends_with("/npm--bar@2.0.0"));

    env.registry.down.store(true, Ordering::Relaxed);
    fs::rename(env.dir.join("remotes/foo"), env.dir.join("remotes/gone")).unwrap();

    // A deleted tree is checked out again.
    fs::remove_dir_all(first.lines().next().unwrap()).unwrap();
    assert_eq!(env.ok(&specs), first);

    // Up-to-date trees need neither git nor tar.
    let out = env.command(&env.dir, &specs).env("PATH", "").output().unwrap();
    assert_eq!(stdout(&out), first);
}

#[test]
fn list_and_clean() {
    let env = Env::new("list_and_clean");
    let repo = env.repo("foo");
    let rev = repo.commit(&[("lib.rs", "")]);
    env.crate_("foo", "1.0.0", Some("https://git.test/foo"), Some((&rev, "")));
    env.npm("bar", "1.0.0", None, None);

    let (a, b) = (env.dir.join("a"), env.dir.join("b"));
    for project in [&a, &b] {
        fs::create_dir_all(project.join(".jj")).unwrap();
        fs::create_dir_all(project.join("sub")).unwrap();
    }

    let foo = env.ok_in(&a.join("sub"), &["path", "crate:foo@1.0.0"]);
    let head = env.ok_in(&a, &["path", "git:https://git.test/foo"]);
    let bar = env.ok_in(&b, &["path", "npm:bar@1.0.0"]);

    assert_eq!(env.ok_in(&a, &["list"]), "crate:foo@1.0.0\ngit:https://git.test/foo");
    assert_eq!(env.ok_in(&b, &["list"]), "npm:bar@1.0.0");
    assert_eq!(env.ok_in(&b, &["list", "--all"]).lines().count(), 3);

    let completions = env.complete(&["clean", "crate:"]);
    assert_eq!(completions, "crate:\ncrate:foo\ncrate:foo@1.0.0");

    assert_eq!(env.ok(&["clean"]), "removed 0 trees, 0 repos");

    // Serving a cached tree marks it used.
    let index = env.store.join("sources.json");
    let mut db: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    db["trees"]["npm--bar@1.0.0"]["used"] = json!(0);
    fs::write(&index, db.to_string()).unwrap();
    env.ok_in(&b, &["path", "npm:bar@1.0.0"]);
    assert_eq!(env.ok(&["clean", "--since", "1d"]), "removed 0 trees, 0 repos");

    // The repo stays for the other tree.
    assert_eq!(env.ok(&["clean", "crate:foo"]), "removed 1 trees, 0 repos");
    assert!(!Path::new(&foo).exists() && Path::new(&bar).exists());
    assert_eq!(git(&head, &["rev-parse", "HEAD"]), rev);
    assert_eq!(env.ok_in(&a, &["list"]), "git:https://git.test/foo");
    assert_eq!(env.ok(&["clean", "--since", "0s"]), "removed 2 trees, 1 repos");
    assert_eq!(env.ok_in(&b, &["list", "--all"]), "");
    assert_eq!(fs::read_dir(env.store.join("trees")).unwrap().count(), 0);

    // Fetching again after a clean recreates the tree.
    assert_eq!(env.ok(&["path", "crate:foo@1.0.0"]), foo);
}

#[test]
fn concurrent() {
    let env = Env::new("concurrent");
    let repo = env.repo("foo");
    let rev = repo.commit(&[("lib.rs", "")]);
    env.crate_("foo", "1.0.0", Some("https://git.test/foo"), Some((&rev, "")));
    env.npm("bar", "1.0.0", None, None);

    let outputs: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let env = &env;
                let spec = ["crate:foo@1.0.0", "npm:bar@1.0.0", "git:https://git.test/foo"][i % 3];
                s.spawn(move || (spec, env.ok(&["path", spec])))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).map(|(s, p)| format!("{s} {p}")).collect()
    });

    let mut distinct = outputs.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "{outputs:#?}");

    assert_eq!(env.ok(&["list"]).lines().count(), 3);
    let worktrees = git(env.store.join("repos/git.test--foo"), &["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 3, "{worktrees}");

    // Locks are removed on release.
    let locks = fs::read_dir(&env.store)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "lock"));
    assert_eq!(locks.count(), 0);
}

#[test]
#[ignore = "network"]
fn real_registries() {
    let store = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("real_registries");
    let _ = fs::remove_dir_all(&store);

    let out = Command::new(env!("CARGO_BIN_EXE_depsrc"))
        .args(["-S", store.to_str().unwrap(), "path", "crate:tokio-util@0.7.12", "npm:zod@3.23.8"])
        .output()
        .unwrap();

    let out = stdout(&out);
    let paths: Vec<&str> = out.lines().collect();
    assert!(paths[0].ends_with("/tokio-util"), "{out}");
    assert_eq!(git(paths[0], &["rev-parse", "HEAD"]), "35f244ad096b5331a38edd5a8f0beb436104873e");
    assert_eq!(git(paths[1], &["rev-parse", "HEAD"]), "ca42965df46b2f7e2747db29c40a26bcb32a51d5");
}

/// A test's directories, git config and registry.
struct Env {
    dir: PathBuf,
    store: PathBuf,
    gitconfig: PathBuf,
    registry: Registry,
}

impl Env {
    fn new(name: &str) -> Env {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("remotes")).unwrap();
        let gitconfig = dir.join("gitconfig");
        let remotes = dir.join("remotes").display().to_string();

        fs::write(
            &gitconfig,
            format!(
                "[url \"file://{remotes}/\"]\n\tinsteadOf = https://git.test/\n\tinsteadOf = https://github.com/\n\
                 [protocol \"file\"]\n\tallow = always\n[user]\n\tname = t\n\temail = t@t\n\
                 [init]\n\tdefaultBranch = main\n"
            ),
        )
        .unwrap();

        Env { store: dir.join("store"), dir, gitconfig, registry: Registry::start() }
    }

    fn repo(&self, name: &str) -> Repo {
        let repo = Repo { dir: self.dir.join("remotes").join(name), gitconfig: self.gitconfig.clone() };
        fs::create_dir_all(&repo.dir).unwrap();
        repo.git(&["init", "-q"]);
        repo.git(&["config", "uploadpack.allowFilter", "true"]);
        repo.git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);

        repo
    }

    /// Publishes a crate version, with `.cargo_vcs_info.json` when `vcs` is given.
    fn crate_(&self, name: &str, version: &str, repo: Option<&str>, vcs: Option<(&str, &str)>) {
        let root = format!("{name}-{version}");
        let mut files = vec![(format!("{root}/Cargo.toml"), format!("[package]\nname = \"{name}\""))];

        if let Some((sha, path)) = vcs {
            let info = json!({"git": {"sha1": sha}, "path_in_vcs": path});
            files.push((format!("{root}/.cargo_vcs_info.json"), info.to_string()));
        }

        let dl = format!("/dl/{root}.crate");
        self.registry.serve(&format!("/crates{dl}"), self.tarball(&files));

        let doc = json!({"version": {"crate": name, "num": version, "repository": repo, "dl_path": dl}});
        self.registry.serve(&format!("/crates/api/v1/crates/{name}/{version}"), doc.to_string().into_bytes());

        let doc = json!({"crate": {"default_version": version, "repository": repo}});
        self.registry
            .serve(&format!("/crates/api/v1/crates/{name}?include=default_version"), doc.to_string().into_bytes());
    }

    /// Publishes an npm version, also tagged `latest`.
    fn npm(&self, name: &str, version: &str, repository: Option<&Value>, git_head: Option<&str>) {
        let tarball = format!("/tarballs/{}-{version}.tgz", name.replace('/', "-"));
        let files = vec![("package/package.json".to_string(), json!({"name": name, "version": version}).to_string())];
        self.registry.serve(&tarball, self.tarball(&files));

        let doc = json!({
            "name": name,
            "version": version,
            "repository": repository,
            "gitHead": git_head,
            "dist": {"tarball": format!("{}{tarball}", self.registry.url)},
        })
        .to_string()
        .into_bytes();

        self.registry.serve(&format!("/npm/{name}/{version}"), doc.clone());
        self.registry.serve(&format!("/npm/{name}/latest"), doc);
    }

    fn tarball(&self, files: &[(String, String)]) -> Vec<u8> {
        let tmp = self.dir.join(format!("tarball-{}", files[0].0.replace('/', "-")));

        for (path, content) in files {
            fs::create_dir_all(tmp.join(path).parent().unwrap()).unwrap();
            fs::write(tmp.join(path), content).unwrap();
        }

        let top = files[0].0.split('/').next().unwrap();
        let out = Command::new("tar").arg("-czf").arg("-").arg("-C").arg(&tmp).arg(top).output().unwrap();
        assert!(out.status.success());
        fs::remove_dir_all(&tmp).unwrap();

        out.stdout
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.dir, args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(cwd, args).output().unwrap()
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_depsrc"));

        cmd.arg("-S")
            .arg(&self.store)
            .arg("-C")
            .arg(cwd)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("DEPSRC_CRATES_URL", format!("{}/crates", self.registry.url))
            .env("DEPSRC_NPM_URL", format!("{}/npm", self.registry.url))
            .env("XDG_CACHE_HOME", self.dir.join("xdg"))
            .env_remove("DEPSRC_STORE")
            .env_remove("COMPLETE");

        cmd
    }

    /// Shell completions of `depsrc <args>`, from the store in the environment.
    fn complete(&self, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_depsrc"))
            .args(["--", "depsrc"])
            .args(args)
            .env("COMPLETE", "fish")
            .env("DEPSRC_STORE", &self.store)
            .output()
            .unwrap();

        stdout(&out)
    }

    /// Stdout of a successful run, which prints nothing else than notes: no git nor tar output.
    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.dir, args)
    }

    fn ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run_in(cwd, args);
        let path = stdout(&out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.lines().all(|l| l.starts_with("note: ")), "unexpected stderr: {stderr}");

        path
    }
}

fn stdout(out: &Output) -> String {
    assert!(out.status.success(), "depsrc failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout.clone()).unwrap().trim().to_string()
}

fn git(dir: impl AsRef<Path>, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir.as_ref()).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

struct Repo {
    dir: PathBuf,
    gitconfig: PathBuf,
}

impl Repo {
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", &self.gitconfig)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();

        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// Commits `files` and returns the commit id.
    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (path, content) in files {
            let path = self.dir.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "commit"]);

        self.head()
    }

    fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }
}

/// HTTP server answering GETs from a path → body map, or 503 when down.
struct Registry {
    url: String,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    down: Arc<AtomicBool>,
}

impl Registry {
    fn start() -> Registry {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let down: Arc<AtomicBool> = Arc::default();
        let (r, d) = (routes.clone(), down.clone());

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split(' ').nth(1).unwrap_or("/").to_string();

                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }

                let body = r.lock().unwrap().get(&path).cloned();

                let (status, body) = match body {
                    _ if d.load(Ordering::Relaxed) => ("503 Service Unavailable", Vec::new()),
                    Some(body) => ("200 OK", body),
                    None => ("404 Not Found", Vec::new()),
                };

                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body));
            }
        });

        Registry { url, routes, down }
    }

    fn serve(&self, path: &str, body: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.to_string(), body);
    }
}
