# Remogentic Architecture

**Mobile Remote Coding Agent System**

## System Overview

Remogentic enables developers to control Claude Code from Android devices against existing dev environments. It's a diff-first, command-centric system optimized for mobile directive development.

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                      Android Client (Kotlin)                 │
│  ┌─────────────┬──────────────┬─────────────┬──────────────┐│
│  │   Chat UI   │ Diff Viewer  │  Commands   │  File Browse ││
│  └─────────────┴──────────────┴─────────────┴──────────────┘│
│                          │ (WebSocket/gRPC)                  │
└──────────────────────────┼───────────────────────────────────┘
                           │
                    [Secure Tunnel]
                           │
┌──────────────────────────┼───────────────────────────────────┐
│            Host Daemon (Rust) - Dev Machine                  │
│  ┌──────────────────────┴────────────────────────────┐      │
│  │              API Gateway (Rust)                    │      │
│  │  - Session Management                              │      │
│  │  - Auth (token-based)                              │      │
│  │  - Stream multiplexing                             │      │
│  └──────┬───────────┬──────────────┬──────────────┬──┘      │
│         │           │              │              │          │
│  ┌──────▼──────┐ ┌──▼────────┐ ┌──▼─────────┐ ┌──▼────────┐│
│  │  Filesystem │ │  Command  │ │    VCS     │ │Port Fwd   ││
│  │   Service   │ │  Executor │ │  Manager   │ │  Service  ││
│  └─────────────┘ └─────┬─────┘ └────────────┘ └───────────┘│
│                        │                                     │
│                  ┌─────▼──────┐                             │
│                  │  Command   │                             │
│                  │Plugin System│                            │
│                  └────────────┘                             │
│         ┌──────────────────────────────────┐               │
│         │  Claude Agent SDK v2 (TypeScript) │               │
│         │  - Agent Runtime                  │               │
│         │  - Tool Implementations           │               │
│         │  - Session Management             │               │
│         │  - Diff Generation                │               │
│         └──────────────────────────────────┘               │
└─────────────────────────────────────────────────────────────┘
             │
             ▼
    [Dev Environment: Filesystem, Git/jj, Services]
```

## Component Details

### 1. Host Daemon (Rust)

**Purpose**: Secure bridge between mobile client and dev environment

**Responsibilities**:
- API endpoint serving (WebSocket primary, REST fallback)
- Session and auth management
- Filesystem operations (sandboxed to approved dirs)
- Command execution with output streaming
- VCS operations (git/jj abstraction)
- Port forwarding coordination

**Key Modules**:
- `api_gateway` - Request routing, WebSocket handling
- `fs_service` - Sandboxed file operations, watching
- `cmd_executor` - Process spawning, streaming, timeout handling
- `vcs_manager` - VCS abstraction layer
- `port_forward` - Tunnel management
- `security` - Auth tokens, directory approval

### 2. Claude Agent SDK v2 Integration (TypeScript)

**Purpose**: Claude Code runtime environment

**Responsibilities**:
- Agent lifecycle management
- Tool implementation (filesystem, command, VCS)
- Diff generation and structured output
- Session persistence and resume
- Guarded execution modes
- Context management

**Key Modules**:
- `agent_runtime` - SDK integration, event loop
- `tools/` - Tool implementations matching daemon APIs
- `diff_engine` - Diff generation and application
- `session_store` - State persistence
- `guard_system` - Approval gates, auto-approve rules

### 3. Command Plugin System

**Purpose**: Extensible, declarative command definitions

**Design**:
```yaml
# Example: .remogentic/commands/git-flow.yaml
command_set:
  id: "git-flow"
  name: "Git Flow Workflow"
  vcs: "git"
  commands:
    - id: "feature-start"
      label: "Start Feature"
      command: "git flow feature start {name}"
      params:
        - name: "name"
          type: "string"
          prompt: "Feature name?"
      category: "branch"
      icon: "git-branch"

    - id: "commit"
      label: "Commit Changes"
      command: "git commit -m {message}"
      params:
        - name: "message"
          type: "string"
          prompt: "Commit message?"
      category: "change"
      auto_approve: false
```

**Features**:
- Hot-reload on file change
- Per-project command sets
- Custom renderers (optional)
- Used by both Claude and UI
- Standard set for git, jj, npm, etc.

### 4. Android App (Kotlin)

**Purpose**: Mobile-first developer control surface

**Architecture**:
- **MVVM pattern** with Jetpack Compose
- **Repository pattern** for data layer
- **WebSocket client** for real-time updates
- **Offline-capable** with local caching

**Screens**:
1. **Environments** - Select/manage dev environments
2. **Chat** - Primary directive interface
3. **Diffs** - View pending/recent changes
4. **Files** - Read-only code browser
5. **Commands** - Quick actions panel
6. **Settings** - Connection, preferences

**Key Features**:
- Optimistic UI updates
- Real-time diff streaming
- Reconnection handling
- Background sync
- Push notifications for long-running tasks

## Data Flow Examples

### Example 1: User Issues Directive

```
[Mobile] User types: "Add validation to login form"
    ↓
[Mobile] Send chat message via WebSocket
    ↓
[Daemon] Route to Claude Agent SDK
    ↓
