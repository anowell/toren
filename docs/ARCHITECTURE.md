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
│         │  Claude Agent (TypeScript)       │                │
│         │  - Anthropic SDK integration     │                │
│         │  - Tool implementations          │                │
│         └──────────────────────────────────┘                │
└─────────────────────────────────────────────────────────────┘
```

## Components

### Daemon (Rust)
- WebSocket + REST API via Axum
- Session and auth management
- Sandboxed filesystem operations
- Command execution with streaming
- VCS abstraction (Git + Jujutsu)
- Segment discovery from `toren.toml`

### Agent Runtime (TypeScript)
- Anthropic SDK integration
- Tool implementations (read, write, execute, vcs)
- Session persistence

### Command Plugin System
```yaml
# Example: plugins/git.yaml
commands:
  - id: "commit"
    command: "git commit -m {message}"
    auto_approve: false
```

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
