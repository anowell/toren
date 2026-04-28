# Environment Variables in `toren.kdl`

The `env` directive sets environment variables for `run` commands inside `toren.kdl`. It works at four scopes (global, setup, destroy, run-child) and accepts two forms.

## Forms

```kdl
env "FILE" "FILE2"          // file form: load env files (repo-root-relative)
env PORT=3000 NODE_ENV=dev  // pair form: inline KEY=VALUE pairs
```

A single `env` node uses one form or the other — never both. To combine, use two nodes:

```kdl
env ".env"
env DEBUG=1 LOG_LEVEL=info
```

`KEY=VALUE` is parsed as a KDL property; KDL's grammar requires the LHS to be a valid bare identifier. To put `=` inside a value, quote the value: `URL="https://x.example?a=b"`.

## Scopes

```kdl
var greeting="hello"
env ".env.shared"               // 1. global: applies to all run commands below
env LOG_LEVEL=info

setup {
    env NODE_ENV=development    // 2. setup-block: applies to subsequent steps
    run "pnpm install" cwd="web"
    env DEBUG=1                 // accumulates procedurally
    run "pnpm build" cwd="web" {
        env DATABASE_URL="..."  // 3. run-child: scoped to this command only
    }
}

destroy {
    env CLEANUP_MODE=full       // 4. destroy-block: same rules, isolated from setup
    run "just teardown"
}
```

## Semantics

- **Procedural.** `env` is a step that mutates the environment for everything following it. Mirrors shell `export`.
- **Last-wins.** Multiple `env` nodes accumulate; later overrides earlier. Within a single file-form node with several paths, files load left-to-right.
- **Scopes nest.** Global → block (setup/destroy) → run-child. Inner scopes inherit and may override.
- **Run-child env never leaks.** Vars set inside `run "cmd" { env ... }` apply only to that command.
- **Process-env precedence.** Config wins over inherited shell env.
- **`KEY=VALUE` splits on the first `=`.** So `FOO=bar=baz` yields `FOO` → `bar=baz`.
- **Templating.** Minijinja runs on env *values* and on env-file *paths* (matching `template src=` and `copy src=`). It does *not* run on key names, and does *not* run on the *contents* of files loaded via the file form — those are read as literal `.env` files.

  ```kdl
  // path is templated; contents are not
  env "{{ ws.name }}.env"
  ```

## Env-file format

```
# Comments start with #
# Blank lines are skipped
KEY=value
ANOTHER=with spaces (no quotes needed)
SPLITS_ON_FIRST_EQUALS=foo=bar=baz
```

- `KEY=VALUE` per line. Split on the first `=`.
- Lines starting with `#` are comments. Blank lines are skipped.
- Keys must match `[A-Za-z_][A-Za-z0-9_]*`.
- Values are taken **literally** — no `export ` prefix, no shell expansion, no quote stripping. If you want quotes in your value, they end up in your value.
- Missing files are a **hard error** by default.

## Var name validation

Both `var NAME=VALUE` and `env NAME=VALUE` reject names that don't match `[A-Za-z_][A-Za-z0-9_]*` at parse time. KDL grammar rejects names containing `=` already.

## Currently deferred

- `env "FILE" optional=true` — silently skip missing file
- `env "FILE" preserve_existing=true` — don't override inherited shell env
- File form for `var` (`var "FILE"`)
- Mixing args and properties on a single `env`/`var` node
