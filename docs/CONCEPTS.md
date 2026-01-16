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

## Environment Variables

```bash
SEGMENT_NAME=Calculator    # Auto-detected from directory if not set
ANCILLARY_NUMBER=One       # Defaults to "One"
SHIP_URL=ws://localhost:8787
SESSION_TOKEN=...          # From pairing
```

---

*Devices are interchangeable. Ancillaries persist. Toren endures.*
