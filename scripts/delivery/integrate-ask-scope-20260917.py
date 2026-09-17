#!/usr/bin/env python3
"""Apply only the reviewed ask CLI bindings; reject drift rather than guessing."""
from pathlib import Path


def replace_once(text: str, old: str, new: str) -> str:
    if text.count(old) != 1:
        raise SystemExit("Ask integration source changed; refusing an ambiguous edit")
    return text.replace(old, new, 1)


root = Path(__file__).resolve().parents[2]
core = root / "src/core/ask.rs"
core_text = replace_once(
    core.read_text(),
    "pub use corpus::{AskCorpus, load_current_ask_corpus};",
    "pub use corpus::{AskCorpus, load_current_ask_corpus, load_scoped_ask_corpus};",
)
cli = root / "src/cli/mod.rs"
cli_text = cli.read_text()
start = cli_text.index("pub struct AskArgs {")
end = cli_text.index("/// Arguments for `ee diagnose-error`.", start)
args = cli_text[start:end]
args = replace_once(
    args,
    "    /// Database path. Defaults to <workspace>/.ee/ee.db.",
    "    /// Select self, team, verified, global-tagged, workspace, or swarm memories.\n"
    "    /// Scope never expands beyond the selected workspace.\n"
    "    #[arg(long, value_parser = parse_memory_scope_arg, default_value = \"workspace\")]\n"
    "    pub memory_scope: MemoryScope,\n\n"
    "    /// Database path. Defaults to <workspace>/.ee/ee.db.",
)
cli_text = cli_text[:start] + args + cli_text[end:]
cli_text = replace_once(
    cli_text,
    "    let corpus = match crate::core::ask::load_current_ask_corpus(\n"
    "        &connection,\n        &workspace_id,\n        chrono::Utc::now(),\n    ) {",
    "    let corpus = match crate::core::ask::load_scoped_ask_corpus(\n"
    "        &connection,\n        &workspace_id,\n        chrono::Utc::now(),\n"
    "        args.memory_scope,\n    ) {",
)
contracts = root / "tests/contracts/ask_lifecycle.rs"
contracts_text = contracts.read_text()
if 'mod scope;' in contracts_text or 'ask_scope.rs' in contracts_text:
    raise SystemExit("Ask scope contract is already registered; refusing duplicate edit")
contracts_text += '\n#[path = "ask_scope.rs"]\nmod scope;\n'
# Validate every replacement before changing any source file.
core.write_text(core_text)
cli.write_text(cli_text)
contracts.write_text(contracts_text)
