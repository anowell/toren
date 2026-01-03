# Scripts Directory

Internal scripts used by the justfile commands.

## Files

- **toren-cli.ts** - Main CLI for sending prompts to Claude
  - Used by: `just prompt`
  - Handles connection, authentication, and streaming responses

- **test-calculator-kata.ts** - End-to-end test suite
  - Used by: `just test-calculator`
  - Proves the system works by having Claude build a calculator

## Usage

Don't run these directly. Use the justfile commands instead:

```bash
# Instead of: npx tsx scripts/toren-cli.ts "..."
just prompt "Build something"

# Instead of: npx tsx scripts/test-calculator-kata.ts
just test-calculator
```

See `just --list` for all available commands.
