# Segment Management

Toren discovers and manages segments (projects) via `~/.toren/config.toml`.

## Configuration

```toml
[ancillaries]
# Segment globs: discover repos as segments
segments = ["~/proj/*", "~/work/special-repo"]
```

Glob entries (containing `*`, `?`, or `[`) are expanded — each matched directory becomes a segment. Non-glob entries are treated as literal segment paths.

When no segments are configured (or CWD isn't under any), breq infers the segment from the current repo's directory name.

## API

**List segments:**
```bash
curl http://localhost:8787/api/segments/list
```

**Create segment:**
```bash
curl -X POST http://localhost:8787/api/segments/create \
  -H "Content-Type: application/json" \
  -d '{"name": "my-project", "root": "examples"}'
```

## Discovery Methods

- **Globs**: Expand `~/proj/*` to find all subdirectories
- **Literal paths**: Explicit individual project paths
- **CWD inference**: Detect repo root from current directory (zero-config)

## CLI Usage

```bash
# Initialize a repo for breq (creates .toren.kdl, offers to register segment)
breq init

# Start a session (segment inferred from CWD)
breq cmd -p "implement feature X"
```
