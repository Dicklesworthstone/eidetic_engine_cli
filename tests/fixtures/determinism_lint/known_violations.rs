use std::collections::HashMap;
use std::fs;
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
fn ambient_uuid_v7_now(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = Uuid::now_v7();
    let _ = uuid::Uuid::now_v7();
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
    let _ = std::env::var_os("EE_SEED");
    let _ = std::env::vars();
}

#[determinism::required]
fn hashmap_iteration(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let map: HashMap<String, String> = HashMap::new();
    for _ in map.iter() {}
    for _ in map.keys() {}
    for _ in map.values() {}
    for _ in map.into_iter() {}
}

#[determinism::required]
fn read_dir_order(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = std::fs::read_dir(".");
    let _ = fs::read_dir(".");
}

#[determinism::required]
fn ambient_chrono_clock(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = chrono::Utc::now();
    let _ = chrono::Local::now();
}

fn benign_documentation_mentions() {
    let _ = "rand::random::<u64>() Instant::now() SystemTime::now() chrono::Utc::now() std::fs::read_dir(.)";
    // rand::thread_rng();
    // chrono::Local::now();
    // std::env::var("EE_SEED");
    // std::env::var_os("EE_SEED");
    // std::env::vars();
    // fs::read_dir(".");
}

fn benign_block_comment_and_raw_string_mentions() {
    /*
     * rand::thread_rng();
     * Uuid::new_v4();
     * Uuid::now_v7();
     * chrono::Utc::now();
     * std::fs::read_dir(".");
     */
    let _ = r#"std::env::var("EE_SEED") Instant::now() SystemTime::now() chrono::Local::now()"#;
}
