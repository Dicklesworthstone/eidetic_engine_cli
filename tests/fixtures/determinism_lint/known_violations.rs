use std::collections::{HashMap, HashSet};
use std::time::{Instant, SystemTime};
use std::{env, fs};

use rand::{Rng, random};
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
    let _: u64 = random::<u64>();
}

#[determinism::required]
fn ambient_getrandom_fill(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).unwrap();
}

#[determinism::required]
fn ambient_ring_system_random(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = ring::rand::SystemRandom::new();
    let _ = SystemRandom::new();
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
    let _ = std::env::args();
    let _ = std::env::args_os();
    let _ = env::var("EE_ALIAS_SEED");
    let _ = env::var_os("EE_ALIAS_SEED");
    let _ = env::vars();
    let _ = env::vars_os();
    let _ = env::args();
    let _ = env::args_os();
}

#[determinism::required]
fn hashmap_iteration(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let mut map: HashMap<String, String> = HashMap::new();
    for _ in map.iter() {}
    for _ in map.keys() {}
    for _ in map.values() {}
    for _ in map.drain() {}
    for _ in map.into_iter() {}
}

#[determinism::required]
fn hashset_iteration(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let mut set: HashSet<String> = HashSet::new();
    for _ in set.iter() {}
    for _ in set.drain() {}
    for _ in set.into_iter() {}
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

#[determinism::required]
fn ambient_domain_id(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = ee::models::MemoryId::now();
    let _ = RuleId::now();
}

#[determinism::required]
fn ambient_process_and_thread(_: &ee::runtime::determinism::Deterministic<Seed>) {
    let _ = std::process::id();
    let _ = process::id();
    let _ = std::thread::current();
    let _ = thread::current();
}

fn benign_documentation_mentions() {
    let _ = "rand::random::<u64>() random::<u64>() getrandom::fill(&mut bytes) SystemRandom::new() Instant::now() SystemTime::now() chrono::Utc::now() MemoryId::now() std::fs::read_dir(.) HashSet";
    // rand::thread_rng();
    // getrandom::fill(&mut bytes);
    // ring::rand::SystemRandom::new();
    // SystemRandom::new();
    // chrono::Local::now();
    // RuleId::now();
    // std::env::var("EE_SEED");
    // std::env::var_os("EE_SEED");
    // std::env::vars();
    // std::env::args();
    // std::env::args_os();
    // env::var("EE_SEED");
    // env::var_os("EE_SEED");
    // env::vars();
    // env::vars_os();
    // env::args();
    // env::args_os();
    // fs::read_dir(".");
    // std::process::id();
    // process::id();
    // std::thread::current();
    // thread::current();
}

fn benign_block_comment_and_raw_string_mentions() {
    /*
     * rand::thread_rng();
     * random::<u64>();
     * getrandom::fill(&mut bytes);
     * ring::rand::SystemRandom::new();
     * SystemRandom::new();
     * Uuid::new_v4();
     * Uuid::now_v7();
     * chrono::Utc::now();
     * MemoryId::now();
     * std::fs::read_dir(".");
     * HashSet<String>::new().iter();
     * env::var("EE_SEED");
     * env::var_os("EE_SEED");
     * env::vars();
     * env::vars_os();
     * std::env::args();
     * std::env::args_os();
     * env::args();
     * env::args_os();
     */
    let _ = r#"std::env::var("EE_SEED") std::env::args() env::args_os() random::<u64>() SystemRandom::new() Instant::now() SystemTime::now() chrono::Local::now() RuleId::now()"#;
}
