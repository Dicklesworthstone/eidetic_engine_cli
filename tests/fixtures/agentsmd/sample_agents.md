# Sample AGENTS.md (bridge parser fixture)

This fixture exercises the ADR 0065 §5 import parser: precision over
recall. Prose sentences like this one carry no modality and are skipped.

## Build rules

- Always run the verify script before pushing changes to main.
- The release pipeline MUST wait for the smoke suite before tagging.
- You MUST NOT regenerate goldens on a Mac-local checkout.
- Prefer structured logging over print-style debugging in handlers.
- short MUST line
- plain step with no modality cue that should never be extracted here

1. Do not commit generated artifacts into the source tree, ever.
2. ship the feature when the tests pass

## Traps the parser must skip

# ALWAYS ignore headings even with hard modality words in them.

| Table rows MUST NOT be extracted no matter what they claim |
|---|

> Blockquotes ALWAYS get skipped by the precision-first parser.

<!-- HTML comments MUST NOT be extracted either, obviously. -->

```bash
echo "code fences NEVER produce rule statements, full stop"
```

The deploy job MUST drain in-flight requests before restarting workers.
This paragraph line has always been lowercase prose and is skipped.
