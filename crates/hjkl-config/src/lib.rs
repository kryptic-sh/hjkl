//! Shared TOML config loader for hjkl-based apps.
//!
//! Apps implement [`AppConfig`] on their config struct and call [`load`]
//! to read it from the platform's XDG config directory. Missing files
//! return `Default::default()` paired with [`ConfigSource::Defaults`] —
//! no file is ever auto-created. Use [`write_default`] explicitly if a
//! consumer wants to scaffold a starter config on user request.
//!
//! ```no_run
//! use hjkl_config::{AppConfig, load};
//! use serde::Deserialize;
//!
//! #[derive(Debug, Default, Deserialize)]
//! struct MyConfig {
//!     greeting: String,
//! }
//!
//! impl AppConfig for MyConfig {
//!     const APPLICATION: &'static str = "myapp";
//! }
//!
//! let (cfg, source) = load::<MyConfig>().unwrap();
//! ```

mod error;
mod loader;
mod validate;

pub use error::ConfigError;
pub use loader::{
    AppConfig, ConfigSource, cache_dir, config_dir, config_path, data_dir, load, load_from,
    load_layered, load_layered_from, write_default,
};
pub use validate::{
    Validate, ValidationError, ensure_non_empty_str, ensure_non_zero, ensure_one_of, ensure_range,
};
