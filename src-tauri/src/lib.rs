#![recursion_limit = "512"]

// lib.rs — Tauri application library entry point.
// main.rs calls run() from here; this allows Tauri mobile targets to work.

#[cfg(not(test))]
pub mod agent;
#[cfg(not(test))]
mod browser;
#[cfg(not(test))]
mod commands;
#[cfg(not(test))]
mod desktop_app;
#[cfg(not(test))]
mod fish;
#[cfg(not(test))]
mod gateway;
#[cfg(not(test))]
pub mod koi;
#[cfg(not(test))]
mod llm;
#[cfg(not(test))]
mod memory;
#[cfg(not(test))]
mod pisci;
#[cfg(not(test))]
mod policy;
#[cfg(not(test))]
mod project_context;
#[cfg(not(test))]
mod scheduler;
#[cfg(not(test))]
mod security;
#[cfg(not(test))]
mod skills;
#[cfg(not(test))]
pub mod store;
#[cfg(not(test))]
mod tools;

#[cfg(not(test))]
pub use desktop_app::run;
#[cfg(not(test))]
pub use store::AppState;

#[cfg(test)]
pub fn run() {}
