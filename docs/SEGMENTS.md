# Segment Management

Toren discovers and manages segments (projects) via `toren.toml`.

## Configuration

```toml
[segments]
# Auto-discover subdirectories
globs = ["examples/*"]

# Where new segments can be created
roots = ["examples"]

# Explicit paths (optional)
paths = []
```

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

- **Globs**: Scan directories matching patterns (`examples/*`)
- **Roots**: Directories where new segments can be created
- **Paths**: Explicit individual project paths

## CLI Usage

```bash
# Start session in a segment (auto-discovered from config)
just prompt examples/calculator
```
