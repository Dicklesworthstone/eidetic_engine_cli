use std::collections::HashMap;
use std::time::{Instant, SystemTime};

use rand::Rng;
use uuid::Uuid;

#[determinism::required]
fn missing_seed_token() {}

#[determinism::required]
fn ambient_rng(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let mut rng = rand::thread_rng();
    let _: u64 = rng.r#gen();
}

#[determinism::required]
fn ambient_random(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _: u64 = rand::random();
}

#[determinism::required]
fn ambient_uuid_v4(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = Uuid::new_v4();
}

#[determinism::required]
fn ambient_time(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = Instant::now();
}

#[determinism::required]
fn ambient_wall_clock(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = SystemTime::now();
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

fn benign_documentation_mentions() {
    let _ = "rand::random::<u64>() Instant::now() SystemTime::now() std::fs::read_dir(.)";
    // rand::thread_rng();
    // std::env::var("EE_SEED");
}
