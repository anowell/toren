# Toren Concepts

> *"I am Toren. I am continuity."*

Inspired by Ann Leckie's *Ancillary Justice*, where Justice of Toren is a starship AI whose
consciousness is distributed across many ancillary bodies. Toren orchestrates coding agents the
same way: one intelligence, many places it can be working at once.

## A workspace is a place

The central idea is small and load-bearing: **a workspace is a place, not an assignment.**

A place is the bundle of things that make a directory somewhere an agent can work:

- a **working copy** — a git worktree or jj workspace on disk
- **VCS state** — its branch/change, its commits, its base
- an **rmux session** — `toren-<segment>-<workspace>-<uid>`, where agents and shells run
- **annotations** — small facts you attach to it (a title, a linked task, the chosen agent)
- **env** — `TOREN_SEGMENT`, `TOREN_WORKSPACE`, `TOREN_WORKSPACE_PATH`, `TOREN_UID`

Processes run *in* a place. Tasks and delivery (the PR, the CI run) are *annotations on* a place.
That distinction is the whole model. An earlier version of toren treated a workspace as an
"assignment" — a workspace welded to one task, created when the task was claimed and destroyed
when it closed. That conflated two things that are actually independent, and this rewrite pulls
them apart.

| Concept | What it is |
|---------|-----------|
| **Toren / Ship** | The daemon — a persistent view over every place |
| **Segment** | A repository that places are created from (e.g. `toren`, `calculator`) |
| **Workspace / Place** | A working copy + VCS state + rmux session + annotations, e.g. `toren/one` |
| **Ancillary** | The classic name for a workspace — a body Toren works through |
| **Interface** | Your device (terminal, browser, phone) — just a viewport |

### Naming

Workspaces within a segment are named with numbered words — `one`, `two`, `three` — following the
books' convention ("One Esk Nineteen"). `breq setup` takes the next free slot; you can also name one
explicitly. Each *incarnation* of a slot (setup, teardown, setup again) also gets a short **uid**,
minted at setup, so the three lives of `toren/one` stay distinguishable in sessions and transcripts.

## Two orthogonal verb families

Every `breq` verb belongs to one of two families, and they move along different axes:

- **Place verbs** manage the workspace: `setup`, `teardown`, `do`, `sh`, `set`/`get`, `list`.
  They create it, run agents in it, read and annotate it, and tear it down.
- **Task verbs** update the tracker: writing `set <ws> task.status ...`, and the
  `breq-complete` / `breq-abort` / `breq-submit` scripts layered over it. They never touch the
  workspace.

The one deliberate crossing point: **`breq do <task>` claims the task it was handed.** That single
tracker write is the *only* tracker side effect in any place verb. Everything else keeps the axes
clean — `teardown` changes no task status and pushes nothing; `breq complete` changes task status
and deletes nothing.

This is what dissolves the old, confused question *"do I complete or destroy?"* They are not two
answers to one question — they are two questions:

- **"Am I finished with this piece of work?"** → a task verb (`breq complete <ws>`).
- **"Am I finished with this place?"** → a place verb (`breq teardown <ws>`).

You can ship a piece and keep the warm workspace for the next one. You can tear down a spike whose
task you never mean to close. `breq list` is where the two axes are shown side by side, so when they
*have* diverged — task closed but workspace still alive, changes never pushed, agent long since idle
— you can see it rather than compute it.

> **Ancillary Justice framing.** An ancillary *is* a workspace: it exists exactly while its place
> exists, and it is cleaned up (torn down) as a unit. What is no longer true is that its *task*
> shares that lifecycle. A body can finish one order and take another; finishing the work and
> retiring the body are separate acts.

## The place verbs

| Verb | What it does |
|------|--------------|
| `breq setup [ws]` | Create a workspace (working copy + hooks), no task, no agent. `--from <ws>` stacks a child on another workspace. Naming an existing working copy adopts it in place. |
| `breq teardown <ws>` | Delete a workspace. Task-agnostic: no status changes, no push. `--kill` also stops live panes; `--no-delete` keeps the working copy and drops only breq's state. |
| `breq do [task]` | Run a coding agent in a place. Needs a task or a `-p` prompt. Infers the workspace from your cwd, else makes a fresh one. Claiming a named task is its one tracker side effect. |
| `breq sh [ws]` | Open a shell in a place, or `breq sh <ws> -- <cmd>` to run a command there. The composability workhorse. |
| `breq get <ws> [key]` | Read a place: full detail, or one key for scripting. `task.*` keys pass through to the tracker. |
| `breq set <ws> <key> <value>` | Write an annotation, or a `task.*` field (pass-through). List keys take `+`/`-`: `breq set one +task runes:tor-1`. |
| `breq list` | One row per workspace, joining core state, annotations, derived VCS, and cached delivery. Never blocks on the network. |
| `breq doctor` | Detect known-bad state and, with `--fix`, repair it. Never runs implicitly. |
| `breq cleanup` | Remove leftovers — orphaned workspace directories, aged-out transcripts. |
| `breq init` | Initialize `toren.kdl` in a repo and install the shipped workflow scripts. |
| `breq plugin` | Manage the Rhai resolver plugins under `~/.toren/plugins`. |

