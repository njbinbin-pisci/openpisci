// lib.rs — required for Tauri's crate-type = ["staticlib", "cdylib", "rlib"]
// The actual application entry point is in main.rs

pub mod agent;
pub mod commands;
pub mod llm;
pub mod policy;
pub mod scheduler;
pub mod store;
pub mod tools;
