//! Widget packages, settings schemas, layout and configuration for Vitrine 2.

pub mod config;
pub mod error;
mod fsutil;
pub mod gallery;
pub mod layout;
pub mod locale;
pub mod manifest;
pub mod package;
pub mod paths;
pub mod settings;

pub use error::{Error, Result};
