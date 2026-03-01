# Plugins

Toren's plugin system lets you extend `breq` with [Rhai](https://rhai.rs/) scripts that call native Rust operations via a host API. Plugins replace the old shell-based aliases for complex workflows that need conditional logic, JSON parsing, or multi-step orchestration.

## How it works

1. **User plugins** in `~/.toren/plugins/*.rhai` are discovered at startup
2. **Community plugins** in `contrib/plugins/` can be copied into your plugin directory
3. **Dispatch order**: plugins > aliases > clap subcommands

When you run `breq <name> [args...]`, breq checks for a matching plugin first. If found, it executes the Rhai script with `ARGS` set to the positional arguments.

## Configuration

```toml
[plugins]
# Directory for user plugin scripts (default: ~/.toren/plugins)
dir = "~/.toren/plugins"

# Disable specific plugins by name
disable = ["abort"]
```

## Writing plugins

Create a `.rhai` file in `~/.toren/plugins/`. The filename (without extension) becomes the command name.

```rhai
// ~/.toren/plugins/hello.rhai
// Usage: breq hello <name>

let name = ARGS[0];
print(`Hello, ${name}!`);
```

### Variables

- **`ARGS`** — Array of positional arguments passed after the command name

### Deferred actions

Plugins that need to start a Claude session can't directly exec into another process. Instead, return a map with `action: "cmd"`:

```rhai
#{
    action: "cmd",
    task_id: "breq-123",
    task_title: "Fix the bug",
    // Optional fields:
    // task_url: "https://...",
    // prompt: "custom prompt text",
    // intent: "act",
}
```

The host interprets this after the script completes and calls `breq cmd` with the specified fields.

## Host API reference

### `shell(program, args) -> String`

Run a command and return its stdout (trimmed). Errors on non-zero exit.

```rhai
let output = shell("git", ["rev-parse", "HEAD"]);
let version = shell("bd", ["show", bead_id, "--field", "title"]);
```

### `shell_status(program, args) -> i64`

Run a command and return its exit code (doesn't error on non-zero).

```rhai
let code = shell_status("git", ["diff", "--quiet"]);
if code != 0 {
    print("Uncommitted changes detected");
}
```

### `breq_show(workspace, field) -> String`

Read a field from an active assignment (native Rust, no subprocess).

Fields: `task_id`/`id`, `task_title`/`title`, `task_url`/`url`, `task_source`/`source`, `workspace`/`workspace_path`, `segment`, `ancillary_id`, `status`.

```rhai
let id = breq_show("one", "task_id");
let title = breq_show("one", "title");
```

### `breq_clean(workspace) -> Map`

Clean a workspace with defaults (no kill, no push). Returns `{ workspace, segment, id?, revision? }`.

```rhai
let result = breq_clean("one");
print(`Cleaned ${result.workspace}`);
```

### `breq_clean_with(workspace, opts) -> Map`

Clean a workspace with options. `opts` is a map with optional `kill` and `push` booleans.

```rhai
let result = breq_clean_with("one", #{ kill: true, push: true });
if result.revision != () {
    print(`Pushed revision: ${result.revision}`);
}
```

### `task_infer(id) -> Map`

Infer task fields from an ID. Splits `source:id` prefixes, fetches title from the task source when possible.

Returns `{ id, title?, url?, source? }`.

```rhai
let task = task_infer("breq-abc");
print(`Task: ${task.id} - ${task.title}`);
```

### `json_parse(text) -> Dynamic`

Parse a JSON string into a Rhai value (maps, arrays, strings, numbers, booleans).

```rhai
let data = json_parse(shell("bd", ["show", "breq-abc", "--json"]));
print(`Title: ${data.title}`);
```

### `env(name) -> String`

Get an environment variable, or empty string if not set.

```rhai
let home = env("HOME");
let editor = env("EDITOR");
```

### `print(msg)`

Print to stderr (stdout is reserved for structured output).

## Community plugins

Example plugins for common workflows live in `contrib/plugins/`. To install them, copy the `.rhai` files into your plugin directory:

```sh
cp contrib/plugins/*.rhai ~/.toren/plugins/
```

### `assign` (beads)

Claims a bead and starts a Claude session.

```
breq assign <bead-id>
```

1. Sets bead status to `in_progress`, assignee to `claude`
2. Infers task fields (title, url) from the bead
3. Returns a deferred `cmd` action to start Claude

### `complete` (beads)

Cleans workspace, pushes changes, and closes the bead.

```
breq complete <workspace>
```

1. Cleans workspace (auto-commit, push, kill processes)
2. Closes the bead (`status -> closed`)

### `abort` (beads)

Cleans workspace and reopens the bead.

```
breq abort <workspace>
```

1. Cleans workspace (kill processes, no push)
2. Reopens bead (`status -> open`, unassigns)

## Daemon API

The daemon exposes a plugin action endpoint:

```
POST /api/assignments/:id/action/:plugin_name
Body: { "args": ["arg1", "arg2"] }
```

Returns `{ success: true }` or `{ success: true, action: { type: "cmd", ... } }` for deferred actions. The daemon can't exec into Claude, so callers handle deferred actions themselves.
