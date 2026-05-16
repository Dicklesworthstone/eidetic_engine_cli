use std::collections::HashMap;
use std::time::Instant;

use rand::Rng;

#[determinism::required]
fn missing_seed_token() {}

#[determinism::required]
fn ambient_rng(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let mut rng = rand::thread_rng();
    let _: u64 = rng.r#gen();
}

#[determinism::required]
fn ambient_time(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = Instant::now();
}

#[determinism::required]
fn ambient_env(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = std::env::var("EE_SEED");
}

#[determinism::required]
fn hashmap_iteration(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let map: HashMap<String, String> = HashMap::new();
    for _ in map.iter() {}
}

#[determinism::required]
fn read_dir_order(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = std::fs::read_dir(".");
}
