# Plugins

Toren's plugin system lets you extend `breq` with [Rhai](https://rhai.rs/) scripts that call native Rust operations via a host API. Plugins replace the old shell-based aliases for complex workflows that need conditional logic, JSON parsing, or multi-step orchestration.

## How it works

1. **User plugins** in `~/.toren/plugins/` are discovered at startup
2. **Community plugins** in `contrib/plugins/` can be copied into your plugin directory
3. **Dispatch order**: plugins > aliases > clap subcommands

Plugins are organized into two directories by kind:

- `commands/` — breq subcommands (e.g., `assign.rhai`, `complete.rhai`)
- `tasks/` — task source resolvers, keyed by source name (e.g., `beads.rhai`, `runes.rhai`)

When you run `breq <name> [args...]`, breq checks for a matching command plugin first. If found, it executes the Rhai script with `ARGS` set to the positional arguments.

**Lazy loading**: Plugin metadata (descriptions, usage) is parsed from doc comments without compiling the Rhai AST. Compilation happens on demand — only when a plugin is actually executed.

## Writing command plugins

Create a `.rhai` file in `~/.toren/plugins/commands/`. The filename (without extension) becomes the command name.

```rhai
// ~/.toren/plugins/commands/hello.rhai
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

Plugins that need to start a coding agent session can't directly exec into another process. Instead, return a map with `action: "do"`:

```rhai
#{
    action: "do",
    task_id: "breq-123",
    task_title: "Fix the bug",
    // Optional fields:
    // task_url: "https://...",
    // prompt: "user message text",
    // intent: "act",  // rendered as system prompt via --append-system-prompt
}
```

The host interprets this after the script completes and calls `breq do` with the specified fields. The `intent` and `prompt` are independent and composable — intent becomes a system prompt that frames the session, while prompt is the user message.

## Writing task resolver plugins

Create a `.rhai` file in `~/.toren/plugins/tasks/`. The filename becomes the source name (e.g., `beads.rhai` handles source `"beads"`).

A resolver plugin implements these functions:

```rhai
/// Required: return task info as a map.
/// Fields: id, title, status, assignee, description, kind (all optional except id, title)
fn info(id) {
    let result = shell("mytool", ["show", id, "--json"]);
    let data = json::parse(result);
    #{ id: data.id, title: data.title, status: data.status, assignee: data.assignee, description: data.description }
}

/// Claim a task — update status and assignee.
fn claim(id, assignee) {
    shell("mytool", ["update", id, "--status", "in_progress", "--assignee", assignee]);
}

/// Mark a task as complete.
fn complete(id) {
    shell("mytool", ["update", id, "--status", "done"]);
}

/// Abort/reopen a task.
fn abort(id) {
    shell("mytool", ["update", id, "--status", "todo", "--assignee", ""]);
}

