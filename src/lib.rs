#![forbid(unsafe_code)]
// `core::index` / `core::search` build deeply nested async graphs (spawn_in ->
// process_index_jobs_coalesced_with_cx_bounded -> process_one_index_job_with_cx
// -> ...). Auto-trait (`Send`) resolution across those coroutine witnesses
// exceeds rustc's default depth of 128, which trips the future-compat
// `recursion_depth_exceeding_limit` lint and therefore fails
// `clippy -D warnings`. This raises a compiler resource bound; it does not
// silence the lint, and rustc itself recommends exactly this remedy.
#![recursion_limit = "256"]
#![cfg_attr(target_vendor = "apple", feature(peer_credentials_unix_socket))]
#![cfg_attr(windows, feature(windows_by_handle))]
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![doc = "Library surface for the `ee` command-line memory substrate."]

pub mod cache;
pub mod cass;
pub mod cli;
pub mod config;
pub mod core;
pub mod curate;
pub mod daemon;
pub mod db;
pub mod eval;
pub mod graph;
pub mod hooks;
pub mod mesh;
pub mod models;
pub mod obs;
pub mod output;
pub mod pack;
pub mod policy;
pub mod runtime;
pub mod search;
pub mod shadow;
pub mod steward;
pub mod util;

#[cfg(feature = "mcp")]
pub mod mcp;

pub mod serve;

pub mod science;

#[cfg(test)]
pub mod fuzz;
#[doc(hidden)]
pub mod testing;
