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

    for (line_index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }

        let cells = trimmed
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.len() != 6 {
            return Err(format!(
                "docs/env_vars.md:{} expected 6 table cells, got {}",
                line_index + 1,
                cells.len()
            ));
        }

        let raw_name = cells[0];
        if !raw_name.starts_with("`EE_") {
            continue;
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
