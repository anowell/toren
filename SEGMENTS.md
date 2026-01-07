# Segment Management

## Overview

Toren now supports multi-segment (project) discovery and management through a TOML configuration file.

## Configuration

Create or edit `toren.toml` in your project root:

```toml
[server]
host = "127.0.0.1"
port = 8787

[segments]
# Auto-discover all subdirectories as segments
globs = ["examples/*"]

# Roots where new segments can be created
roots = ["examples"]

# Individual segment paths (optional)
paths = []

[ancillary]
max_concurrent = 5
default_model = "claude-sonnet-4-5-20250929"

[auto_approve]
non_vcs_commands = true
vcs_commands = false
file_operations = false

approved_directories = ["."]
```

## Usage

### List Available Segments

```bash
curl http://localhost:8787/api/segments/list | jq
```

Returns:
```json
{
  "segments": [
    {
      "name": "calculator",
      "path": "/Users/you/proj/examples/calculator",
      "source": "glob"
    },
    {
      "name": "fizzbuzz",
      "path": "/Users/you/proj/examples/fizzbuzz",
      "source": "glob"
    }
  ],
  "roots": [
    "/Users/you/proj/examples"
  ],
  "count": 2
}
```

### Create New Segment

```bash
curl -X POST http://localhost:8787/api/segments/create \
  -H "Content-Type: application/json" \
  -d '{
    "name": "my-project",
    "root": "/Users/you/proj/examples"
  }'
```

## Integration

### Web UI

The web interface will show:
- List of available segments
- Active ancillaries per segment
- Button to create new segments
- Segment selector when starting a session

### CLI (`just prompt`)

```bash
# Start session in a segment
just prompt examples/calculator

# The segment path is auto-discovered from config
```

## Concept

- **Segment Globs**: Directories to scan for existing projects (`examples/*`)
- **Segment Roots**: Where new projects can be created (`examples`)
- **Segment Paths**: Individual projects outside globs

This allows you to:
1. Have Toren auto-discover all projects in a directory
2. Control where new projects can be created
3. Explicitly add projects from anywhere

## Next Steps

- Web UI implementation for segment management
- Ancillary-to-segment binding in WebSocket protocol
- Session persistence across segments
