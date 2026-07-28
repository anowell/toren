# Architecture

## System Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    Client (Web/Android/CLI)                  │
└──────────────────────────┬───────────────────────────────────┘
                           │ WebSocket
┌──────────────────────────┼───────────────────────────────────┐
│                   Toren Daemon (Rust)                        │
│  ┌───────────────────────┴────────────────────────────┐     │
│  │              API Gateway (Axum)                     │     │
│  │  - Session Management                               │     │
│  │  - Auth (token-based)                               │     │
│  └──────┬───────────┬──────────────┬──────────────┬───┘     │
│  ┌──────▼──────┐ ┌──▼────────┐ ┌──▼─────────┐ ┌──▼────────┐ │
│  │  Filesystem │ │  Command  │ │    VCS     │ │  Segments │ │
│  │   Service   │ │  Executor │ │  Manager   │ │  Manager  │ │
│  └─────────────┘ └───────────┘ └────────────┘ └───────────┘ │
│         ┌──────────────────────────────────┐                │
│         │  Pane Runner (rmux-sdk)          │                │
│         │  - spawns agents into rmux panes │                │
│         │  - mirrors pane bytes to clients │                │
│         └──────────────┬───────────────────┘                │
└────────────────────────┼────────────────────────────────────┘
                         │ Unix socket
              ┌──────────▼───────────┐
              │     rmux daemon      │  ← `breq` and the toren daemon
              │  sessions / panes    │    mirror the same panes
              └──────────────────────┘
```

## Components

### Daemon (Rust)
- WebSocket + REST API via Axum
- Session and auth management
- Sandboxed filesystem operations
- Command execution with streaming
- VCS abstraction (Git + Jujutsu)
- Segment discovery from `~/.toren/config.kdl`

### State model

There is **no global registry** and no assignment store. A workspace is a *place* — a working copy
that the VCS knows about — and toren enumerates places by walking the VCS, not by consulting a side
file. Each place carries its own git-excluded state:

- `<ws>/.toren/state.json` — durable facts about the place (title, linked tasks, chosen agent),
  versioned and written atomically
- `<ws>/.toren/cache.json` — derived values with timestamps (notably cached PR/CI delivery)

A short **uid**, minted at `breq setup`, distinguishes incarnations of a slot and is embedded in the
rmux session name (`toren-<segment>-<ws>-<uid>`). Task-source-owned fields
(status, assignee) are never cached — they are read live through task resolvers. The daemon and
`breq` build the same `WorkspaceView` join over this, so `breq get <ws> --json` and
`GET /api/workspaces/:segment/:name` return the same shape.

### Pane Runner (Rust, `rmux-sdk`)
- Spawns the agent CLI into an rmux pane, the same process `breq do` would exec
- Streams raw pane bytes to browsers and forwards keystrokes back

See [terminals.md](terminals.md) for the session layout and how this coexists with zellij.

### Resolver plugins

Trackers, agents, and forges are Rhai resolver plugins under `~/.toren/plugins/` in three families —
`tasks/`, `agents/`, `delivery/`. Adding an integration is one `.rhai` file, no rebuild. See
[plugins.md](plugins.md).

## Protocols

### WebSocket (`ws://localhost:8787/ws`)
```typescript
// Requests
{ type: 'Auth', token: string, ancillary_id?: string, segment?: string }
{ type: 'Command', request: CommandRequest }

// Responses
{ type: 'AuthSuccess', session_id: string }
{ type: 'CommandOutput', output: CommandOutput }
{ type: 'Error', message: string }
```

### Workspace terminal (`ws://localhost:8787/ws/workspaces/:segment/:name`)
Binary frames carry raw pane bytes; text frames carry JSON control messages. On connect the client
receives a paint of the pane's screen, then live output, with no gap between the two.

Frames *from* the daemon open with a big-endian `u32` epoch. Re-seeding paints the whole screen, so
bytes from before a paint are wrong rather than late: both ends discard anything from an earlier
epoch, and a client seeing a new one clears before applying it. Frames *to* the daemon are
keystrokes, unprefixed.

The socket is kept alive from both sides: the daemon sends protocol pings (a browser answers those
itself) and the browser sends `ping` as JSON, since its API cannot send a protocol one.
```typescript
// Requests
{ type: 'data', data: string }              // keystrokes
{ type: 'resize', cols: number, rows: number }
{ type: 'interrupt' }
{ type: 'resync' }                          // repaint me: this terminal looks wrong
{ type: 'ping' }

// Responses
{ type: 'status', status: string, session: string }
{ type: 'error', message: string }
{ type: 'pong' }
```

### REST Endpoints
- `POST /pair` - Exchange pairing token for session
- `GET /health` - Daemon status
- `GET /api/segments/list` - List discovered segments
- `GET /api/workspaces` - List every workspace (all segments)
- `GET /api/workspaces/:segment` - List a segment's workspaces
- `GET /api/workspaces/:segment/:name` - One workspace's full `WorkspaceView`
- `POST /api/workspaces/:segment/:name/start` - Start an agent (`{ agent?, prompt?, model?, resume?, session? }`)
- `POST /api/workspaces/:segment/:name/stop` - Stop the agent
- `POST /api/workspaces/:segment/:name/shell` - Open a new shell window
- `GET /api/workspaces/:segment/:name/sessions` - The workspace's recorded agent sessions
- `POST /api/workspaces/:segment/:name/windows/:window/close` - Dismiss one window (a held pane)
- `GET /api/agents` - Agents this daemon can start, and the configured default

## Security

- Token-based pairing (6-digit PIN)
- Session JWT after pairing
- Directory sandboxing (approved_directories)
- Command approval system

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Daemon | Rust, Tokio, Axum |
| Agent | TypeScript, Anthropic SDK |
| Web UI | SvelteKit |
| Mobile | Kotlin, Jetpack Compose (future) |
| VCS | Git, Jujutsu |
