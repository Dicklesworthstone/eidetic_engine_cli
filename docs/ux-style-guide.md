# ee CLI UX Style Guide

This document defines the output patterns for the `ee` command-line interface.
Golden fixtures in `tests/fixtures/golden/` enforce these patterns.

## Core Principles

1. **Stdout is data, stderr is diagnostics**: Machine-readable output (JSON, JSONL)
   goes to stdout. Progress bars, warnings, and debug info go to stderr.

2. **Deterministic output**: Given the same database state, the same command
   produces identical JSON output. IDs, timestamps, and ordering are stable.

3. **Agent-native by default**: Every command supports `--json` for machine
   consumption. The response envelope is always `ee.response.v1` or `ee.error.v1`.

4. **Useful errors**: Errors include a repair hint (`next` action) when possible.

## Response Envelope (ee.response.v1)

All successful JSON responses use this envelope:

```json
{
  "schema": "ee.response.v1",
  "data": { ... }
}
```

With `--meta`, additional fields are included:

```json
{
  "schema": "ee.response.v1",
  "meta": {
    "timestamp": "2026-01-01T00:00:00Z",
    "elapsed_ms": 42,
    "workspace_id": "wsp_..."
  },
  "data": { ... }
}
```

## Error Envelope (ee.error.v1)

All errors use this envelope:

```json
{
  "schema": "ee.error.v1",
  "error": {
    "code": "error_code",
    "message": "Human-readable description.",
    "repair": "ee command to fix it"
  }
}
```

The `repair` field is optional but should be present when a fix is known.

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Usage error (invalid arguments) |
| 2 | Configuration error |
| 3 | Storage error |
| 4 | Search/index error |
| 5 | Import error |
| 6 | Degraded mode unsatisfied |
| 7 | Policy denied |
| 8 | Migration required |

## Human Output Format

Human-readable output follows this structure:

### Success

```
<action verb>: <summary>

<details line 1>
<details line 2>

Next: <suggested follow-up command>
```

### Error

```
error: <short description>

<explanation paragraph>

Next:
  <repair command>

Details:
  <diagnostic command>
```

## Output Mode Selection

| Flag | Environment | Result |
|------|-------------|--------|
| `--json` | - | JSON to stdout |
| `--robot` | - | JSON to stdout (agent-oriented defaults) |
| - | `EE_FORMAT=json` | JSON to stdout |
| `--format toon` | - | TOON format |
| (none) | (none) | Human-readable |

## Color

- Color is enabled only when stdout is a TTY and `NO_COLOR` is not set.
- Machine-readable formats (`--json`, `--jsonl`, etc.) never use color.
- Use `--no-color` to force plain text.

## Golden Fixtures

Reference fixtures are in `tests/fixtures/golden/`:

- `error/*.golden` - Error envelope patterns
- `status/*.golden` - Status response patterns
- `version/*.golden` - Version response patterns
- `human/*.golden` - Human-readable output patterns

These fixtures are validated by tests to ensure the UX contract is maintained.
