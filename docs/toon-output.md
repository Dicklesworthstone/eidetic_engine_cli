# TOON Output Format

TOON (Terse Object Output Notation) is a token-efficient output format for `ee` commands. It is a **renderer over canonical JSON**, not a storage or audit format.

## When to Use TOON

- LLM context windows where token efficiency matters
- Human scanning of structured output
- Quick visual inspection of nested data

## When NOT to Use TOON

- Storage, audit, or persistence (use JSON)
- Hook consumption (use JSON or hook format)
- MCP protocol messages (use JSON)
- Test assertions (use JSON golden fixtures)
- Export/import operations (use JSON or JSONL)

## Format Specification

TOON transforms JSON into a whitespace-delimited hierarchical format:

### Basic Syntax

```
key: value
nested:
  child_key: child_value
  another: 42
array[3]:
  - item1
  - item2
  - item3
```

### Key-Value Pairs

JSON `{"key": "value"}` becomes:
```
key: value
```

### Nested Objects

JSON `{"outer": {"inner": "value"}}` becomes:
```
outer:
  inner: value
```

### Arrays

JSON `{"items": ["a", "b", "c"]}` becomes:
```
items[3]:
  - a
  - b
  - c
```

### Arrays of Objects with Field Selection

Large arrays of objects use a compact header notation:
```
results[10]{id,score,content}:
  mem_001,0.95,First result
  mem_002,0.91,Second result
```

### Null Values

JSON `{"value": null}` becomes:
```
value: null
```

### Boolean Values

JSON `{"enabled": true, "disabled": false}` becomes:
```
enabled: true
disabled: false
```

## Response Envelope

All `ee` TOON output follows the standard response envelope:

```
schema: ee.response.v1
success: true
data:
  command: status
  version: 0.1.0
  ...
```

Error responses use the error schema:
```
schema: ee.error.v1
error:
  code: storage_unavailable
  message: Database not initialized
  repair: ee init --workspace .
```

## Token Efficiency

TOON achieves token savings by:

1. **No quoted keys**: `"key": "value"` → `key: value`
2. **No braces**: No `{` `}` around objects
3. **No brackets**: Array notation `[3]` in headers only
4. **Indentation**: 2-space indentation replaces nesting syntax
5. **Compact arrays**: Header notation for homogeneous arrays

Typical savings: 20-40% fewer tokens compared to JSON.

## CLI Usage

```bash
# Explicit format flag
ee status --format toon

# All commands support --format toon
ee doctor --format toon
ee health --format toon
ee capabilities --format toon
ee why mem_001 --format toon
ee search "query" --format toon
ee context "task" --format toon
```

## Comparison with JSON

| Aspect | JSON | TOON |
|--------|------|------|
| Machine parsing | Native | Requires decoder |
| Token efficiency | Baseline | 20-40% savings |
| Storage/audit | Yes | No |
| Hook output | Yes | No |
| MCP protocol | Yes | No |
| Human readable | Medium | High |
| Schema validation | Direct | Via JSON roundtrip |

## Decoding TOON

TOON can be decoded back to equivalent JSON for validation:

```rust
use toon::try_decode;

let toon_output = "schema: ee.response.v1\nsuccess: true";
let json = try_decode(toon_output, None)?;
```

## Determinism

TOON output is deterministic:
- Same input JSON always produces same TOON
- Field ordering matches JSON source order
- Array elements preserve order
- No random or timestamp-based variation

## Error Handling

If TOON encoding fails (invalid JSON input), the output is:

```
schema: ee.error.v1
error:
  code: toon_encoding_failed
  message: "TOON encoding failed: <error details>"
```

## Best Practices

1. **Use JSON for programmatic consumption** - TOON is for display
2. **Don't parse TOON in scripts** - Use `--json` instead
3. **Use TOON for LLM context** - Token savings matter
4. **Golden tests use JSON** - TOON is a rendering layer
5. **Hooks receive JSON** - Never configure TOON for hooks

## See Also

- `ee agent-docs formats` - All output format options
- `ee agent-docs contracts` - JSON schema contracts
- `ee --format json` - Canonical JSON output
