//! Language server for Oberon-07.
//!
//! The server uses the front end of the compiler `o7c` (crate `o7-front`):
//! the diagnostics are the messages of the compiler, and all information
//! about names comes from the semantic index of the front end.
//!
//! Modules:
//!
//! - [`server`]: the message loop and the dispatch of requests;
//! - [`vfs`]: documents and the conversion of positions;
//! - [`config`]: the settings (`searchPaths`, `target`);
//! - [`workspace`]: module graph, memo table, interface snapshots;
//! - [`features`]: one module for each kind of request.

pub mod config;
pub mod features;
pub mod server;
pub mod vfs;
pub mod workspace;

/// The version of the server.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The version of the front end (`o7-front`).
pub const FRONT_VERSION: &str = env!("O7_FRONT_VERSION");
/// The commit of the compiler repository that contains the front end.
pub const FRONT_COMMIT: &str = env!("O7_FRONT_COMMIT");

/// The text of `--version` and of `serverInfo.version`.
pub fn version_text() -> String {
	format!("{VERSION} (o7-front {FRONT_VERSION}, compiler commit {FRONT_COMMIT})")
}
