# Toren Concepts

> *"I am Toren. I am continuity."*

Inspired by Ann Leckie's *Ancillary Justice*, where Justice of Toren is a starship AI with distributed consciousness across many human bodies.

## Core Metaphor

| Concept | What It Is | Example |
|---------|-----------|---------|
| **Toren/Ship** | The daemon - persistent intelligence | The running daemon process |
| **Segment** | A project/working directory | `examples/calculator` |
| **Ancillary** | A Claude session working on a segment | "Calculator One" |
| **Interface** | Your device (phone, laptop, CLI) | Just a viewport - no special name |

## Naming Convention

**Pattern**: `<SegmentName> <Number>`

Examples:
- `Calculator One` - First ancillary in calculator segment
- `Todo Two` - Second ancillary in todo segment
- `Howie Three` - Third ancillary in Howie segment

Numbers use words (One, Two, Three) following the books' convention ("One Esk Nineteen").

## Why This Works

1. **Continuity**: Switch devices freely - the ancillary persists
2. **Multiple ancillaries**: Same segment can have multiple ancillaries for parallel work
3. **Natural language**: "Calculator One connected" is more meaningful than "client-1"
4. **Scalable**: Maps to git worktrees / jj workspaces

## Structure Example

```
Toren (daemon)
├── Segment: Calculator
│   └── Calculator One (Claude session)
├── Segment: Todo
│   ├── Todo One
│   └── Todo Two
└── Segment: Api
    └── Api One
```

## The breq CLI

`breq` (bead request) manages work assignments between beads (tasks) and ancillaries.

### Core Principle

**Ancillary = Workspace**: An ancillary is in use exactly when its workspace exists. When work completes or is aborted, both the workspace and assignment are cleaned up together.

### Commands

| Command | Description | Bead Effect |
|---------|-------------|-------------|
| `breq assign <bead>` | Deploy ancillary on task | → in_progress |
| `breq complete <ref>` | Keep commits, cleanup workspace | → closed |
| `breq abort <ref>` | Discard work, cleanup workspace | → open (unassigned) |
| `breq abort --close` | Abort and close bead | → closed |
| `breq resume <ref>` | Continue work (recreates workspace if needed) | (reopens if needed) |

### Workflow Examples

**Complete a task:**
```bash
breq assign my-bead        # Creates workspace, assigns to Claude
# ... Claude works ...
breq complete one          # Cleanup, close bead, print commit ref
jj rebase -r abc123 -d main  # Integrate from default workspace
```

**Discard and retry:**
```bash
breq abort one             # Cleanup, bead returns to open
breq assign my-bead        # Start fresh
```

**Continue interrupted work:**
```bash
breq resume one            # Recreates workspace if needed, launches Claude
```

### State Recovery

Each command handles inconsistent states gracefully:
- `complete` - works even if workspace is already gone
- `abort` - works even if workspace is missing or assignment is stale
- `resume` - recreates workspace if missing, reopens bead if closed

## Environment Variables

```bash
SEGMENT_NAME=Calculator    # Auto-detected from directory if not set
ANCILLARY_NUMBER=One       # Defaults to "One"
SHIP_URL=ws://localhost:8787
SESSION_TOKEN=...          # From pairing
```

---

*Devices are interchangeable. Ancillaries persist. Toren endures.*
