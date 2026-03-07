# Configuration

Toren uses a single config file at `~/.toren/config.toml`. All toren state (assignments, workspaces, history) also lives under `~/.toren/`.

You can override the config path with `--config <path>`.

For most users, `breq init` in a repo is sufficient — it creates `toren.kdl` for workspace hooks and offers to register the repo as a segment. No manual config editing needed.

## Full Reference

```toml
[ancillaries]
# Segment globs: directories matching these patterns are available as segments.
# Each segment is a repo that breq can create ancillary workspaces for.
segments = ["~/projects/*", "~/work/special-repo"]

# Where ancillary workspaces are created on disk.
# Default: ~/.toren/workspaces
workspace_root = "~/.toren/workspaces"

# Max ancillaries per segment (default: 10)
max_per_segment = 10

[proxy]
# Base domain for per-workspace reverse proxy routes via Station.
# Workspace routes become: <ws_name>.<repo_name>.<domain>
# Default: lvh.me
domain = "lvh.me"

[server]
# Daemon host and port (only used by toren-daemon)
host = "127.0.0.1"
port = 8787

[tasks]
# Default task source when an ID is provided without a source prefix (e.g., "beads:breq-abc")
default_source = "beads"

[intents]
# Named prompt templates for breq do -i <name>.
# Available template variables: {{ task.id }}, {{ task.title }}, {{ task.url }}, {{ task.source }}
act = """Implement {{ task.id }}: {{ task.title }}

Complete the task as specified. When done, summarize changes."""

plan = """Design an approach for {{ task.id }}: {{ task.title }}

Investigate the codebase, explore options, and propose a design."""

review = """Review the implementation of {{ task.id }}: {{ task.title }}

Verify completeness, check for issues, and assess confidence."""

[aliases]
# Shell command templates invoked as breq subcommands (lower priority than plugins).
# Positional args: $1, $2, etc. Clean output vars: $ID, $WORKSPACE, $SEGMENT, $REVISION.
# Note: built-in plugins now handle assign/complete/abort. Custom aliases still work.
show = "breq list $1 --detail"
```

## Sections

### `[ancillaries]`

**`segments`** — Glob patterns that discover project directories. Each matched directory becomes a segment (a repo breq can manage workspaces for). When you run `breq do` from within a repo, breq matches your CWD against these patterns to determine the segment.

If no segments are configured, breq infers the segment from the current repo's directory name. You only need explicit segments when managing multiple repos or using `breq list --all`.

`breq init` offers to add your repo's parent directory (e.g. `~/projects/*`) or the repo itself to this list.

**`workspace_root`** — The directory where ancillary workspaces are created. Layout: `<workspace_root>/<segment_name>/<workspace_name>/`. Defaults to `~/.toren/workspaces`.

**`max_per_segment`** — Maximum number of concurrent ancillary workspaces per segment. Defaults to 10. Workspace names are numbered words: "one", "two", ..., up to this limit.

### `[proxy]`

Controls how [Station](../station/README.md) reverse proxy routes are set up for workspaces. Only relevant if your `toren.kdl` uses the `proxy` directive.

**`domain`** — Base domain for routes. Defaults to `lvh.me` (resolves to 127.0.0.1 via wildcard DNS). Workspace routes are computed as `<workspace_name>.<repo_name>.<domain>`.

### `[server]`

Only used by the toren daemon. Ignored by breq.

### `[tasks]`

**`default_source`** — The default task source used when an ID is provided without a `source:id` prefix. Defaults to `"beads"`. When you run `breq do --task-id my-task`, the source is set to this value. To override, use the prefix syntax: `breq do --task-id linear:ENG-123`.

### `[intents]`

Named prompt templates used with `breq do -i <intent>`. The default intents (`act`, `plan`, `review`) cover common workflows. You can add custom intents or override defaults.

Template variables: `{{ task.id }}`, `{{ task.title }}`, `{{ task.url }}`, `{{ task.source }}`

### `[aliases]`

Shell command templates that become breq subcommands. Aliases have lower priority than plugins — if a plugin and alias share the same name, the plugin wins.

Aliases receive positional arguments (`$1`, `$2`) and environment variables from clean output (`$ID`, `$WORKSPACE`, `$SEGMENT`, `$REVISION`).

## Workspace Hooks (toren.kdl)

Per-repo workspace configuration lives in `toren.kdl` at the repo root (not in `~/.toren/config.toml`). See the [README](../README.md#workspace-hooks-torenkdl) for details.
