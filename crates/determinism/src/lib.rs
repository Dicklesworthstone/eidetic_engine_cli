//! Proc-macro enforcement for typed deterministic code paths.
//!
//! `#[determinism::required]` is intentionally dependency-free and conservative:
//! it checks the tagged function's token stream for a `Deterministic<Seed>`
//! parameter and rejects common ambient randomness, wall-clock, process, and
//! filesystem sources. The richer fixture scanner in
//! `tests/determinism_lint_catches_known_violations.rs` remains the detailed
//! contract for the lint's known violation catalog.

use proc_macro::{Delimiter, Group, TokenStream, TokenTree};

#[proc_macro_attribute]
pub fn required(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let compact = compact_non_literal_tokens(item.clone());
    let mut errors = Vec::new();

    if !has_deterministic_seed_parameter(item.clone()) {
        errors.push("#[determinism::required] requires a Deterministic<Seed> parameter");
    }

    for (needle, message) in AMBIENT_CALLS {
        if compact.contains(needle) {
            errors.push(message);
        }
    }

    if contains_domain_id_now(&compact) {
        errors.push("use seeded ID helpers instead of ambient typed Id::now");
    }

    if let Some(message) = errors.first() {
        return compile_error(message);
    }

    item
}

const AMBIENT_CALLS: &[(&str, &str)] = &[
    (
        "thread_rng(",
        "use Deterministic<Seed> instead of rand::thread_rng",
    ),
    (
        "rand::random(",
        "use Deterministic<Seed> instead of rand::random",
    ),
    (
        "rand::random<",
        "use Deterministic<Seed> instead of rand::random",
    ),
    (
        "random(",
        "use Deterministic<Seed> instead of imported rand::random",
    ),
    (
        "random<",
        "use Deterministic<Seed> instead of imported rand::random",
    ),
    (
        "getrandom::fill(",
        "use Deterministic<Seed> instead of direct OS entropy",
    ),
    (
        "ring::rand::SystemRandom::new(",
        "use Deterministic<Seed> instead of ring::rand::SystemRandom",
    ),
    (
        "SystemRandom::new(",
        "use Deterministic<Seed> instead of ring::rand::SystemRandom",
    ),
    (
        "Uuid::new_v4(",
        "use DeterministicClock/seeded ID helpers instead of Uuid::new_v4",
    ),
    (
        "uuid::Uuid::new_v4(",
        "use DeterministicClock/seeded ID helpers instead of Uuid::new_v4",
    ),
    (
        "Uuid::now_v7(",
        "use DeterministicClock/seeded ID helpers instead of Uuid::now_v7",
    ),
    (
        "uuid::Uuid::now_v7(",
        "use DeterministicClock/seeded ID helpers instead of Uuid::now_v7",
    ),
    (
        "Instant::now(",
        "inject timing at the boundary instead of calling Instant::now",
    ),
    (
        "SystemTime::now(",
        "inject wall-clock time at the boundary instead of calling SystemTime::now",
    ),
    (
        "Utc::now(",
        "inject UTC timestamps at the boundary instead of calling Utc::now",
    ),
    (
        "chrono::Utc::now(",
        "inject UTC timestamps at the boundary instead of calling Utc::now",
    ),
    (
        "Local::now(",
        "inject local timestamps at the boundary instead of calling Local::now",
    ),
    (
        "chrono::Local::now(",
        "inject local timestamps at the boundary instead of calling Local::now",
    ),
    (
        "std::env::var(",
        "read env through the registered config boundary",
    ),
    (
        "env::var(",
        "read env through the registered config boundary",
    ),
    (
        "std::env::var_os(",
        "read optional env through the registered config boundary",
    ),
    (
        "env::var_os(",
        "read optional env through the registered config boundary",
    ),
    (
        "std::env::vars(",
        "iterate env only through a deterministic registered boundary",
    ),
    (
        "env::vars(",
        "iterate env only through a deterministic registered boundary",
    ),
    (
        "std::env::vars_os(",
        "iterate env only through a deterministic registered boundary",
    ),
    (
        "env::vars_os(",
        "iterate env only through a deterministic registered boundary",
    ),
    (
        "std::env::args(",
        "read process args through the registered CLI boundary",
    ),
    (
        "env::args(",
        "read process args through the registered CLI boundary",
    ),
    (
        "std::env::args_os(",
        "read process args through the registered CLI boundary",
    ),
    (
        "env::args_os(",
        "read process args through the registered CLI boundary",
    ),
    (
        "std::env::current_dir(",
        "inject current directory/workspace at the boundary instead of calling env::current_dir",
    ),
    (
        "env::current_dir(",
        "inject current directory/workspace at the boundary instead of calling env::current_dir",
    ),
    (
        "std::env::temp_dir(",
        "inject temp directory at the boundary instead of calling env::temp_dir",
    ),
    (
        "env::temp_dir(",
        "inject temp directory at the boundary instead of calling env::temp_dir",
    ),
    (
        "std::fs::read_dir(",
        "sort read_dir entries before deterministic output",
    ),
    (
        "fs::read_dir(",
        "sort read_dir entries before deterministic output",
    ),
    (
        "std::process::id(",
        "inject the host PID at the boundary instead of calling std::process::id",
    ),
    (
        "process::id(",
        "inject the host PID at the boundary instead of calling std::process::id",
    ),
    (
        "std::thread::current(",
        "inject the thread identifier at the boundary instead of std::thread::current",
    ),
    (
        "thread::current(",
        "inject the thread identifier at the boundary instead of std::thread::current",
    ),
];

