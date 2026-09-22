#![forbid(unsafe_code)]

use ee::config::EnvVar;

type TestResult<T = ()> = Result<T, String>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct DocEnvVar {
    name: String,
    category: String,
    default: Option<String>,
    controls: String,
}

fn parse_doc_env_vars() -> TestResult<Vec<DocEnvVar>> {
    let content = include_str!("../docs/env_vars.md");
    let mut entries = Vec::new();

    // docs/env_vars.md documents build-time `option_env!`/`env!` names in a
    // separate four-column table under "## Build-time variables". Those are not
    // runtime registry entries -- `EnvVar::all()` contains none of them -- and
    // they ARE `EE_`-prefixed, so the `EE_` filter below cannot exclude them
    // the way it excludes the third-party table. The six-column shape belongs
    // to the registry table only, so rows have to be attributed to a table
    // before it is applied. Any heading resets the flag, so an unknown section
    // fails loudly on the six-cell rule rather than being silently skipped.
    let mut in_build_time_table = false;

    for (line_index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim();
            in_build_time_table = heading.eq_ignore_ascii_case("Build-time variables");
            continue;
        }
        if in_build_time_table {
            continue;
        }
        if !trimmed.starts_with('|') {
            continue;
        }

        let cells = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        // docs/env_vars.md carries TWO tables. The registry table (:44) is
        // `| Name | Category | Type | Default | Controls | Notes |`; a second,
        // narrower one (:19) documents the third-party vars `EMBEDDING_MODEL`
        // and `OPENAI_API_KEY`, whose surrounding prose says their "values are
        // never displayed, never passed to Frankensearch, and never used for
        // retrieval". They are not `EE_*` and are not in the registry.
        //
        // The six-cell shape is a requirement of the REGISTRY table, so the
        // `EE_` filter -- which this loop already applied two lines later --
        // has to run first. Applied after, it made a hard error out of a table
        // this test was never meant to read.
        //
        // This cannot go vacuous: `docs_env_vars_table_matches_registry`
        // compares the parsed rows against the whole of `EnvVar::all()`, so a
        // registry row skipped here fails that equality loudly.
        let raw_name = match cells.first() {
            Some(name) if name.starts_with("`EE_") => *name,
            _ => continue,
        };

        if cells.len() != 6 {
            return Err(format!(
                "docs/env_vars.md:{} expected 6 table cells, got {}",
                line_index + 1,
                cells.len()
            ));
        }

        let name = raw_name.trim_matches('`').to_owned();
        let default = match cells[3] {
            "none" => None,
            value => Some(value.trim_matches('`').to_owned()),
        };

        entries.push(DocEnvVar {
            name,
            category: cells[1].to_owned(),
            default,
            controls: cells[4].to_owned(),
        });
    }

    Ok(entries)
}

#[test]
fn docs_env_vars_table_matches_registry() -> TestResult {
    let documented = parse_doc_env_vars()?;
    let expected = EnvVar::all()
        .iter()
        .map(|var| DocEnvVar {
            name: var.name().to_owned(),
            category: var.category().to_owned(),
            default: var.default_value().map(str::to_owned),
            controls: var.description().to_owned(),
        })
        .collect::<Vec<_>>();

    if documented == expected {
        Ok(())
    } else {
        Err(format!(
            "docs/env_vars.md drifted from EnvVar::all()\nexpected: {expected:#?}\nactual:   {documented:#?}"
        ))
    }
}

#[test]
fn docs_env_vars_mentions_capabilities_surface() -> TestResult {
    let content = include_str!("../docs/env_vars.md");
    if content.contains("data.envOverrides[]") {
        Ok(())
    } else {
        Err("docs/env_vars.md must mention the capabilities envOverrides surface".to_owned())
    }
}
