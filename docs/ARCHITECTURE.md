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
              │     rmux daemon      │  ← `breq` and `rmux attach`
              │  sessions / panes    │    reach the same sessions
              └──────────────────────┘
```

## Components

### Daemon (Rust)
- WebSocket + REST API via Axum
- Session and auth management
- Sandboxed filesystem operations
- Command execution with streaming
- VCS abstraction (Git + Jujutsu)
- Segment discovery from `~/.toren/config.toml`

### State model

There is **no global registry** and no assignment store. A workspace is a *place* — a working copy
that the VCS knows about — and toren enumerates places by walking the VCS, not by consulting a side
file. Each place carries its own git-excluded state:

- `<ws>/.toren/annotations.json` — facts set on the place (title, linked tasks, chosen agent)
- `<ws>/.toren/cache.json` — derived values with timestamps (notably cached PR/CI delivery)

A short **uid**, minted at `breq setup`, distinguishes incarnations of a slot and is embedded in the
rmux session name (`toren-<segment>-<ws>-<uid>`) and transcript paths. Task-source-owned fields
(status, assignee) are never cached — they are read live through task resolvers. The daemon and
`breq` build the same `WorkspaceView` join over this, so `breq get <ws> --json` and
`GET /api/workspaces/:segment/:name` return the same shape.

### Pane Runner (Rust, `rmux-sdk`)
- Spawns the agent CLI into an rmux pane, the same process `breq do` would exec
- Streams raw pane bytes to browsers and forwards keystrokes back
- Records every mirrored pane to a transcript file

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
receives everything the pane has produced so far, then live output, with no gap between the two.
```typescript
// Requests
{ type: 'data', data: string }              // keystrokes
{ type: 'resize', cols: number, rows: number }
{ type: 'interrupt' }

// Responses
{ type: 'status', status: string, session: string }
{ type: 'error', message: string }
```

### REST Endpoints
- `POST /pair` - Exchange pairing token for session
- `GET /health` - Daemon status
- `GET /api/segments/list` - List discovered segments
- `GET /api/workspaces` - List every workspace (all segments)
- `GET /api/workspaces/:segment` - List a segment's workspaces
- `GET /api/workspaces/:segment/:name` - One workspace's full `WorkspaceView`
- `POST /api/workspaces/:segment/:name/start` - Start an agent (`{ agent?, prompt?, model?, resume? }`)
- `POST /api/workspaces/:segment/:name/stop` - Stop the agent

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