`breq do` absorbs the old `assign` verb. There is no separate "claim a task and start" command;
`do <task>` *is* that, and `do -p "..."` is the prompt-only case. Task context (source, id, title,
description) is composed into the prompt ahead of anything you add.

## State model

There is **no global registry.** The old `~/.toren/assignments.json` is gone. Instead:

- **The VCS enumerates workspaces.** A place exists because its working copy exists and the VCS
  knows about it — `breq list` is a walk over that, not a lookup in a side file.
- **Annotations live with the workspace.** Each place carries a git-excluded
  `<ws>/.toren/annotations.json` (the facts you set) and `<ws>/.toren/cache.json` (derived values
  with timestamps). Nothing about a place lives anywhere but the place.
- **A uid is minted at setup** and embedded in the rmux session name
  (`toren-<segment>-<ws>-<uid>`) and in transcript paths.
- **Transcripts** are recorded under `~/.toren/transcripts/<segment>/<ws>/<uid>/<window>.raw`, so
  three incarnations of a slot keep three separate records.
- **Delivery is cached, never blocking.** PR/CI state is written to `cache.json` with a timestamp;
  `list` renders that cache and only reaches the network on `--refresh`. Task-source-owned fields
  (status, assignee, title) are the opposite — always asked of the source, never cached, so breq
  can never claim a status the tracker disagrees with.

### Stacking

`breq setup --from <ws>` creates a **child** workspace stacked on another (a jj child, or a
git branch off a branch). Instead of the normal `setup` hooks it runs `toren.kdl`'s `fork {}` block
(falling back to `setup` if there is none), where `{{ parent.path }}` names the workspace being
forked — so you can copy-on-write the parent's runtime state rather than rebuilding it.

### Adoption

Any working copy can become a place breq manages: `breq setup <name>` on an existing, undecorated
directory **adopts** it in place rather than recreating it — a hand-made worktree, or one that
outlived its annotations, joins the fold. `breq teardown --no-delete` is the inverse: it drops
breq's state but leaves the working copy on disk.

## The extension census: four layers

Toren is meant to be extended without a release. There are four distinct layers, from repo-specific
to session-specific:

1. **`toren.kdl` hooks** *(per repo)* — how a workspace is built and torn down: `copy`, `share`,
   `template`, `run`, `proxy`, `env`, plus the `setup` / `fork` / `destroy` blocks. See the
   [README](../README.md#workspace-hooks-torenkdl) and [env.md](env.md).

2. **Rhai resolvers** *(per integration, `~/.toren/plugins/`)* — a plugin *census* across three
   families, each a directory of small `.rhai` files keyed by name:
   - **`tasks/`** — a task tracker. `info` / `claim` / `set_field` / `complete` / `abort` /
     `create`. Shipped: `beads`, `github`, `linear`, `runes`.
   - **`agents/`** — a coding agent. `argv` / `resume_argv` / `activity` / `title` / `session_id`.
     Shipped and vendored: `claude`, `codex`, `gemini`, `opencode`, `pi` — user-overridable.
   - **`delivery/`** — a forge, for PR/CI state. `prs`. Shipped: `github`.

   Adding a new tracker, agent, or forge is one `.rhai` file — no rebuild, no release. See
   [plugins.md](plugins.md).

3. **Shell scripts** *(your workflow verbs, on `PATH` or in `~/.toren/bin`)* — anything that isn't a
   built-in verb is dispatched git-style: `breq complete` runs a `breq-complete` script. `complete`,
   `abort`, and `submit` ship as editable defaults (installed by `breq init`). They are ordinary
   shell over the place/task verbs, and they are *meant* to be edited — the per-tracker status
   strings in them are defaults, not a vocabulary breq imposes.

4. **Agent skills** *(per session, in the agent itself)* — in-session behavior and personas live in
   the agent's own skills, not in toren. (This is why toren no longer has "intents": prompt framing
   belongs to the agent, and task context already composes into the prompt via `do`.)

Each layer is independently editable and each replaces something that used to require a code change.

## Environment variables

Inside a place, `breq` sets:

```bash
TOREN_SEGMENT=toren
TOREN_WORKSPACE=one
TOREN_WORKSPACE_PATH=/Users/you/.toren/workspaces/toren/one
TOREN_UID=aaa111
```

Scripts lean on these: `breq complete` with no argument acts on `$TOREN_WORKSPACE`.

---

*Devices are interchangeable. Places persist. Toren endures.*
