# Agent Detection Fixtures

Simulated agent installation roots for deterministic agent detection tests.

## Structure

Each subdirectory simulates a detected agent installation:

- `codex/sessions/` - Simulates Codex CLI installation
- `gemini/tmp/` - Simulates Gemini CLI installation  
- `claude/projects/` - Simulates Claude Code installation
- `cursor/.cursor/` - Simulates Cursor IDE installation
- `remote_mirror/ssh-csd/home/agent/.codex/sessions/` - Simulates a Codex root mirrored from `/home/agent/.codex/sessions` on host `csd`
- `remote_mirror/ssh-csd/home/agent/.claude/projects/` - Simulates a Claude root mirrored from `/home/agent/.claude/projects` on host `csd`

## Usage

Pass these as `root_overrides` to `detect_installed_agents()`:

```rust
use franken_agent_detection::{AgentDetectOptions, AgentDetectRootOverride};

let fixtures = Path::new("tests/fixtures/agent_detect");
let opts = AgentDetectOptions {
    only_connectors: Some(vec!["codex", "gemini"]),
    root_overrides: vec![
        AgentDetectRootOverride {
            slug: "codex".to_string(),
            root: fixtures.join("codex/sessions"),
        },
        AgentDetectRootOverride {
            slug: "gemini".to_string(),
            root: fixtures.join("gemini/tmp"),
        },
    ],
    ..Default::default()
};
```

This enables deterministic tests that don't depend on actual system state.

Remote mirror fixtures are intentionally plain directories. `ee` uses them to
publish deterministic origin metadata and path rewrite rules without enabling
connector-backed history readers in the default dependency profile.
