//! Scope-preserving, genuinely read-only MCP access to the shared ask CLI.
//!
//! Constraints must not disappear at the transport boundary. Reject unknown
//! fields and conflicting aliases instead of silently answering a wider query.
//! Path applicability, workspace authorization, evidence admission, semantic
//! scoring and citation integrity remain the responsibility of the shared CLI.

use std::ffi::OsString;

use serde_json::{Value, json};

const ALIASES: &[(&str, &str)] = &[
    ("limitEvidence", "limit_evidence"),
    ("minConfidence", "min_confidence"),
    ("requireConfidence", "require_confidence"),
    ("memoryScope", "memory_scope"),
    ("readOnly", "read_only"),
    ("path", "paths"),
];
const FIELDS: &[&str] = &[
    "question", "workspace", "database", "limitEvidence", "minConfidence",
    "requireConfidence", "memoryScope", "readOnly", "path",
];
const SCOPES: &[&str] = &["self", "team", "verified", "global", "workspace", "swarm"];

pub(super) fn schema() -> Value {
    let mut schema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "question": {
                "type": "string", "minLength": 1,
                "description": "Question answered from exact memory, rule or admitted transcript spans"
            },
            "workspace": { "type": "string", "minLength": 1, "description": "Workspace path" },
            "database": { "type": "string", "minLength": 1, "description": "Database path override" },
            "limitEvidence": { "type": "integer", "minimum": 1, "maximum": u32::MAX },
            "minConfidence": { "type": "number", "minimum": 0, "maximum": 1 },
            "requireConfidence": { "type": "number", "minimum": 0, "maximum": 1 },
            "memoryScope": {
                "type": "string", "enum": SCOPES, "default": "workspace",
                "description": "Select authority within this workspace; never expands the workspace boundary"
            },
            "path": {
                "oneOf": [
                    { "type": "string", "minLength": 1 },
                    { "type": "array", "items": { "type": "string", "minLength": 1 } }
                ],
                "description": "Literal workspace-relative task targets for directory/file rules, not globs or file contents"
            },
            "readOnly": {
                "type": "boolean", "const": true, "default": true,
                "description": "Always read-only: no retrieval audits, query-miss writes or migrations"
            }
        },
        "required": ["question"]
    });
    for &(canonical, alias) in ALIASES {
        let mut property = schema["properties"][canonical].clone();
        property["description"] = format!("Alias for {canonical}; do not supply both spellings").into();
        schema["properties"][alias] = property;
    }
    schema
}

fn field<'a>(arguments: &'a Value, canonical: &str) -> Option<&'a Value> {
    arguments.get(canonical).or_else(|| {
        ALIASES.iter().find_map(|&(name, alias)| {
            (name == canonical).then(|| arguments.get(alias)).flatten()
        })
    })
}

fn string<'a>(value: &'a Value, name: &str) -> Result<&'a str, String> {
    value.as_str().filter(|text| !text.trim().is_empty())
        .ok_or_else(|| format!("Argument '{name}' must be a non-empty string"))
}

