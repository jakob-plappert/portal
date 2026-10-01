/// Product labels live in one presentation-only module. Persistence and
/// generation contracts use stable generic IDs, so renaming Nexus later does
/// not require a database migration or worker protocol change.
pub const NEXUS_DISPLAY_NAME: &str = "Nexus";
pub const NEXUS_SUBTITLE: &str = "AI Content Studio";
