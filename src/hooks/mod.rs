pub mod installer;

pub use installer::{
    HookInstallOptions, HookInstallReport, HookStatusOptions, HookStatusReport, HookType,
    check_hook_status, install_hooks,
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