[Claude] Analyze codebase, generate plan
    ↓
[Claude] Produce diffs
    ↓
[Daemon] Stream diffs back to mobile
    ↓
[Mobile] Display diffs in viewer
    ↓
[Mobile] User approves
    ↓
[Daemon] Apply diffs to filesystem
    ↓
[Mobile] Show success, updated file states
```

### Example 2: Command Execution

```
[Mobile] User taps "Run Tests" command
    ↓
[Mobile] Send command execution request
    ↓
[Daemon] Check approval rules
    ↓
[Daemon] Execute via cmd_executor
    ↓
[Daemon] Stream output in real-time
    ↓
[Mobile] Display streaming output
    ↓
[Daemon] Send completion status
    ↓
[Mobile] Update UI with result
```

## Security Model

**Trust Boundary**: Host daemon trusts the mobile app via token auth

**Protections**:
- Token-based authentication (stored securely on device)
- Directory sandboxing (explicit approval required)
- Command allowlisting (configurable per environment)
- No arbitrary code execution from mobile
- All Claude actions go through approval gates

**Auth Flow**:
1. Daemon generates pairing token on startup
2. User enters token in mobile app (QR code or manual)
3. Mobile receives session JWT
4. All subsequent requests use JWT

## Diff Handling Strategy

**Format**: Unified diff (standard patch format)

**Generation**:
- Claude produces diffs via Agent SDK tools
- Diffs are validated before transmission
- File context included (configurable lines)

**Application**:
- Server-side application using `patch` or Rust library
- Atomic: all-or-nothing per diff set
- Conflict detection and reporting
- Rollback support

**Mobile Display**:
- Syntax highlighting via library
- Collapsible hunks
- Side-by-side on landscape
- Jump to file

## VCS Integration

**Abstraction Layer**:
```rust
trait VcsManager {
    fn status(&self) -> Result<VcsStatus>;
    fn diff(&self, options: DiffOptions) -> Result<String>;
    fn commit(&self, message: &str) -> Result<()>;
    fn branch_info(&self) -> Result<BranchInfo>;
    // ... other common operations
}

impl VcsManager for GitManager { ... }
impl VcsManager for JjManager { ... }
```

**jj-Specific Features**:
- Tool APIs expose jj operations (squash, describe, etc.)
- Claude can reason about jj workflows
- Custom command renderers for jj UI

**Detection**:
- Check for `.jj/` or `.git/`
- User can override detection

## Port Forwarding

**Strategy**: Lightweight tunnel using existing tools

**Options**:
1. SSH reverse tunnel (requires SSH access)
2. Cloudflare Tunnel (free tier)
3. LocalTunnel (simple, open-source)

**Implementation**:
- Daemon manages tunnel lifecycle
- Expose only requested ports
- Generate unique URLs
- Display URLs in mobile app
- Open in mobile browser

## V1 Scope

**Must Have**:
- ✅ Host daemon (basic API, filesystem, commands)
- ✅ Claude Agent SDK integration (chat, diffs, basic tools)
- ✅ Android app (chat, diffs, commands)
- ✅ Git support (basic operations)
- ✅ Command plugin system (YAML definitions)
- ✅ Token auth
- ✅ Directory sandboxing

**V1.x (Soon After)**:
- jj support
- Port forwarding
- Custom command renderers
- Advanced VCS workflows
- Session persistence
- Multi-environment management

**Future**:
- iOS client
- Collaborative features (multi-user)
- Advanced conflict resolution
- Plugin marketplace
- Desktop app (uses same daemon)

## Technology Stack Summary

| Component | Technology | Rationale |
|-----------|-----------|-----------|
| Host Daemon | Rust | Performance, safety, ecosystem |
| Agent Runtime | TypeScript (Node.js) | Agent SDK v2 requirement |
| Mobile Client | Kotlin (Jetpack Compose) | Native Android, modern |
| Diff Library | Rust (similar/diff-rs) | Fast, integrated |
| WebSocket | Rust (tokio-tungstenite) | Async, efficient |
| API Protocol | JSON-RPC over WebSocket | Simple, extensible |
| Tunnel | SSH/Cloudflare/Local | Configurable, secure |

## Implementation Phases

### Phase 1: Core Infrastructure (Current)
- Project setup
- Host daemon skeleton
- Basic API endpoints
- Simple filesystem operations
- Command execution

### Phase 2: Agent Integration
- Claude SDK v2 setup
- Tool implementations
- Diff generation
- Session management

### Phase 3: Command System
- Plugin loader
- YAML parser
- Command execution integration
- Basic git commands

### Phase 4: Mobile Foundation
- App structure
- API client
- Basic UI screens
- WebSocket connection

### Phase 5: Integration & Testing
- End-to-end flows
- Error handling
- Performance optimization
- Documentation

## Open Decisions

1. **WebSocket vs gRPC**: WebSocket chosen for simplicity, gRPC considered for future
2. **Diff library**: Evaluate similar-rs vs patch-rs
3. **Tunnel solution**: Start with SSH, add alternatives later
4. **Session storage**: SQLite vs filesystem JSON
5. **Android min SDK**: Target API 26+ (Android 8.0+)

---

**Document Status**: Initial design - Subject to evolution during implementation
**Last Updated**: 2026-01-02
