# depsrc

`depsrc` is a simple CLI for coding agents to fetch sources of any package. It clones the git
history when available, and uses the version in your project lockfile by default.

It is really helpful to give agents proper context about packages they use from the real source,
instead of guessing or reading pre-processed files from the registries. Since clones have full
history, it helps when updating dependencies or tracing back a bug too.

The CLI is simple, fast and convenient for agents:
```sh
$ rg "Serialize" $(depsrc path crate:serde)
$ git -C $(depsrc path npm:vite@8.3.5) log
```

## Installation

`depsrc` runs on Linux and macOS, and needs `git` (2.49+), `tar` and `gzip` in `PATH`.
There are multiple installation options:

- **Prebuilt binaries** are available from each
  [GitHub release](https://github.com/baptiste0928/depsrc/releases).
- From source with `cargo`: `cargo install --git https://github.com/baptiste0928/depsrc.git --locked`.
- From source with Nix: `nix profile install github:baptiste0928/depsrc`.

After it is installed, tell your agent to use it. Add this to your `AGENTS.md` or `CLAUDE.md`:

```markdown
Use `depsrc` to read the source of a package, see `depsrc --help`.
```

## How it works

`depsrc` supports fetching dependencies from [crates.io](https://crates.io/) and
[npm](https://www.npmjs.com/), as well as arbitrary repos from GitHub or any git host. If no version
is specified, it defaults to the one from your lockfile (cargo, npm, pnpm, yarn and bun are
supported), but you can also specify any arbitrary version or git ref (tag, branch, commit).

Sources are stored in `~/.cache/depsrc` and git repos are reused when multiple versions of the same
package are fetched. By default we clone a blobless repo (with only the history, file contents are
fetched on demand by git) to speed up fetching and use less disk.

The file layout inside `~/.cache/depsrc` looks like this:

```
~/.cache/depsrc
├── sources.json                                 index of resolved versions and their usage
├── repos/                                       bare blobless clones, one per repo
│   ├── github.com--colinhacks--zod
│   └── github.com--GitoxideLabs--gitoxide
└── trees/                                       worktrees of these clones, one per version
    ├── github.com--colinhacks--zod@1899684f
    ├── github.com--colinhacks--zod@f3c9ec03
    ├── github.com--GitoxideLabs--gitoxide@37860b34
```

To clean up sources unused for more than 30 days, use `depsrc clean`.
