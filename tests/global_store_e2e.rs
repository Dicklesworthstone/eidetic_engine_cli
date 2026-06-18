//! E2E: the user-global memory tier (bd-1pq3c, ADR 0083) is a REAL separate
//! on-disk store, not policy-only.
//!
//! Proves that `open_or_create_global_store` + `read_global_store_memories`
//! persist a memory and read it back across **independent** connections
//! (simulating separate `ee` process invocations against
//! `~/.local/share/ee/global`), and that two different store roots are
//! isolated — the core contract a `remember --global` write / global-tier read
//! depends on.

use ee::core::global_store::{
    global_workspace_id, open_or_create_global_store, read_global_store_memories, GlobalStorePaths,
};
use ee::db::CreateMemoryInput;

fn global_memory_input(workspace_id: &str, content: &str) -> CreateMemoryInput {
    CreateMemoryInput {
        workspace_id: workspace_id.to_owned(),
        level: "semantic".to_owned(),
        kind: "rule".to_owned(),
        content: content.to_owned(),
        workflow_id: None,
        confidence: 0.95,
        utility: 0.0,
        importance: 0.0,
        provenance_uri: None,
        trust_class: "self".to_owned(),
        trust_subclass: None,
        tags: vec!["global".to_owned()],
        valid_from: None,
        valid_to: None,
    }
}

#[test]
fn global_store_persists_across_independent_opens_and_isolates_roots() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = GlobalStorePaths::from_root(&tempdir.path().join("global"));

    // A read before any write returns empty (the store does not exist yet),
    // so a global-tier read needs no separate pre-existence check.
    assert!(read_global_store_memories(&paths, false)
        .expect("read empty global store")
        .is_empty());

    // Invocation 1: create the separate store and write a user-global memory.
    {
        let (connection, workspace_id) =
            open_or_create_global_store(&paths).expect("create global store");
        assert!(paths.database_path.exists(), "global ee.db materialized");
        assert_eq!(workspace_id, global_workspace_id(&paths));
        connection
            .insert_memory(
                "mem_e2e_global0000000000000000001",
                &global_memory_input(&workspace_id, "prefer X over Y across all repos"),
            )
            .expect("insert global memory");
    }

    // Invocation 2: a fresh open of the same on-disk store reads it back.
    let memories = read_global_store_memories(&paths, false).expect("read global store");
    assert_eq!(memories.len(), 1, "global memory persisted across opens");
    assert_eq!(memories[0].content, "prefer X over Y across all repos");

    // Re-opening is idempotent: the stable global workspace id is unchanged.
    let (_again, workspace_id_again) =
        open_or_create_global_store(&paths).expect("reopen global store");
    assert_eq!(workspace_id_again, global_workspace_id(&paths));

    // Separate-store isolation: a different root is its own empty store.
    let other = GlobalStorePaths::from_root(&tempdir.path().join("other-global"));
    assert!(
        read_global_store_memories(&other, false)
            .expect("read isolated store")
            .is_empty(),
        "separate global store roots are isolated"
    );
}
