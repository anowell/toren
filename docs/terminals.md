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
├── window "cmd"       a one-shot command              (holds when it finishes)
└── window "agent"     the coding agent                (replaced each `breq do`)
```

A workspace's session holds a *set* of windows: one or more shells, any commands you have run, and,
when an agent is running, an `agent` window. Every `breq sh` (and the web UI's "New shell") opens a
shell window of its own — two terminals running `breq sh` on one workspace are two shells, never two
mirrors of one pane — and each closes when you `exit` it, like any terminal. To watch a window that
already exists instead, name it: `breq sh <ws> --window <name>`.

**Whether a pane closes with its process is decided when the window is created**, not inferred
afterwards. A window that is just a shell closes; a window made from a *command* — `breq sh <ws> --
<cmd>`, or the agent — keeps `remain-on-exit` set and lingers as a dead pane. That is what makes an
agent's `exited` state observable in `breq list`, lets a browser still show what it did, and keeps a
finished command's output on screen until you dismiss it. A held pane grows a line saying how it
ended and what to do about it:

```
[exited 3 — <ENTER> re-run, <ESC> drop to shell, <Ctrl-c> close]
```

For an agent the offer is *resume* rather than re-run: the session id is known, so `<ENTER>`
continues that session instead of starting the agent cold — the same thing `breq do --resume` does.
`breq sh --hold` / `--no-hold` override the default in either direction; `--no-hold` is what keeps
`breq sh <ws> -- <cmd>` usable in a pipeline.

The line is drawn into the pane's byte stream rather than as chrome, so the browser shows the same
one, and its three keys work there too — with buttons beside them, since a held pane is also
dismissed from the window list. It has to be easy: a resume is always a *new* pane, so held ones
accumulate one per session until they are cleared.

Replacing a window's process always mints a new pane (so the daemon hands an attached browser over
to it) and never drops the session to zero windows, so replacing the agent when it is the only
window left doesn't tear the session down.

The session name is derived from the place (its uid included), not stored separately, so `breq` and
the daemon find the same session without coordinating. Because the uid changes when a slot is torn
down and set up again, a session left over from a previous incarnation — pointing at a directory
that no longer exists — is never mistaken for the current one and is reconciled away.

Who talks to rmux how:

| | how | why |
|---|---|---|
| `breq` | the `rmux` CLI, plus `toren-mirror` for the pane it is showing | session conventions keep working with the toren daemon stopped, and `toren-lib` stays free of an async SDK |
| `toren-daemon` | `rmux-sdk`, through the same `toren-mirror` | needs pane byte streams and pane state events, which the CLI doesn't expose well |

Both reach the same rmux daemon over its Unix socket. That socket is the seam.

**rmux is used only as a server.** Nothing runs `rmux attach`: the local terminal and the browser
are both thin *mirrors* of one pane — its bytes out, keystrokes in, resize — which is why there is
no prefix key, no status bar and no window list in the way, and why `exit` returns you to the shell
you came from. The app inside still sees rmux's `TERM` (`tmux-256color`); that is inherent to one
pty being rendered in two places at once.

## Everyday workflow

```bash
breq do -p "fix the flaky test"     # creates the workspace, spawns the agent, mirrors its pane
# ... work ...
# close the terminal — the pane, and the agent in it, keep running in rmux
breq sh one                        # open a shell of your own in the same session
breq sh one --window agent         # watch the agent that is already running there
```

There is no detach *chord*: keystrokes all belong to the pane, so leaving is closing the terminal
(or killing `breq`). Coming back to an *agent* is `breq do` / `breq sh <ws> --window agent`, which
mirrors the pane that is there rather than starting a second one; a bare `breq sh` always opens a
fresh shell.

Open `https://<your-toren>/a/<segment>/one` in a browser and you get the same pane, live, in an
xterm.js terminal: same scrollback, same keystrokes, no second agent process. Close the tab; the
agent keeps running. Reattach from either side whenever.

The browser also starts sessions: "New \<agent\> agent" per configured agent, and "Resume Previous
Session" listing what this workspace has recorded — the same list `breq do --resume <sessionId>`
takes an id from. If the terminal ever falls out of step (a burst of output, a connection that
stalled), the daemon repaints the pane's screen rather than streaming on into a broken one.

If an agent is already running in that workspace, `breq do` refuses rather than replacing it —
spawning kills the existing agent window, and that agent may be one you or the daemon started
somewhere else. Watch it (`breq sh <ws> --window agent`), or pass `--force` to replace it
deliberately. `--window` mirrors any window of the session by name, which is also how you get back
to a shell or a command you left running.

`breq do --no-rmux` (or `TOREN_NO_RMUX=1`) restores the old behaviour of exec'ing the agent
directly. If `rmux` isn't installed at all, that's the automatic fallback — you just don't get
detach-survival or browser attach.

## What a mirror is responsible for

A mirror is a thin thing, but "one pane, several viewers" breaks four assumptions a terminal is
built on, and the mirror is where each is put back.

