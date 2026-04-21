//! Local storage primitives: SQLite database plus encrypted settings store.
//!
//! The desktop / CLI hosts wrap these in their own top-level state container
//! (the desktop's `AppState` adds Tauri handles, browser manager, IM gateway,
//! …). The kernel itself only needs raw `Database` and `Settings` access.

pub mod db;
pub mod settings;

pub use db::Database;
pub use settings::Settings;
