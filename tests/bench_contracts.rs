//! Run the assertions embedded in benchmark sources through the normal test gate.
//!
//! Criterion entry points remain available to their harness-less bench targets.
//! Importing the sources here lets libtest collect their existing `#[test]`
//! functions without running Criterion or duplicating the assertions.

#![allow(
    dead_code,
    reason = "Criterion entry points and their helpers are not invoked by this libtest target"
)]

#[path = "../benches/agent_profile.rs"]
mod agent_profile;
#[path = "../benches/consolidator.rs"]
mod consolidator;
#[path = "../benches/context.rs"]
mod context;
#[path = "../benches/context_stream.rs"]
mod context_stream;
#[path = "../benches/context_with_ppr.rs"]
mod context_with_ppr;
#[path = "../benches/curate_candidates.rs"]
mod curate_candidates;
#[path = "../benches/graph_full_stack.rs"]
mod graph_full_stack;
#[path = "../benches/graph_minhash_rank.rs"]
mod graph_minhash_rank;
#[path = "../benches/graph_pagerank.rs"]
mod graph_pagerank;
#[path = "../benches/import_cass.rs"]
mod import_cass;
#[path = "../benches/link.rs"]
mod link;
#[path = "../benches/outcome.rs"]
mod outcome;
#[path = "../benches/pack_size.rs"]
mod pack_size;
#[path = "../benches/remember.rs"]
mod remember;
#[path = "../benches/search.rs"]
mod search;
#[path = "../benches/why.rs"]
mod why;
