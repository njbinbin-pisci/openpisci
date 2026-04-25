//! Desktop app top-level: bootstrap / lifecycle glue.
//!
//! Split out from the old monolithic `desktop_app.rs`:
//! - [`logging`] — log dir + rolling file + crash reporter
//! - [`markers`] — IM outbound `SEND_IMAGE:` / `SEND_FILE:` marker parsing
//! - [`headless`] — shared headless execution helpers
//! - [`bootstrap`] — Tauri `Builder` setup, plugin wiring, background
//!   loops, and the `invoke_handler!` command registration

pub mod bootstrap;
pub mod headless;
pub mod logging;
pub mod markers;

pub use bootstrap::run;
