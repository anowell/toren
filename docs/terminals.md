# Terminals: rmux, zellij, and the web UI

Toren runs coding agents inside [rmux](https://rmux.io/) sessions. That one decision is what lets
`breq do` in a terminal and the toren web UI show you the *same* running agent, rather than two
separate implementations of "talk to Claude" that drift apart.

## The model

One **workspace** — a segment plus a place, like `toren/one` — maps to one rmux **session** named
`toren-<segment>-<workspace>-<uid>`, where the `uid` is minted at setup and distinguishes one
incarnation of a slot from the next:

```
toren-toren-one-aaa111
├── window "shell"     a login shell in the workspace  (closes when you exit it)
├── window "shell-2"   another shell, on demand        (a dev server, a log tail, …)
└── window "agent"     the coding agent                (replaced each `breq do`)
```

A workspace's session holds a *set* of windows: one or more shells and, when an agent is running,
an `agent` window. Shells are opened on demand (`breq sh` ensures one; the web UI can open more) and
each closes when you `exit` it, like any terminal. The `agent` window is the exception — it keeps
`remain-on-exit` set, so a finished or crashed agent lingers as a dead pane: that is what makes its
`exited` state observable in `breq list` and lets a reattaching browser still show what it did.
Continuing an agent is a separate act — `breq do --resume` starts a fresh process from the agent's
session id. Replacing a window's process always mints a new pane (so the daemon hands an attached
browser over to it) and never drops the session to zero windows, so replacing the agent when it is
the only window left doesn't tear the session down.

The session name is derived from the place (its uid included), not stored separately, so `breq` and
the daemon find the same session without coordinating. Because the uid changes when a slot is torn
down and set up again, a session left over from a previous incarnation — pointing at a directory
that no longer exists — is never mistaken for the current one and is reconciled away.

Who talks to rmux how:

| | how | why |
|---|---|---|
| `breq` | the `rmux` CLI | keeps working with the toren daemon stopped, and keeps `toren-lib` free of an async SDK |
| `toren-daemon` | `rmux-sdk` | needs pane byte streams and pane state events, which the CLI doesn't expose well |

Both reach the same rmux daemon over its Unix socket. That socket is the seam.

## Everyday workflow

```bash
breq do -p "fix the flaky test"     # creates the workspace, spawns the agent, attaches you to it
# ... work ...
# detach (rmux's detach chord) — the agent keeps running
breq sh one                        # attach to the same session's shell window
```

Open `https://<your-toren>/a/<segment>/one` in a browser and you get the same pane, live, in an
xterm.js terminal: same scrollback, same keystrokes, no second agent process. Close the tab; the
agent keeps running. Reattach from either side whenever.

If an agent is already running in that workspace, `breq do` refuses rather than replacing it —
spawning kills the existing agent window, and that agent may be one you or the daemon started
somewhere else. Attach to it (`breq sh <ws>`), or pass `--force` to replace it deliberately.

`breq do --no-rmux` (or `TOREN_NO_RMUX=1`) restores the old behaviour of exec'ing the agent
directly. If `rmux` isn't installed at all, that's the automatic fallback — you just don't get
detach-survival or browser attach.

## Living with zellij

rmux and zellij don't interoperate. There's no protocol between them; an rmux session is invisible
to zellij and vice versa. In practice that's fine, because they operate at different layers:

- **zellij stays your outer layer.** Tabs, panes, navigation, and your keybindings are unchanged.
  `breq do` run inside a zellij pane just execs `rmux attach` in that pane.
- **rmux is the inner layer, per workspace.** Splitting *inside* an agent session (a dev server
  pane, a log tail) is an rmux split, not a zellij one — that pane needs to live in the session the
  daemon can see.

What you give up is zellij-native pane management within an agent session. What you gain is that
the session outlives the terminal it was started from, and is reachable from a browser.

A reasonable arrangement is one zellij tab per workspace, each holding one `rmux attach`. You keep
zellij's tab ergonomics and rmux's persistence, and neither layer has to know about the other.

## Transcripts

rmux keeps scrollback in daemon memory only, and a pane whose process has exited loses its screen
entirely (`capture-pane` shows just `Pane is dead (status N, …)`). So the daemon also records every
pane it mirrors to `~/.toren/transcripts/<segment>/<workspace>/<uid>/<window>.raw` — raw bytes,
escape sequences and all. The uid in the path is the place's incarnation, so a slot reused three
times keeps three separate records rather than overwriting.

That file is the durable record, and it is what the browser replays. Attaching seeds the terminal
from the transcript's last 2 MB, then switches to the live stream — so a finished agent, or one
whose daemon has restarted since, still shows what it did instead of a blank screen. When output
has aged out of that window, the terminal says so before the replay.

You can also read it directly:

```bash
cat ~/.toren/transcripts/toren/one/aaa111/agent.raw
```

A `.raw.cursor` sidecar records how far the transcript has been written, so re-attaching to a pane
appends only what is new rather than replaying its history into the file a second time. Aged-out
transcripts are pruned by `breq cleanup --transcripts <days>`.

## Lifecycle notes

- **Daemon restart loses rmux sessions** if the rmux daemon went down with it; agents are not
  auto-respawned, since silently restarting an expensive agent is worse than reporting that it's
  gone. Start a new one with `breq do --resume`.
- **`breq teardown` kills the session**, but only after checking it. The session always holds an
  idle shell sitting in the workspace, which would otherwise block teardown forever — so it has to
  come down. But a live agent, or any pane running something other than an idle shell, means there
  is work in there: teardown refuses and tells you to pass `--kill`. (Note that the task verbs —
  `breq complete` / `abort` — never touch the session at all; they only update the tracker.)
- **The web terminal follows the current agent.** Replacing the agent — `breq do` again, or from the
  UI — re-points the mirror; open browsers are told the old pane ended and reconnect to the new one.
- **rmux's own web-share** (`rmux web-share`) is a separate feature from toren's web UI — it mints
  E2E-encrypted URLs for handing a session to another person. Toren's UI uses toren's own auth and
  embeds the terminal in the workspace page instead. Both can be used on the same session.
