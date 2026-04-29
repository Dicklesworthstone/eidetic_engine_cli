pub mod path;
pub mod workspace;

pub use path::{PathExpander, PathExpansionError};
pub use workspace::{
    WORKSPACE_MARKER, WorkspaceError, WorkspaceLocation, discover, discover_from_current_dir,
};

pub const SUBSYSTEM: &str = "config";

#[must_use]
pub const fn subsystem_name() -> &'static str {
    SUBSYSTEM
}

#[cfg(test)]
mod tests {
    use super::subsystem_name;

    #[test]
    fn subsystem_name_is_stable() {
        assert_eq!(subsystem_name(), "config");
    }
}
