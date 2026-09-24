# Persistent candidate-pool defaults

`ee config set pack.candidate_pool 24` writes the workspace default to
`.ee/config.toml`. `ee config get pack.candidate_pool` reports the merged value
and its source. `ee config set pack.candidate_pool 24 --dry-run` previews the
write without creating or changing a file. An explicit `--config PATH` chooses
another configuration file, including `~/.config/ee/config.toml` for the user
layer.

For `ee pack`, `ee context`, and query-file packs, precedence is:

1. Explicit `--candidate-pool N`.
2. Query-file `budget.candidatePool`, when using a query file.
3. An applicable task-lens candidate-pool override.
4. Workspace configuration, then user configuration.
5. The built-in configuration default, **100**.

An explicit `--candidate-pool 100` is an override, not a signal to use the
configuration default. Runtime operating-profile caps still apply after this
resolution, so the effective request pool can be smaller than the configured
value. This setting controls the candidates considered for packing, not the
number guaranteed to appear in the final token-budgeted pack.

Both TOML configuration and `config set` require an integer from **1** through
**4294967295**. Invalid values are rejected without changing the config file.
Existing unrelated settings and comments are preserved.

Regression coverage lives in `config_candidate_pool_tests`,
`context_candidate_pool_tests`, `cli::candidate_pool_tests`, and the public
command test `tests/pack_candidate_pool_cli.py`.
