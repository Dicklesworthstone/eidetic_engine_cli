pub mod installer;

pub use installer::{
    HookInstallOptions, HookInstallReport, HookStatusOptions, HookStatusReport, HookType,
    PREFLIGHT_HOOK_SHELL_SCHEMA_V1, PreflightHookShell, PreflightHookShellOptions,
    PreflightHookShellReport, check_hook_status, generate_preflight_shell_snippet, install_hooks,
};

pub const SUBSYSTEM: &str = "hooks";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::subsystem_name;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "hooks");
    }
}
