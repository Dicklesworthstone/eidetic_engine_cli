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
    "    /// Team scope requires the workspace database, not an alternate store.\n"
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
corpus = root / "src/core/ask_corpus.rs"
corpus_text = replace_once(
    corpus.read_text(),
    "    if scope == MemoryScope::Team {\n",
    "    if scope == MemoryScope::Team {\n"
    "        admission::require_workspace_roster(connection, workspace_id)?;\n",
)
scopes = root / "src/core/ask_scope_tests.rs"
scopes_text = replace_once(
    scopes.read_text(),
    "    let root = tempfile::tempdir().unwrap();\n",
    "    let root = tempfile::tempdir().unwrap();\n"
    "    std::fs::create_dir(root.path().join(\".ee\")).unwrap();\n",
)
if scopes_text.count('.join("ask.db")') != 3:
    raise SystemExit("Ask scope fixture changed; refusing an incomplete database-path edit")
scopes_text = scopes_text.replace('.join("ask.db")', '.join(".ee/ee.db")')
scopes_text += '''

#[test]
fn team_scope_rejects_an_alternate_store_roster_without_falling_back() {
    let (root, _local, workspace) = fixture();
    let other = DbConnection::open_file(&root.path().join("alternate.db")).unwrap();
    other.migrate().unwrap();
    other.insert_workspace(&workspace, &CreateWorkspaceInput {
        path: root.path().to_string_lossy().into_owned(), name: None,
    }).unwrap();
    seed(&other, &workspace, 1, "Mallory", "human_explicit", &[]);
    member(&other, &workspace, "mbr_00000000000000000000000000000004", "Mallory", "active");
    let error = load_scoped_ask_corpus(&other, &workspace, Utc::now(), MemoryScope::Team).unwrap_err();
    assert!(matches!(error, DomainError::PolicyDenied { .. }));
    assert!(!error.message().contains("Mallory"));
    assert!(!error.message().contains(&root.path().to_string_lossy().to_string()));
    // The database is valid and usable; only its claim to team authority is refused.
    assert_eq!(load_current_ask_corpus(&other, &workspace, Utc::now()).unwrap().candidates.len(), 1);
}

#[test]
fn unavailable_team_roster_fails_closed_and_releases_the_snapshot() {
    let (_root, db, workspace) = fixture();
    seed(&db, &workspace, 1, "Bob", "human_explicit", &[]);
    db.execute_raw("ALTER TABLE team_members RENAME TO unavailable_roster").unwrap();
    let error = load_scoped_ask_corpus(&db, &workspace, Utc::now(), MemoryScope::Team).unwrap_err();
    assert!(!error.message().contains("unavailable_roster"));
    db.begin_read_snapshot().unwrap();
    db.commit_read_snapshot().unwrap();
    assert_eq!(load_current_ask_corpus(&db, &workspace, Utc::now()).unwrap().candidates.len(), 1);
}
'''
# Validate every replacement before changing any source file.
for path, text in [(core, core_text), (cli, cli_text), (contracts, contracts_text),
                   (corpus, corpus_text), (scopes, scopes_text)]:
    path.write_text(text)
