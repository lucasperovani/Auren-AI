//! Auren-AI server library.
//!
//! The binary in `main.rs` is a thin shell around this library so that
//! integration tests can exercise the same code paths as the running server.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod events;
pub mod services;
pub mod state;