/// Stage all arguments first: an invalid constraint cannot leave a partial
/// invocation in the caller's buffer. There is no shell interpolation.
pub(super) fn build_args(args: &mut Vec<OsString>, arguments: &Value) -> Result<(), String> {
    let object = arguments.as_object().ok_or("ee_ask arguments must be an object")?;
    if object.keys().any(|key| {
        !FIELDS.contains(&key.as_str()) && !ALIASES.iter().any(|&(_, alias)| alias == key)
    }) {
        return Err("Unknown ee_ask argument; use the advertised tool schema".to_owned());
    }
    for &(canonical, alias) in ALIASES {
        if object.contains_key(canonical) && object.contains_key(alias) {
            return Err(format!("Conflicting aliases for '{canonical}'"));
        }
    }
    let question = string(field(arguments, "question").ok_or("Missing required argument 'question'")?, "question")?;
    if let Some(read_only) = field(arguments, "readOnly") {
        if read_only.as_bool() != Some(true) {
            return Err("ee_ask is read-only; readOnly must be true".to_owned());
        }
    }
    // Workspace selection is supplied by the MCP owner before the command.
    // Validate it here as well, but do not introduce a second workspace flag.
    if let Some(workspace) = field(arguments, "workspace") {
        string(workspace, "workspace")?;
    }
    let mut staged = vec![OsString::from("ask"), OsString::from("--read-only")];
    if let Some(scope) = field(arguments, "memoryScope") {
        let scope = string(scope, "memoryScope")?;
        if !SCOPES.contains(&scope) {
            return Err("Argument 'memoryScope' must name a supported memory scope".to_owned());
        }
        staged.push(format!("--memory-scope={scope}").into());
    }
    if let Some(paths) = field(arguments, "path") {
        match paths {
            Value::String(_) => staged.push(format!("--path={}", string(paths, "path")?).into()),
            Value::Array(paths) => {
                for path in paths {
                    staged.push(format!("--path={}", string(path, "path")?).into());
                }
            }
            _ => return Err("Argument 'path' must be a string or an array of strings".to_owned()),
        }
    }
    if let Some(limit) = field(arguments, "limitEvidence") {
        let limit = limit.as_u64().and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or("Argument 'limitEvidence' must be an integer from 1 to 4294967295")?;
        staged.push(format!("--limit-evidence={limit}").into());
    }
    for (name, flag) in [("minConfidence", "--min-confidence"), ("requireConfidence", "--require-confidence")] {
        if let Some(value) = field(arguments, name) {
            let number = value.as_f64().filter(|number| number.is_finite() && (0.0..=1.0).contains(number))
                .ok_or_else(|| format!("Argument '{name}' must be a number from 0 to 1"))?;
            staged.push(format!("{flag}={number}").into());
        }
    }
    if let Some(database) = field(arguments, "database") {
        staged.push(format!("--database={}", string(database, "database")?).into());
    }
    // A question beginning with '-' remains text, never an option or another
    // workspace selection. Equals-form options similarly preserve path bytes.
    staged.push("--".into());
    staged.push(question.into());
    args.extend(staged);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn arguments(value: Value) -> Vec<String> {
        let mut args = Vec::new();
        build_args(&mut args, &value).unwrap();
        args.into_iter().map(|value| value.into_string().unwrap()).collect()
    }

    fn rejects(value: Value) -> String {
        let mut args = vec![OsString::from("ee")];
        let before = args.clone();
        let error = build_args(&mut args, &value).unwrap_err();
        assert_eq!(args, before, "invalid arguments must not leave a partial command");
        error
    }

    #[test]
    fn default_ask_is_genuinely_read_only() {
        assert_eq!(arguments(json!({"question": "Which port?"})),
            ["ask", "--read-only", "--", "Which port?"]);
    }

    #[test]
    fn all_memory_scopes_reach_the_shared_cli() {
        for scope in SCOPES {
            let args = arguments(json!({"question": "Which port?", "memoryScope": scope}));
            assert!(args.contains(&format!("--memory-scope={scope}")));
        }
    }

    #[test]
    fn paths_preserve_literal_commas_spaces_unicode_and_option_prefixes() {
        let args = arguments(json!({"question": "Which checks?", "path": ["src/a,b.rs", "src/café notes.rs", "--private.rs"]}));
        for path in ["src/a,b.rs", "src/café notes.rs", "--private.rs"] {
            assert!(args.contains(&format!("--path={path}")));
        }
        assert_eq!(args.iter().filter(|arg| arg.starts_with("--path=")).count(), 3);
    }

    #[test]
    fn string_and_single_element_array_paths_have_identical_meaning() {
        assert_eq!(arguments(json!({"question": "Q", "path": "src/a,b.rs"})),
            arguments(json!({"question": "Q", "paths": ["src/a,b.rs"]})));
    }

    #[test]
    fn constraint_aliases_preserve_exact_invocation() {
        let canonical = json!({"question":"Q", "limitEvidence":2, "minConfidence":0.5,
            "requireConfidence":0.6, "memoryScope":"verified", "readOnly":true});
        let aliases = json!({"question":"Q", "limit_evidence":2, "min_confidence":0.5,
            "require_confidence":0.6, "memory_scope":"verified", "read_only":true});
        assert_eq!(arguments(canonical), arguments(aliases));
    }

    #[test]
    fn conflicting_aliases_are_never_silently_prioritized() {
        for &(canonical, alias) in ALIASES {
            let mut value = json!({"question":"Q"});
            value[canonical] = "private-canary".into();
            value[alias] = Value::Null;
            let error = rejects(value);
            assert!(error.contains("Conflicting aliases"));
            assert!(!error.contains("private-canary"));
        }
    }

    #[test]
    fn unknown_constraints_are_rejected_without_echoing_private_keys() {
        let error = rejects(json!({"question":"Q", "private-canary-typo":"team"}));
        assert!(error.contains("Unknown ee_ask argument"));
        assert!(!error.contains("private-canary"));
    }

    #[test]
    fn malformed_paths_do_not_become_unscoped_queries() {
        for paths in [Value::Null, json!(false), json!(42), json!({}), json!(""), json!(["src/lib.rs", 3]), json!([" "])] {
            rejects(json!({"question":"Q", "paths":paths}));
        }
    }

    #[test]
    fn invalid_scopes_fail_closed_without_disclosure() {
        for scope in [json!("private-canary"), json!(""), json!(["team"]), Value::Null] {
            assert!(!rejects(json!({"question":"Q", "memoryScope":scope})).contains("private-canary"));
        }
    }

    #[test]
    fn read_only_false_or_wrong_types_cannot_enable_audit_writes() {
        for value in [json!(false), json!("true"), json!(1), Value::Null] {
            rejects(json!({"question":"Q", "readOnly":value}));
        }
    }

    #[test]
    fn numeric_constraints_are_checked_before_dispatch() {
        for key in ["minConfidence", "requireConfidence"] {
            for number in [json!(-0.1), json!(1.1), json!("0.5"), Value::Null] {
                let mut value = json!({"question":"Q"});
                value[key] = number;
                rejects(value);
            }
        }
        for limit in [json!(0), json!(-1), json!(1.5), json!(u64::MAX)] {
            rejects(json!({"question":"Q", "limitEvidence":limit}));
        }
        let args = arguments(json!({"question":"Q", "minConfidence":0, "requireConfidence":1}));
        assert!(args.contains(&"--min-confidence=0".to_owned()));
        assert!(args.contains(&"--require-confidence=1".to_owned()));
    }

    #[test]
    fn option_like_questions_and_database_values_remain_single_arguments() {
        let question = "--workspace=/private-canary";
        let args = arguments(json!({"question":question, "database":"--foreign-store"}));
        assert_eq!(&args[args.len()-2..], &["--", question]);
        assert!(args.contains(&"--database=--foreign-store".to_owned()));
        let schema = schema();
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"]["readOnly"]["const"], true);
        for &(canonical, alias) in ALIASES {
            assert_eq!(schema["properties"][canonical]["type"], schema["properties"][alias]["type"]);
        }
    }
}
