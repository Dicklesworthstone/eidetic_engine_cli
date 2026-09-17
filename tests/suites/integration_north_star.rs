//! The north-star context end-to-end walkthrough.
//!
//! Split out of `integration_n_r` (bd-in3xj), for the same reason and by the
//! same method as `integration_property`: the shards are split by FILENAME, and
//! cost does not follow a name.
//!
//! Measured at d02b372c7 with `-- -Z unstable-options --report-time`, on a run
//! that completed 613/35/1 of 649 announced:
//!
//! ```text
//!   north_star_context_e2e   7 tests   11,482.80s summed   54.1% of ALL test time
//!   its longest test         2042.41s  ==  2042.41s, the ENTIRE test phase
//! ```
//!
//! That equality is the point. These tests run concurrently, so the shard's wall
//! clock was one test's duration and the other 642 finished inside its shadow.
//! With the module here, `integration_n_r`'s wall clock is set by its next
//! heaviest test at roughly 635s instead.
//!
//! Why that mattered: RCH charges COMPILE time against the same cap as the run,
//! and a cold compile for this tree swings 598s-995s. Across four runs of
//! `integration_n_r` at one base, outcome was a clean function of compile time
//! -- 598s and 647s completed, 952s and 995s were killed at 3600s, no overlap.
//! A ~2042s wall clock left less margin than the build's own variance.
//!
//! Nothing is weakened and nothing is skipped: all seven tests still run, with
//! their assertions unchanged, in a target whose budget can be set for what they
//! actually are -- a handful of very long real-binary `ee` walkthroughs rather
//! than ordinary integration tests. Serialising them was considered and
//! rejected: they already run concurrently, so a spawn gate would turn a 2042s
//! wall into something near their 11,483s sum.
//!
//! The swarm-required `cargo test --workspace north_star` filter still reaches
//! these tests; it selects by test name across targets, not by target.
//!
//! Filter with `cargo test --test integration_north_star <module>::`.

#[path = "../north_star_context_e2e.rs"]
mod north_star_context_e2e;
