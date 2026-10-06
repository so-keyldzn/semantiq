//! CLI command implementations for Semantiq

mod calibrate;
mod common;
mod index;
mod init;
mod init_cursor;
pub(crate) mod query;
mod serve;
mod stats;
mod update;

pub(crate) use calibrate::calibrate;
pub(crate) use index::{Phases, index_with};
pub(crate) use init::{InitOptions, init};
pub(crate) use init_cursor::init_cursor;
pub(crate) use serve::serve;
pub(crate) use stats::stats;
pub(crate) use update::update;