fn compact_non_literal_tokens(tokens: TokenStream) -> String {
    let mut output = String::new();
    append_non_literal_tokens(tokens, &mut output);
    output
}

fn has_deterministic_seed_parameter(tokens: TokenStream) -> bool {
    let mut saw_function_keyword = false;

    for token in tokens {
        match token {
            TokenTree::Ident(ident) if ident.to_string() == "fn" => {
                saw_function_keyword = true;
            }
            TokenTree::Group(group)
                if saw_function_keyword && group.delimiter() == Delimiter::Parenthesis =>
            {
                return compact_non_literal_tokens(group.stream()).contains("Deterministic<Seed>");
            }
            _ => {}
        }
    }

    false
}

fn append_non_literal_tokens(tokens: TokenStream, output: &mut String) {
    for token in tokens {
        match token {
            TokenTree::Group(group) => append_group(group, output),
            TokenTree::Ident(ident) => output.push_str(&ident.to_string()),
            TokenTree::Punct(punct) => output.push(punct.as_char()),
            TokenTree::Literal(_) => {}
        }
    }
}

fn append_group(group: Group, output: &mut String) {
    match group.delimiter() {
        Delimiter::Parenthesis => output.push('('),
        Delimiter::Brace => output.push('{'),
        Delimiter::Bracket => output.push('['),
        Delimiter::None => {}
    }
    append_non_literal_tokens(group.stream(), output);
    match group.delimiter() {
        Delimiter::Parenthesis => output.push(')'),
        Delimiter::Brace => output.push('}'),
        Delimiter::Bracket => output.push(']'),
        Delimiter::None => {}
    }
}

fn contains_domain_id_now(compact: &str) -> bool {
    let mut search_start = 0;
    while let Some(relative_index) = compact[search_start..].find("::now(") {
        let now_index = search_start + relative_index;
        let prefix = &compact[..now_index];
        let type_name = prefix
            .rsplit(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
            .next()
            .unwrap_or_default();
        if type_name.ends_with("Id") {
            return true;
        }
        search_start = now_index + "::now(".len();
    }
    false
}

fn compile_error(message: &str) -> TokenStream {
    format!("compile_error!({message:?});")
        .parse()
        .expect("compile_error token stream")
}
