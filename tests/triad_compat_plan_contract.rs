use clap::CommandFactory;

#[test]
fn triad_compat_plan_mentions_every_top_level_command() {
    let plan = include_str!("../docs/triad_compat_plan.md");
    let command = ee::cli::Cli::command();
    let missing = command
        .get_subcommands()
        .map(clap::Command::get_name)
        .filter(|name| !plan.contains(&format!("`ee {name}")))
        .collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "docs/triad_compat_plan.md is missing top-level commands: {}",
        missing.join(", ")
    );
}