/// Create a new task. Return the created task ID.
fn create(title, desc) {
    let args = ["create", "--title", title];
    if desc != () {
        args += ["--description", desc];
    }
    shell("mytool", args)
}
```

Resolvers are called by the `task::` host API and by the `PluginManager` internals for multi-source resolution.

## Host API reference

### `task::` — task operations

#### `task::info(id) -> Map`

Resolve a task by ID. Tries configured task sources in order until one succeeds. Splits `source:id` prefixes (e.g., `"beads:abc-123"`) to target a specific source.

Returns `{ id, source, title, description?, status?, assignee?, kind? }`.

```rhai
let t = task::info("abc-123");
print(`Task: ${t.id} [${t.source}] - ${t.title}`);
```

#### `task::claim(source, id, assignee)`

Claim a task via its resolver — typically updates status and assignee.

```rhai
task::claim("beads", "abc-123", "claude");
```

#### `task::complete(source, id)`

Mark a task as complete via its resolver.

```rhai
task::complete("beads", "abc-123");
```

#### `task::abort(source, id)`

Abort/reopen a task via its resolver.

```rhai
task::abort("beads", "abc-123");
```

#### `task::create(source, title [, desc]) -> String`

Create a new task via a resolver. Returns the created task ID.

```rhai
let id = task::create("beads", "Fix the login bug", "Users can't log in after password reset");
```

### `toren::` — toren context

#### `toren::config(key) -> String`

Read a config value by dot-separated key path.

```rhai
let source = toren::config("tasks.default_source");
let port = toren::config("server.port");
```

#### `toren::assignment(workspace) -> Map`

Resolve a workspace name to its active assignment.

Returns: `id`, `ancillary_id`, `segment`, `workspace_path`, `status`, `task_id`, `task_title`, `task_url`, `task_source`, `session_id`, `ancillary_num`, `base_branch`.

```rhai
let info = toren::assignment("one");
print(`Task: ${info.task_id} in ${info.workspace_path}`);
```

### `json::` — JSON operations

#### `json::parse(text) -> Dynamic`

Parse a JSON string into a Rhai value.

```rhai
let data = json::parse(shell("bd", ["show", "abc", "--json"]));
```

#### `json::stringify(value) -> String`

Serialize a Rhai value to JSON.

### `fs::` — filesystem operations

- `fs::read(path) -> String` — read file contents
- `fs::write(path, content)` — write file contents
- `fs::exists(path) -> bool` — check if path exists
- `fs::glob(pattern) -> Array` — glob for files
- `fs::ls(path) -> Array` — list directory entries

### `path::` — path operations

- `path::join(a, b) -> String` — join path segments
- `path::parent(path) -> String` — parent directory
- `path::filename(path) -> String` — filename component
- `path::ext(path) -> String` — file extension

### `toml::` — TOML operations

- `toml::parse(text) -> Dynamic` — parse TOML string

### `http::` — HTTP client

- `http::get(url [, opts]) -> Map` — GET request
- `http::post(url, opts) -> Map` — POST request
- `http::put(url, opts) -> Map` — PUT request
- `http::patch(url, opts) -> Map` — PATCH request
- `http::delete(url [, opts]) -> Map` — DELETE request

Options: `headers` (map), `body` (string), `json` (value, auto-serialized). Returns `{ status, body, ok }`.

### Core functions

#### `shell(program, args) -> String`

Run a command and return its stdout (trimmed). Errors on non-zero exit.

```rhai
let output = shell("git", ["rev-parse", "HEAD"]);
```

#### `shell(program, args, opts) -> Map`

Extended shell with options: `dir` (working directory), `env` (environment map), `stdin` (input string), `timeout` (milliseconds). Returns `{ stdout, stderr, status }`.

#### `shell_status(program, args) -> i64`

Run a command and return its exit code (doesn't error on non-zero).

```rhai
let code = shell_status("git", ["diff", "--quiet"]);
if code != 0 {
    print("Uncommitted changes detected");
}
```

#### `env(name) -> String`

Get an environment variable, or empty string if not set.

#### `cwd() -> String`

Get the current working directory.

#### `platform() -> String`

Get the platform identifier (e.g., "macos", "linux").

#### `parse_args(args, spec) -> Map`

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

#### `print(msg)`

Print to stdout.

### `ws::` — workspace operations

#### `ws_changes(workspace) -> Array`

Get the list of commits/changes in a workspace (relative to its base branch).

Returns an array of `{ id, summary }` maps.

```rhai
let changes = ws_changes("one");
for c in changes {
    print(`${c.id}: ${c.summary}`);
}
```

## Community plugins

Example plugins for common workflows live in `contrib/plugins/`. To install them, copy into your plugin directory:

```sh
cp -r contrib/plugins/* ~/.toren/plugins/
```

### `assign`

Claims a task and starts a coding agent session. Source-agnostic — delegates to task resolver plugins for status updates.

```
breq assign <task-id> [--intent <name>]
```

Options: `--intent` / `-i` — intent template to use as system prompt (e.g., "act", "plan")

1. Resolves task fields via `task::info(id)`
2. Claims the task via `task::claim(source, id, assignee)`
3. Returns a deferred action to start a coding agent session

### `complete`

Cleans workspace, pushes changes, and closes the task.

```
breq complete <workspace>
```

1. Resolves workspace to its active assignment
2. Cleans workspace (auto-commit, push, kill processes)
3. Closes the task via `task::complete(source, id)`

### `abort`

Cleans workspace and reopens the task.

```
breq abort <workspace>
```

1. Resolves workspace to its active assignment
2. Cleans workspace (kill processes, no push)
3. Reopens the task via `task::abort(source, id)`

## Daemon API

The daemon exposes a plugin action endpoint:

```
POST /api/assignments/:id/action/:plugin_name
Body: { "args": ["arg1", "arg2"] }
```

Returns `{ success: true }` or `{ success: true, action: { type: "do", ... } }` for deferred actions. The daemon can't exec into Claude, so callers handle deferred actions themselves.
