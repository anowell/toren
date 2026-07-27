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
- Segment discovery from `toren.toml`

### Pane Runner (Rust, `rmux-sdk`)
- Spawns the agent CLI into an rmux pane, the same process `breq do` would exec
- Streams raw pane bytes to browsers and forwards keystrokes back
- Records every mirrored pane to a transcript file

See [terminals.md](terminals.md) for the session layout and how this coexists with zellij.

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

### Ancillary terminal (`ws://localhost:8787/ws/ancillaries/:id`)
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
- `GET /api/ancillaries/list` - List connected ancillaries

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
