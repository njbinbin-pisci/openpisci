#![recursion_limit = "512"]

// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

pub mod app;
mod browser;
mod commands;
mod fish;
mod gateway;
pub mod headless_cli;
pub mod host;

#[cfg(test)]
mod live_smoke;
pub mod notify;
mod pisci;
pub mod pool;
pub mod runtime;
mod skills;
pub mod store;
mod tools;

pub use app::run;
pub use store::AppState;
