//! `semantiq update` — check for and install a newer release in place.

use anyhow::Result;
use semantiq_mcp::self_update::{self, UpdateOptions};

/// Current binary version, baked in at compile time.
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Check for a newer release and, unless `check_only`, replace the running
/// binary with it.
///
/// The network I/O and filesystem swap are blocking, so the work runs on a
/// dedicated blocking thread to avoid stalling the Tokio runtime.
pub(crate) async fn update(check_only: bool, force: bool) -> Result<()> {
    let opts = UpdateOptions { check_only, force };
    tokio::task::spawn_blocking(move || self_update::run(CURRENT_VERSION, opts)).await??;
    Ok(())
}
