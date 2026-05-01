#![forbid(unsafe_code)]
#![doc = "Library surface for the `ee` command-line memory substrate."]

pub mod cache;
pub mod cass;
pub mod cli;
pub mod config;
pub mod core;
pub mod curate;
pub mod db;
pub mod eval;
pub mod graph;
pub mod hooks;
pub mod models;
pub mod obs;
pub mod output;
pub mod pack;
pub mod policy;
pub mod search;
pub mod shadow;
pub mod steward;

#[cfg(feature = "mcp")]
pub mod mcp;

#[cfg(feature = "serve")]
pub mod serve;

pub mod science;

#[cfg(test)]
pub mod fuzz;
#[cfg(test)]
pub mod testing;
