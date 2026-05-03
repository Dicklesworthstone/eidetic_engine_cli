# ADR 0001: CLI-First Memory Substrate

Status: accepted
Date: 2026-04-29

## Context

The original Eidetic Engine was service-first and agent-loop-oriented. Modern
agent harnesses already own planning, tool execution, approvals, recovery, and
conversation state. `ee` needs to provide durable memory without becoming a
replacement harness or requiring a background service for normal use.

## Decision

`ee` is a local CLI-first memory substrate. The command line is the primary
product and compatibility contract. Daemon, MCP, HTTP, hook, and library
surfaces are optional adapters over the same core services and must not be
required for normal workflows.

## Consequences

Agents and humans can call `ee` from any shell. JSON output remains scriptable
and stable. The product pressure stays on fast one-shot workflows such as
`ee context`, `ee search`, and `ee remember`.

Daemon and MCP work must wait until the core CLI is useful. Features that make
`ee` an agent loop, planner, chat shell, tool router, or central web service are
out of scope.

## Rejected Alternatives

- FastAPI or web service as the primary product.
- MCP as the only or central interface.
- Always-on memory steward required for core commands.
- Agent control loop that directs tool use or task planning.

## Verification

- Walking-skeleton tests run `ee init`, `ee remember`, `ee search`,
  `ee context`, `ee why`, and `ee status` without a daemon.
- Machine-facing commands assert stdout-only data and stderr-only diagnostics.
- MCP and future server tests prove adapter parity with CLI schemas instead of
  defining independent behavior.
