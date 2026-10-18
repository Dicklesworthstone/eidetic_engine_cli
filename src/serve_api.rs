//! HTTP transport compatibility surface and the durable daemon journal API.
//!
//! Transport implementation and wire types remain in `serve.rs`. Explicit
//! journal exports take precedence over the transport module's historical
//! implementations so CLI status, startup and recovery all use the same
//! locking and bounded-reader contract.

#[path = "serve.rs"]
mod transport;
pub use transport::*;

#[path = "daemon/job_journal.rs"]
mod job_journal;

pub use job_journal::{
    DaemonExecution, DaemonJobInvocation, DaemonRecoveryReport, DaemonRunPlan,
    daemon_status_report, load_daemon_job_rows, record_daemon_foreground_report,
    record_daemon_foreground_start, recover_orphaned_daemon_jobs,
};

#[path = "daemon/durable_foreground.rs"]
mod durable_foreground;

pub use durable_foreground::{
    DaemonExecutionReport, run_daemon_foreground_durable,
    run_daemon_foreground_durable_supervised,
};
