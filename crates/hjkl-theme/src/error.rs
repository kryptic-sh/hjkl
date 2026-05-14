/// Errors produced by theme parsing and resolution.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid hex color: {0:?}")]
    BadHex(String),
    #[error("unresolved palette ref: ${0}")]
    UnresolvedPalette(String),
    #[error("invalid modifier: {0:?}")]
    BadModifier(String),
}
