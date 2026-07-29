# Plugins

Toren integrates with your tracker, your coding agents, and your forge through [Rhai](https://rhai.rs/)
**resolver plugins** under `~/.toren/plugins/`. A resolver answers the small, source-specific
questions that breq's core can't: *how do I read this task? how do I spawn this agent? what happened
to this branch after it left the workspace?* Adding support for a new tracker, agent, or forge is a
single `.rhai` file — no rebuild, no release.

> Resolvers are one of toren's four extension layers. The others are `toren.kdl` hooks (per repo),
> workflow shell scripts (per workflow, see below), and agent skills (per session). See
> [CONCEPTS.md](CONCEPTS.md#the-extension-census-four-layers).

## The three families

Plugins live in three directories under `~/.toren/plugins/`, one per family. The filename (without
`.rhai`) is the name breq keys on — `tasks/runes.rhai` handles the `runes` source, `agents/claude.rhai`
is the `claude` agent.

| Family | Directory | Answers | Shipped |
|--------|-----------|---------|---------|
| **Tasks** | `tasks/` | read & update a tracker | `beads`, `github`, `linear`, `runes` |
| **Agents** | `agents/` | spawn & observe a coding agent | `claude`, `codex`, `gemini`, `opencode`, `pi` |
| **Delivery** | `delivery/` | PR/CI state for a forge | `github` |

The shipped agent resolvers are *vendored* — present out of the box and user-overridable. Drop a
file with the same name in your own `agents/` directory and it wins.

**Lazy loading**: plugin metadata (the doc comment) is parsed without compiling the Rhai AST.
Compilation happens on demand, only when a resolver function is actually called.

## Task resolvers (`tasks/`)

A task resolver reads and mutates one tracker. breq calls its functions when you `breq do <task>`,
`breq get <ws> task.<field>`, or `breq set <ws> task.<field> <value>`.

```rhai
/// My tracker. Resolves tasks via the `mytool` CLI.

fn info(id) {
    let data = json::parse(shell("mytool", ["show", id, "--json"]));
    #{ id: data.id, title: data.title, status: data.status,
       assignee: data.assignee, description: data.description, kind: data.kind }
}

fn claim(id, assignee) {
    shell("mytool", ["update", id, "--status", "in-progress", "--assignee", assignee]);
}

fn set_field(id, field, value) {
    shell("mytool", ["update", id, "--" + field, value]);
}

fn create(title, desc) {
    let args = ["create", "--title", title];
    if desc != () { args += ["--description", desc]; }
    shell("mytool", args)   // return the created id
}
```

`info` is the only required function; return `{ id, title, status?, assignee?, description?, kind?,
url? }`. Task-source-owned fields (status, assignee, title) are always read live — breq never caches
them, so a workspace can't claim a status the tracker disagrees with.

## Agent resolvers (`agents/`)

An agent resolver answers two questions core can't: how to spawn the agent, and how to read what it's
doing. Everything else — sessions, windows, attaching — is agent-agnostic.

```rhai
/// ctx for argv/resume_argv: #{ prompt, model, auto_approve, session_id, workspace }

fn argv(ctx) {
    let args = ["myagent"];
    if ctx.model != () && ctx.model != "" { args += ["--model", ctx.model]; }
    if ctx.prompt != () && ctx.prompt != "" { args += [ctx.prompt]; }
    args
}

fn resume_argv(ctx) {
    let args = ["myagent", "--resume"];
    if ctx.session_id != () && ctx.session_id != "" { args += [ctx.session_id]; }
    args
}

/// "running" mid-turn, "idle" otherwise, "" with no session at all.
fn activity(ws_path) { ... }

/// A session summary, used as a workspace title of last resort.
fn title(ws_path) { ... }

/// The agent's own session id, for --resume.
fn session_id(ws_path) { ... }
```

`argv` / `resume_argv` are what breq execs; `activity` / `title` / `session_id` are what `breq list`
and `breq get` read to show what the agent is doing. See `contrib/plugins/agents/claude.rhai` for a
full worked example (it reads Claude's per-directory JSONL logs).

## Delivery resolvers (`delivery/`)

A delivery resolver reports on a branch after it has left the workspace — the PR and its CI. It is
read-only and slow (network), so breq writes its result into `<ws>/.toren/cache.json` with a
timestamp and **never calls it on the `breq list` hot path** — only from a command already
rendering that one workspace, or on `--refresh`.

```rhai
/// ctx: #{ path, branches }  — branches are VCS-derived (e.g. "feature@origin" or "origin/feature")
/// returns: array of #{ branch, id, url, state, ci }

fn prs(ctx) {
    let found = [];
    for branch in ctx.branches {
        let out = shell("gh", ["pr", "list", "--head", local_name(branch),
            "--state", "all", "--json", "number,url,state,headRefName,statusCheckRollup"],
            #{ dir: ctx.path });
        // ...parse and push #{ branch, id, url, state, ci } ...
    }
    found
}
```

Which delivery resolver to use is chosen by `[delivery] source` in config, or a per-workspace
`delivery` resolver; with exactly one installed, breq uses it. See
[configuration.md](configuration.md).

## Managing plugins

```sh
breq plugin list                    # installed + available across all three families
breq plugin install tasks/linear    # fetch a resolver from the contrib repo
breq plugin install agents/codex    # ...or an agent, or delivery/<forge>
breq plugin install ./my/tasks/custom.rhai   # ...or from a local path
```

`breq plugin install` resolves a local path first, otherwise fetches from `contrib/plugins/` in the
[toren repo](https://github.com/anowell/toren). For a local file, the **parent directory name**
(`tasks`, `agents`, or `delivery`) determines the family it installs into.

## Workflow verbs are shell scripts, not plugins

Multi-step workflows — "ship this", "hand it back", "open a PR" — are **not** Rhai plugins. They are
ordinary shell scripts dispatched git-style: `breq <name>` with an unknown verb runs a `breq-<name>`
script found on your `PATH` or in `~/.toren/bin`.

`breq init` installs the shipped defaults, which are *task verbs* — they update your tracker over the
place/task surface (`breq get`, `breq set task.*`) and never tear the workspace down:

| Script | What it does |
|--------|--------------|
| `breq-complete` | Marks each linked task done (per-tracker status strings). No teardown, no push. |
| `breq-abort` | Reopens each linked task and drops its assignee. |
| `breq-submit` | Pushes, opens a PR, marks tasks in-review. Installed when `breq init` detects github + `gh`. |

These are yours to edit — the per-tracker status values in them are defaults, not a vocabulary breq
imposes. Because completing a task and tearing down its place are separate axes, none of these delete
the workspace; when you're done with the place, `breq teardown <ws>`.

> The old model had `commands/` Rhai plugins (`assign`, `complete`, `abort`) and a `DeferredAction`
> protocol for scripts that needed to start an agent. Those are gone. `assign` is now `breq do <task>`
> directly; `complete`/`abort` are the shell scripts above.
