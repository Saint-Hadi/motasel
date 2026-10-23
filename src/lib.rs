//! motasel — a simple, secure local-network file sharing web application.
//!
#![allow(missing_docs)]
#![forbid(unsafe_code)]

pub mod app;
pub mod config;
pub mod errors;
pub mod extract;
pub mod handlers;
pub mod models;
pub mod state;
pub mod storage;

pub use app::build_app;
pub use config::Config;
pub use state::AppState;