**One connection per mirror.** rmux allows **16 pane-output subscriptions per connection** — a
server-side limit, measured rather than guessed (`cargo run -p toren-mirror --example rmux_spike --
caps`). Every mirror therefore takes a connection of its own. Sharing one put a ceiling on how many
panes a workspace could show at once, and mirrors past the ceiling reported live panes as *exited
and being held* — which is also why starting an agent in one answered `409 already running`. A
failed subscription now means "ask the pane what it is doing", never "the pane is over".

**Mirrors are reference-counted.** A mirror costs a connection, a subscription and a pump, so it
exists while somebody is watching and is swept about half a minute after the last viewer leaves —
long enough to absorb a tab switch, a reload, or a socket a phone dropped. An unwatched mirror also
drops to the slow polling clock rather than chasing output nobody reads.

**Liveness is watched separately from mirroring, and pushed.** The daemon keeps a `state_events`
subscription on every pane it knows about whether or not anything is showing it — a long poll on a
transport of its own, no output subscription, no forked `list-panes`. That is what makes killing a
process in a window you are not looking at show up immediately rather than the next time you click
into it; `/ws/lifecycle` is where it comes out. The old design observed liveness only in a mirror's
pump, so unwatched panes had no watcher at all.

**One PTY has one size, so one viewer owns it.** Both a terminal and a browser tab resizing the same
pane meant last-write-wins, and the loser rendered a screen laid out for the winner — the cursor at
the bottom of a pane whose UI is drawn higher up. The pane now carries a note saying which viewer is
sizing it, which both `breq` and the daemon can read, and the note goes to whoever typed most
recently (tmux's `window-size latest`, arrived at independently). Viewers that do not own the size
render the pane's grid exactly, scaled to fit their window, with a button to take the size. A viewer
that leaves hands the size back — which is what makes "the terminal closed, resize to the browser"
work without anything having to detect that the terminal closed.

**The mirror is the only party to a terminal query.** A query and its answer are a conversation
between one program and one terminal; N viewers means N answers to a question asked once. rmux
already answers CPR, DA1/2/3, DSR, DECRQM, XTVERSION, OSC colour queries and XTGETTCAP, so the
mirror drops them on the way out — and drops viewers' *replies* on the way in, where they would
otherwise arrive at a program that was answered long ago and land on whatever reads stdin next.
That is the stray-character symptom. Image and terminfo payloads (kitty graphics, Sixel, DECUDK)
are content rather than conversation and pass through untouched.

## Living with zellij

rmux and zellij don't interoperate. There's no protocol between them; an rmux session is invisible
to zellij and vice versa. In practice that's fine, because they operate at different layers:

- **zellij stays your outer layer.** Tabs, panes, navigation, and your keybindings are unchanged.
  `breq do` run inside a zellij pane is just a program in that pane, with no keys of its own —
  there is no second prefix to fight yours.
- **rmux is the inner layer, per workspace.** A second process for a workspace (a dev server, a log
  tail) is another window in its rmux session — `breq sh <ws> -- <cmd>` — so it lives where the
  daemon and the browser can see it.

What you give up is zellij-native management of the workspace's other processes. What you gain is
that they outlive the terminal they were started from, and are reachable from a browser.

A reasonable arrangement is one zellij tab per workspace, each holding one `breq do` or `breq sh`.
You keep zellij's tab ergonomics and rmux's persistence, and neither layer has to know about the
other.

## What is kept

Toren records no raw terminal output of its own. Every layer already keeps the record that suits
it: a coding agent keeps its own structured session, a shell keeps its command history, and rmux
keeps scrollback. Attaching a browser seeds the terminal from whatever rmux still holds for the
pane, then switches to the live stream.

What toren records instead is the link between those records: when a workspace is torn down, the
destroy event in `~/.toren/logs/` carries the agent that worked there and that agent's own session
id, so the incarnation can still be traced back to the agent's transcript of it long after the
working copy is gone.

## Lifecycle notes

- **Daemon restart loses rmux sessions** if the rmux daemon went down with it; agents are not
  auto-respawned, since silently restarting an expensive agent is worse than reporting that it's
  gone. Start a new one with `breq do --resume`.
- **`breq destroy` kills the session**, but only after checking it. The session always holds an
  idle shell sitting in the workspace, which would otherwise block destroy forever — so it has to
  come down. But a live agent, or any pane running something other than an idle shell, means there
  is work in there: destroy refuses and tells you to pass `--kill`. (The `breq complete` / `abort`
  scripts end in `destroy --kill` for that reason — by the time they run, the tracker says the work
  is finished or handed back, so whatever is still in the panes is not worth keeping.)
- **The web terminal follows the current agent.** Replacing the agent — `breq do` again, or from the
  UI — re-points the mirror; open browsers are told the old pane ended and reconnect to the new one.
- **rmux's own web-share** (`rmux web-share`) is a separate feature from toren's web UI — it mints
  E2E-encrypted URLs for handing a session to another person. Toren's UI uses toren's own auth and
  embeds the terminal in the workspace page instead. Both can be used on the same session.
