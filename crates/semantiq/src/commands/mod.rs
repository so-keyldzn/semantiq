//! CLI command implementations for Semantiq

mod index;
mod init;
mod init_cursor;
mod search;
mod serve;
mod stats;

pub use index::index;
pub use init::init;
pub use init_cursor::init_cursor;
pub use search::search;
pub use serve::serve;
pub use stats::stats;
