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

### Doc comments

Add `///` doc comments at the top of your plugin for help metadata:

```rhai
/// Short description shown in `breq --help`.
///
/// Usage: breq myplugin <arg>
///
/// Detailed help text shown by `breq myplugin --help`.
/// Can span multiple lines.

let arg = ARGS[0];
```

- The first paragraph (before a blank `///` line) is the **description** — shown in `breq --help`
- The full text is the **usage** — shown by `breq <plugin> --help`

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
    // prompt: "user message text",
    // intent: "act",  // rendered as system prompt via --append-system-prompt
}
```

The host interprets this after the script completes and calls `breq cmd` with the specified fields. The `intent` and `prompt` are independent and composable — intent becomes a system prompt that frames the session, while prompt is the user message.

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

### `task(id) -> Map`

Infer task fields from an ID. Splits `source:id` prefixes, fetches title and description from the task source when possible.

Returns `{ id, title?, description?, url?, source? }`.

```rhai
let task = task("breq-abc");
print(`Task: ${task.id} - ${task.title}`);
```

### `ancillary(workspace) -> Map`

Resolve a workspace name to its active assignment and return all fields.

Returns a map with: `id`, `ancillary_id`, `segment`, `workspace_path`, `status`, `task_id`, `task_title`, `task_url`, `task_source`, `session_id`, `ancillary_num`, `base_branch`.

```rhai
let info = ancillary("one");
print(`Task: ${info.task_id} in ${info.workspace_path}`);
```

### `ws_changes(workspace) -> Array`

Get the list of commits/changes in a workspace (relative to its base branch).

Returns an array of `{ id, summary }` maps.

```rhai
let changes = ws_changes("one");
for c in changes {
    print(`${c.id}: ${c.summary}`);
}
```

### `config(key) -> String`

Read a config value by dot-separated key path. Returns string representation (strings as-is, numbers/bools stringified, objects as JSON).

```rhai
let source = config("tasks.default_source");
let port = config("server.port");
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

### `parse_args(args, spec) -> Map`

Parse CLI-style arguments according to a spec. Returns `{ args, opts }` where `args` is an array of positional arguments and `opts` is a map of parsed option values keyed by long name.

Spec is a map where each key is a long option name and each value is a config map:

- `type` (required): `"bool"`, `"string"`, or `"int"`
- `short` (optional): single-char short alias (e.g. `"s"` for `-s`)
- `default_val` (optional): default value if not provided (bool defaults to `false`, string/int default to `()`)

```rhai
let parsed = parse_args(ARGS, #{
    push: #{ type: "bool" },
    segment: #{ type: "string", short: "s" },
    count: #{ type: "int", default_val: 5 },
});
// parsed.args -> positional arguments
// parsed.opts.push -> true/false
// parsed.opts.segment -> string or ()
// parsed.opts.count -> int (5 if not provided)
```

`--` stops option parsing; everything after it becomes positional. Unknown flags error.

### `print(msg)`

Print to stdout.

## Community plugins

Example plugins for common workflows live in `contrib/plugins/`. To install them, copy the `.rhai` files into your plugin directory:

```sh
cp contrib/plugins/*.rhai ~/.toren/plugins/
```

### `assign`

Claims a task and starts a Claude session. Source-agnostic — dispatches status updates based on the task source (beads, github, linear). The task description is passed as the user message; the intent (if specified) is rendered as a system prompt.

```
breq assign <task-id> [--intent <name>]
```

Options: `--intent` / `-i` — intent template to use as system prompt (e.g., "act", "plan")

1. Infers task fields (source, title, url, description) from the ID
2. Updates task status to in-progress via source-specific CLI
3. Returns a deferred `cmd` action to start Claude (description as user message, intent as system prompt)

### `complete`

Cleans workspace, pushes changes, and closes the task.

```
breq complete <workspace>
```

1. Resolves workspace to its active assignment
2. Cleans workspace (auto-commit, push, kill processes)
3. Closes the task via source-specific CLI

### `abort`

Cleans workspace and reopens the task.

```
breq abort <workspace>
```

1. Resolves workspace to its active assignment
2. Cleans workspace (kill processes, no push)
3. Reopens the task via source-specific CLI

## Daemon API

The daemon exposes a plugin action endpoint:

```
POST /api/assignments/:id/action/:plugin_name
Body: { "args": ["arg1", "arg2"] }
```

Returns `{ success: true }` or `{ success: true, action: { type: "cmd", ... } }` for deferred actions. The daemon can't exec into Claude, so callers handle deferred actions themselves.
