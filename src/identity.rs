/// Runtime mode for path and naming resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeMode {
    /// Safe-by-default development mode that cannot collide with production
    /// Shipyard state unless explicitly pointed at the same directories.
    Isolated,
    /// Production Shipyard mode.
    Shipyard,
}

impl RuntimeMode {
    /// Stable string form for output and serialization.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::Shipyard => "shipyard",
        }
    }
}

/// Product naming and directory identity for a given runtime mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductIdentity {
    /// CLI binary name.
    pub binary_name: &'static str,
    /// Machine-global config directory stem.
    pub config_stem: &'static str,
    /// Machine-global state directory stem.
    pub state_stem: &'static str,
    /// Tracked per-repo configuration directory.
    pub tracked_project_dir_name: &'static str,
    /// Private per-repo overlay directory.
    pub local_overlay_dir_name: &'static str,
}

impl ProductIdentity {
    /// Resolve the product identity for a runtime mode.
    #[must_use]
    pub const fn for_mode(mode: RuntimeMode) -> Self {
        match mode {
            RuntimeMode::Isolated => Self {
                binary_name: "shipyard",
                config_stem: "shipyard-dev",
                state_stem: "shipyard-dev",
                tracked_project_dir_name: ".shipyard",
                local_overlay_dir_name: ".shipyard-dev.local",
            },
            RuntimeMode::Shipyard => Self {
                binary_name: "shipyard",
                config_stem: "shipyard",
                state_stem: "shipyard",
                tracked_project_dir_name: ".shipyard",
                local_overlay_dir_name: ".shipyard.local",
            },
        }
    }
}
