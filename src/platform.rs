/// Small platform enum used by pure path-resolution code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Platform {
    /// macOS.
    MacOs,
    /// Linux and other Unix-like non-macOS targets.
    Linux,
    /// Windows.
    Windows,
}

impl Platform {
    /// Resolve the current compilation target.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}
