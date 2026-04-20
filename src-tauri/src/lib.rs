#![recursion_limit = "512"]

// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

pub mod agent;
mod browser;
mod commands;
#[cfg(not(test))]
mod desktop_app;
mod fish;
mod gateway;
pub mod headless_cli;
pub mod koi;
mod llm;
mod memory;
mod pisci;
mod policy;
mod project_context;
mod scheduler;
mod security;
mod skills;
pub mod store;
mod tools;

#[cfg(not(test))]
pub use desktop_app::run;
pub use store::AppState;

#[cfg(test)]
pub fn run() {}
