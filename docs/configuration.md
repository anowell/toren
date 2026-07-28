# Configuration

Toren uses a single config file at `~/.toren/config.kdl` — the same language as the per-repo
`toren.kdl`. Toren keeps no global workspace registry: each workspace carries its own state in a
git-excluded `<ws>/.toren/` directory, and the VCS enumerates the workspaces. Shared,
non-workspace state (plugins, scripts, logs) lives under `~/.toren/`.

You can override the config path with `--config <path>`.

An older `~/.toren/config.toml` is converted to `config.kdl` the first time toren loads it. The
old file is left where it is, and any setting toren no longer has is reported rather than dropped
in silence. `config.kdl.example` in this repo is the annotated reference.

For most users, `breq init` in a repo is sufficient — it creates `toren.kdl` for workspace hooks and offers to register the repo as a segment. No manual config editing needed.

## Full Reference

```kdl
ancillaries {
    // Segment globs: directories matching these patterns are available as segments.
    // Each segment is a repo that breq can create ancillary workspaces for.
    segments "~/projects/*" "~/work/special-repo"

    // Where ancillary workspaces are created on disk.
    // Default: ~/.toren/workspaces
    workspace_root "~/.toren/workspaces"

    // Max ancillaries per segment (default: 10)
    max_per_segment 10

    // Coding agent for `breq do`; auto-detected from PATH if unset.
    // agent "claude"
}

proxy {
    // Base domain for per-workspace reverse proxy routes via Station.
    // Workspace routes become: <ws_name>.<repo_name>.<domain>
    // Default: lvh.me
    domain "lvh.me"
}

server {
    // Daemon host and port (only used by toren-daemon)
    host "127.0.0.1"
    port 8787
}

tasks {
    // Ordered task sources tried when resolving an ID with no source prefix.
    // If omitted, auto-detects from installed task resolver plugins.
    // sources "runes" "beads"
}

delivery {
    // Which delivery resolver reads PR/CI state for `breq list`/`get`.
    // Optional: with exactly one delivery plugin installed, breq uses it.
    // A workspace can also override per-place with its own `delivery` resolver.
    // source "github"
}

aliases {
    // Shell command templates invoked as breq subcommands (lowest priority — after
    // breq-<name> scripts and clap subcommands). Positional args: $1, $2, etc.
    show "breq get $1"
}
```

## Sections

### `ancillaries`

**`segments`** — Glob patterns that discover project directories. Each matched directory becomes a segment (a repo breq can manage workspaces for). When you run `breq do` from within a repo, breq matches your CWD against these patterns to determine the segment.

If no segments are configured, breq infers the segment from the current repo's directory name. You only need explicit segments when managing multiple repos or using `breq list --all`.

`breq init` offers to add your repo's parent directory (e.g. `~/projects/*`) or the repo itself to this list.

**`workspace_root`** — The directory where ancillary workspaces are created. Layout: `<workspace_root>/<segment_name>/<workspace_name>/`. Defaults to `~/.toren/workspaces`.

**`max_per_segment`** — Maximum number of concurrent ancillary workspaces per segment. Defaults to 10. Workspace names are numbered words: "one", "two", ..., up to this limit.

**`agent`** — Coding agent for `breq do` (an `agents/` plugin name, optionally `name:model`). Auto-detected from PATH when unset; a workspace's own recorded agent wins over this.

### `proxy`

Controls how [Station](../station/README.md) reverse proxy routes are set up for workspaces. Only relevant if your `toren.kdl` uses the `proxy` directive.

**`domain`** — Base domain for routes. Defaults to `lvh.me` (resolves to 127.0.0.1 via wildcard DNS). Workspace routes are computed as `<workspace_name>.<repo_name>.<domain>`.

### `server`

Only used by the toren daemon. Ignored by breq.

### `tasks`

**`sources`** — Ordered list of task sources tried when resolving an ID without a `source:id`
prefix. If not set, toren auto-detects from installed task resolver plugins. To target a specific
source, use the prefix syntax: `breq do linear:ENG-123`. The legacy TOML single-string form
(`default_source = "beads"`) is still read during migration.

### `delivery`

**`source`** — Which delivery resolver (`~/.toren/plugins/delivery/<name>.rhai`) reads PR and CI
state for `breq list` and `breq get`. Optional: with exactly one delivery plugin installed, breq
uses it without configuration. A workspace can also override per-place with its own `delivery`
resolver.
Delivery is fetched by the commands that render one workspace (`breq get`, the daemon's workspace
view) and by `breq list --refresh`, each of which writes what it gets into `<ws>/.toren/cache.json`.
Plain `breq list` renders that cache — with each entry's age — and never reaches the network.

### `aliases`

Shell command templates that become breq subcommands. Aliases are the last-resort dispatch: a bare
`breq <name>` tries a `breq-<name>` script and a clap subcommand before falling back to an alias.

Aliases receive positional arguments (`$1`, `$2`).

## Logs

`breq` and `toren-daemon` each write a rolling JSON log under `~/.toren/logs/` — `breq.jsonl` and
`toren-daemon.jsonl`, rotated daily, seven days kept. Workspace create/adopt/teardown, agent
start/resume/exit, plugin failures and VCS errors land there as structured events. It is a debug
record, not an interface: nothing reads it back, and it replaces the old
`~/.toren/completion_history.jsonl`, which `breq doctor` will point out if you still have one.

## Workspace Hooks (toren.kdl)

Per-repo workspace configuration lives in `toren.kdl` at the repo root (not in `~/.toren/config.kdl`). See the [README](../README.md#workspace-hooks-torenkdl) for details.
