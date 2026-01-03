# Toren Naming Conventions

## Ancillary Justice Mapping

Following the naming from Ann Leckie's *Ancillary Justice*:

### In the Books
- **Ship**: Justice of Toren (the One Esk)
- **Segments**: One Esk, Two Esk, Kalr, Bo, etc. (deck/hull sections)
- **Ancillaries**: One Esk Nineteen, Kalr Five, Bo Nine (individual bodies)

### In Toren
- **Ship**: Toren (the daemon/core intelligence)
- **Segments**: Repository names (Howie, Agent, MyProject)
- **Ancillaries**: Workspace instances (Howie One, Agent Two, MyProject Three)

## Structure

```
Toren
├── Segment: Howie
│   ├── Howie One    (workspace/ancillary #1)
│   ├── Howie Two    (workspace/ancillary #2)
│   └── Howie Three  (workspace/ancillary #3)
├── Segment: Agent
│   ├── Agent One
│   └── Agent Two
└── Segment: MyProject
    └── MyProject One
```

## Ancillary Naming Format

**Pattern**: `<SegmentName> <Number>`

Where:
- `SegmentName` = Repository/directory name (capitalized)
- `Number` = Word form (One, Two, Three, Four, Five, etc.)

**Examples**:
- `Howie One` - First workspace in Howie repo
- `Agent Two` - Second workspace in Agent repo
- `Claude Five` - Fifth workspace in Claude repo

## Implementation

### Ancillary Side (TypeScript)

```typescript
// Auto-detect from directory name
const segmentName = process.env.SEGMENT_NAME || detectSegmentName();
const ancillaryNumber = process.env.ANCILLARY_NUMBER || 'One';
const ancillaryId = `${segmentName} ${ancillaryNumber}`;

// Result: "Howie One"
```

### Ship Side (Rust)

The daemon tracks which ancillaries are connected to which segments:

```toml
# .toren/config.toml
[[segments]]
name = "Howie"
path = "/Users/you/code/howie"
ancillaries = ["One", "Two"]

[[segments]]
name = "Agent"
path = "/Users/you/code/agent"
ancillaries = ["One"]
```

## Number Words

Numbers are converted to words for readability:

| Number | Word | Example |
|--------|------|---------|
| 1 | One | Howie One |
| 2 | Two | Howie Two |
| 3 | Three | Agent Three |
| 4 | Four | Claude Four |
| 5 | Five | Toren Five |
| ... | ... | ... |
| 19 | Nineteen | (like One Esk Nineteen) |
| 20 | Twenty | Howie Twenty |

For numbers > 20, use numeric or extended word libraries.

**Future**: Use `num2words` crate (Rust) for comprehensive number-to-word conversion.

## Future: Git Worktrees & JJ Workspaces

Eventually ancillaries will map to:
- **Git worktrees**: Multiple checkouts of the same repo
- **Jujutsu workspaces**: Native jj workspace support

```bash
# Git worktree example
git worktree add ../howie-two main
# → Ancillary: "Howie Two"

# Jujutsu workspace example
jj workspace add ../agent-three
# → Ancillary: "Agent Three"
```

## Connection Messages

When an ancillary connects:

```
Ancillary Howie One initializing...
Connecting to ship: ws://localhost:8787
Howie One connected to Toren
Howie One synchronized with Toren
Howie One ready for directives
```

When disconnecting:

```
Howie One disconnecting...
Howie One disconnected from Toren
```

## Environment Variables

```bash
# Required
ANTHROPIC_API_KEY=...

# Optional (auto-detected if not provided)
SEGMENT_NAME=Howie           # Defaults to directory name
ANCILLARY_NUMBER=One         # Defaults to "One"

# Connection
SHIP_URL=ws://localhost:8787
SESSION_TOKEN=...            # From pairing
```

## Rationale

### Why This Works

1. **Natural Language**: "Howie One connected" is more meaningful than "client-1 connected"
2. **Scalable**: Works for multiple workspaces of same repo
3. **Familiar**: Fans of the books will recognize the pattern
4. **Functional**: Maps perfectly to git worktrees/jj workspaces
5. **Memorable**: Easy to remember which ancillary you're working in

### Why Word Numbers?

Following the books' convention:
- "One Esk Nineteen" not "One Esk 19"
- "Kalr Five" not "Kalr 5"
- More human, less mechanical
- Fits the distributed consciousness metaphor

## Examples in Practice

### Single Developer, Multiple Projects

```
Toren
├── MyApp One      (primary dev workspace)
├── Website One    (blog workspace)
└── Scripts One    (utility scripts)
```

### Single Project, Multiple Workspaces

```
Toren/Howie
├── Howie One      (main branch)
├── Howie Two      (feature/new-ui)
└── Howie Three    (hotfix/bug-123)
```

### Mobile + Laptop

```
Laptop → Howie One connected
Mobile → Howie One connected   (same ancillary, reconnected)

# OR

Laptop → Howie One connected
Mobile → Howie Two connected   (different worktree)
```

## Technical Details

### Auto-Detection

If `SEGMENT_NAME` not provided, detect from:
1. Current directory name
2. Git/jj repo name
3. Parent directory name

```typescript
export function detectSegmentName(): string {
  const cwd = process.cwd();
  const dirName = path.basename(cwd);
  return capitalize(dirName);
}
```

### Collision Handling

If `Howie One` already connected:
- From same device: Reconnect to existing session
- From different device: Allow (shared consciousness)
- New workspace needed: Use `Howie Two`

### Persistence

Ship tracks active ancillaries in memory:

```rust
struct Segment {
    name: String,
    path: PathBuf,
    active_ancillaries: HashMap<String, AncillaryConnection>,
}
```

## Future Enhancements

1. **Auto-numbering**: Ship suggests next available number
2. **Workspace sync**: Detect git worktrees/jj workspaces automatically
3. **Segment groups**: Organize segments by project
4. **Ancillary roles**: Mark primary/secondary ancillaries
5. **History**: Track which ancillary made which changes

---

**Remember**: Toren is continuity. Segments are locations. Ancillaries are bodies. One consciousness, many viewports.
