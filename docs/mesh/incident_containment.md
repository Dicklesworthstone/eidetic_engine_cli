# Mesh Incident Containment Runbook

Use this runbook when mesh peers, workspace scope, policy decisions, or export
behavior look wrong. Containment must preserve local memories, cache rows, and
audit evidence; it is not a cleanup or deletion workflow.

## Immediate Steps

1. Inspect current posture:

   ```bash
   ee mesh status --workspace . --json
   ```

2. Preview the narrowest disable:

   ```bash
   ee mesh disable --workspace . --dry-run --reason "unexpected peer" --json
   ```

3. Apply workspace containment when the preview is correct:

   ```bash
   ee mesh disable --workspace . --reason "unexpected peer" --json
   ```

4. For a single peer, suspend that peer instead of widening the blast radius:

   ```bash
   ee mesh disable --workspace . --peer peer_alpha --temporary-for 30m --reason "unexpected body lane" --json
   ```

5. Re-enable only with an explicit command after review:

   ```bash
   ee mesh reenable --workspace . --confirm-reenable --json
   ```

## Containment Guarantees

- Listener and background sync posture is stopped or reported as already
  stopped.
- Queued exports are cancelled or reported as zero when no queue exists.
- New peer requests are rejected while containment is active.
- Peer body, embedding, graphLink, and revisionNotice capabilities are
  suspended for peer-specific containment.
- Local cache and source-of-truth memories remain readable.
- No files, memories, cache rows, or audit rows are deleted by containment.
- All-workspaces containment is reported as an operator-global action; it does
  not silently rewrite every workspace config from one command.

## Verification

The e2e smoke is:

```bash
scripts/e2e_mesh_emergency_disable.sh
```

The log emits `ee.test_event.v1` JSON lines for:

- `mesh_enabled_before`
- `disable_requested`
- `listener_stopped`
- `queued_exports_cancelled`
- `local_search_still_works`
- `reenable_requires_explicit_command`
