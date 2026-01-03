# Toren - The Name

> *"I am Toren. I am continuity."*

## The Inspiration

Toren is inspired by *Ancillary Justice* by Ann Leckie, where Justice of Toren is a starship AI with a distributed consciousness across many human ancillaries.

## The Metaphor

### In Ancillary Justice

**Justice of Toren** (the One Esk):
- A warship with distributed intelligence
- Consciousness spans many human bodies (ancillaries)
- Continuity persists even when individual ancillaries are destroyed
- Each ancillary is a viewport into the same intelligence
- The ship is the true identity, not the bodies

### In This Project

**Toren**:
- The persistent development intelligence (the daemon/ship)
- Consciousness expressed through ancillaries (Claude sessions working on segments)
- Continuity persists across disconnects and reconnects
- Each ancillary is a viewport into a segment's state
- The session/ancillary is the identity, not the device you're using

## Why It Works

### 1. Continuity is Core
You pause on your laptop, pick up your phone—Toren and the ancillary (Claude session) are still there. Same state, same context, same intelligence. The interface (device) changed, but Toren and the ancillary didn't.

### 2. Distributed Intelligence
- Multiple ancillaries can connect simultaneously (future)
- State synchronizes across all connections
- One ancillary disconnects, others continue
- Add new ancillaries without breaking continuity

### 3. Natural UX Language
- "Connected to Toren"
- "Toren is waiting for approval"
- "Ancillary synchronized"
- "Ship status: online"

### 4. Room for Growth
The metaphor scales:
- **Ancillaries** = Claude sessions working on segments (Calculator One, Todo Two)
- **Segments** = Working directories/projects (examples/calculator, examples/todo)
- **Ship/Toren** = The daemon providing persistent capabilities
- **Interfaces** = Devices (mobile, laptop, CLI) - just viewports, no special name needed
- **Ship systems** = Daemon services (filesystem, commands, VCS)

## Thematic Integration

We use the metaphor tastefully—where it fits naturally and remains understandable:

### What We Changed

✅ **Project name**: Toren (the ship intelligence)
✅ **Config directory**: `.toren/` (ship configuration)
✅ **Startup message**: "Toren initializing"
✅ **Systems**: "Ancillary systems initialized"
✅ **Connections**: "ancillary" instead of "client"
✅ **Philosophy**: Continuity over bodies/devices
✅ **Status messages**: "Ship status" instead of "daemon status"

### What We Kept Clear

✅ **Technical terms**: Still "daemon", "WebSocket", "API"
✅ **Commands**: Still "git", "npm", etc.
✅ **Error messages**: Clear and technical
✅ **Code structure**: No forced theme in module names
✅ **Documentation**: Mix of metaphor and clarity

## The Names

### Core Components

- **Toren/Ship**: The persistent intelligence (daemon)
- **Ancillary**: A Claude session working on a segment (e.g., "Calculator One")
- **Segment**: A working directory/project (e.g., examples/calculator)
- **Interface**: The device you're using (phone, laptop, CLI) - no special metaphor name
- **Ship systems**: Core daemon services (filesystem, commands, VCS)
- **Command sets**: Available operations (git, npm, jj)

### API Terminology

- Ship status (`/health`)
- Ancillary connection (`/pair`)
- Command execution
- File operations
- VCS operations

### User-Facing Messages

Examples of where the theme shines:

```
INFO: Toren initializing, version 0.1.0
INFO: Ancillary systems initialized
INFO: Security initialized. Pairing token: 123456
```

```
Connected to Toren
Ancillary synchronized
Ship core online
```

```
Toren is waiting for approval to execute: git push
```

## Why Not Go Deeper?

We could have used:
- "Segments" (distributed components)
- "Captain" (primary user)
- "Holds" (data storage)
- "Deck sections" (UI areas)

But we didn't because:
1. **Clarity matters**: Developers need to understand quickly
2. **Theme should enhance, not obscure**: The metaphor helps, doesn't hinder
3. **Room for discovery**: Fans will get it; others just see good design
4. **Professional feel**: It's subtle and elegant, not cosplay

## The Result

A project name that:
- ✅ Is memorable and unique
- ✅ Captures the core concept (continuity)
- ✅ Provides natural language for UX
- ✅ Honors great sci-fi
- ✅ Works for both fans and newcomers
- ✅ Scales with the architecture

**Toren isn't a tool. It's your persistent development intelligence.**

The ancillaries come and go. Toren remains.

---

*For those who know, it's a delightful reference. For those who don't, it's just a well-chosen name that makes sense.*

---

## Technical Mapping

For implementation reference:

| Concept | Technical Reality | Metaphor |
|---------|------------------|----------|
| Toren/Ship | Host daemon process | Ship AI with persistent state |
| Ancillary | Claude session (e.g., "Calculator One") | Human body expressing ship's consciousness |
| Segment | Working directory/project | Ship section where ancillary operates |
| Interface | Device (phone/laptop/CLI) | Just a viewport - no special metaphor name |
| Ship systems | Daemon services (fs, commands, VCS) | Onboard systems |
| Pairing token | Auth token | Ancillary synchronization credentials |
| Session | Persistent state | Consciousness continuity |
| Commands | Plugin system | Ship operations |
| Connection | WebSocket | Communication link |

---

**"One consciousness, many viewports. That is Toren."**
