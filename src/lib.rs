//! mrx as a library: config loading, repo sets, operation planning, the
//! executor, and the terminal front ends. `main.rs` is a thin CLI dispatch
//! over this crate.

pub mod cli;
pub mod config;
pub mod executor;
pub mod operations;
pub mod render_plain;
pub mod sets;
pub mod summarize;
pub mod ui;
