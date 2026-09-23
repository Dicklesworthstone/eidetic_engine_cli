//! Property, metamorphic and fuzz modules.
//!
//! Split out of `integration_n_r` (bd-in3xj). Every `property_*.rs` file sorts
//! into the N-R range, so the by-filename shard split had put 98 of the suite's
//! 105 property functions -- 21,832 proptest cases, 98.8% of the whole suite --
//! behind one cap alongside 771 ordinary integration tests. n_r could not reach
//! its `test result:` line, so none of its tests could be graded at all.
//!
//! The shards are split by NAME; this is the one kind of test whose cost does
//! not follow its name. Keep proptest modules here.
//!
//! Filter with `cargo test --test integration_property <module>::`.

#[path = "../support/graph_generator.rs"]
mod graph_generator;
// Declared once here: every module below reaches it as `super::isolated_ee`.
// A second `#[path]` declaration of the same file is clippy::duplicate_mod.
#[path = "../support/isolated_ee.rs"]
mod isolated_ee;

#[path = "../property_context_query_metamorphic.rs"]
mod property_context_query_metamorphic;
#[path = "../property_eql_query_parsing.rs"]
mod property_eql_query_parsing;
#[path = "../property_graph_articulation_points.rs"]
mod property_graph_articulation_points;
#[path = "../property_graph_dominance_frontier.rs"]
mod property_graph_dominance_frontier;
#[path = "../property_graph_gomory_hu.rs"]
mod property_graph_gomory_hu;
#[path = "../property_graph_hits.rs"]
mod property_graph_hits;
#[path = "../property_graph_k_truss.rs"]
mod property_graph_k_truss;
#[path = "../property_graph_minhash_rank.rs"]
mod property_graph_minhash_rank;
#[path = "../property_graph_onion_layers.rs"]
mod property_graph_onion_layers;
#[path = "../property_graph_pagerank.rs"]
mod property_graph_pagerank;
#[path = "../property_graph_topological_order.rs"]
mod property_graph_topological_order;
#[path = "../property_graph_transitive_closure.rs"]
mod property_graph_transitive_closure;
#[path = "../property_mesh_frame.rs"]
mod property_mesh_frame;
#[path = "../property_origin_stream.rs"]
mod property_origin_stream;
#[path = "../property_output_governor.rs"]
mod property_output_governor;
#[path = "../property_pack_metamorphic.rs"]
mod property_pack_metamorphic;
#[path = "../property_pack_profile_variation_metamorphic.rs"]
mod property_pack_profile_variation_metamorphic;
#[path = "../property_plan_cache.rs"]
mod property_plan_cache;
#[path = "../property_profile_probe.rs"]
mod property_profile_probe;
#[path = "../property_query_and_pack.rs"]
mod property_query_and_pack;
#[path = "../property_read_pool.rs"]
mod property_read_pool;
#[path = "../property_redaction_idempotence.rs"]
mod property_redaction_idempotence;
#[path = "../property_remember_search_metamorphic.rs"]
mod property_remember_search_metamorphic;
#[path = "../property_response_envelope.rs"]
mod property_response_envelope;
#[path = "../property_shadow_tuning.rs"]
mod property_shadow_tuning;
#[path = "../property_simhash.rs"]
mod property_simhash;
#[path = "../radix_ulid_sort_proptest.rs"]
mod radix_ulid_sort_proptest;
#[path = "../redaction_fuzz.rs"]
mod redaction_fuzz;
