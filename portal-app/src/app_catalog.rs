use crate::paths::validate_app_id;

/// The identity of every application compiled into Portal.
///
/// This enum is deliberately closed: adding an application is a source-code
/// change that can be reviewed and tested. Portal does not discover plugins or
/// load application definitions from runtime files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalAppId {
    Home,
    Nexus,
    Compute,
    Settings,
}

impl PortalAppId {
    /// Slint still selects one of four explicit page branches by integer. This
    /// conversion keeps those boundary values in one Rust location instead of
    /// repeating them throughout callback code.
    pub const fn navigation_index(self) -> i32 {
        match self {
            Self::Home => 0,
            Self::Nexus => 1,
            Self::Compute => 2,
            Self::Settings => 3,
        }
    }

    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Nexus => "nexus",
            Self::Compute => "compute",
            Self::Settings => "settings",
        }
    }

    pub fn from_navigation_index(index: i32) -> Option<Self> {
        match index {
            0 => Some(Self::Home),
            1 => Some(Self::Nexus),
            2 => Some(Self::Compute),
            3 => Some(Self::Settings),
            _ => None,
        }
    }
}

/// Presentation and storage facts for a built-in application.
///
/// `storage_id` is optional because shell surfaces such as Home and Settings
/// do not need empty data directories. When it is present, the identifier is
/// validated by the same path policy as `PortalPaths::app_dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortalAppDescriptor {
    pub id: PortalAppId,
    pub display_name: &'static str,
    pub subtitle: &'static str,
    pub storage_id: Option<&'static str>,
}

pub const PORTAL_APPS: &[PortalAppDescriptor] = &[
    PortalAppDescriptor {
        id: PortalAppId::Home,
        display_name: "Home",
        subtitle: "Projects and shots",
        storage_id: None,
    },
    PortalAppDescriptor {
        id: PortalAppId::Nexus,
        display_name: "Nexus",
        subtitle: "AI Content Studio",
        storage_id: Some("nexus"),
    },
    PortalAppDescriptor {
        id: PortalAppId::Compute,
        display_name: "Compute",
        subtitle: "RunPod queue and local job history",
        storage_id: None,
    },
    PortalAppDescriptor {
        id: PortalAppId::Settings,
        display_name: "Settings",
        subtitle: "Portal and Nexus configuration",
        storage_id: None,
    },
];

pub fn descriptor(id: PortalAppId) -> &'static PortalAppDescriptor {
    PORTAL_APPS
        .iter()
        .find(|descriptor| descriptor.id == id)
        .expect("every PortalAppId must have exactly one catalog descriptor")
}

/// Fail during startup if a source edit introduces duplicate identities or an
/// unsafe storage directory. The catalog is compile-time data, but validating
/// it makes accidental maintenance errors visible rather than silently mapping
/// two internal applications to one navigation slot or directory.
pub fn validate_catalog() -> Result<(), String> {
    validate_descriptors(PORTAL_APPS)
}

fn validate_descriptors(apps: &[PortalAppDescriptor]) -> Result<(), String> {
    for (index, app) in apps.iter().enumerate() {
        validate_app_id(app.id.stable_id())?;
        if let Some(storage_id) = app.storage_id {
            validate_app_id(storage_id)?;
        }
        if PortalAppId::from_navigation_index(app.id.navigation_index()) != Some(app.id) {
            return Err(format!(
                "Portal app '{}' has an inconsistent navigation mapping.",
                app.id.stable_id()
            ));
        }

        for other in &apps[index + 1..] {
            if app.id == other.id || app.id.stable_id() == other.id.stable_id() {
                return Err(format!(
                    "Portal app catalog contains duplicate ID '{}'.",
                    app.id.stable_id()
                ));
            }
            if app.id.navigation_index() == other.id.navigation_index() {
                return Err(format!(
                    "Portal app catalog contains duplicate navigation index {}.",
                    app.id.navigation_index()
                ));
            }
            if let (Some(storage_id), Some(other_storage_id)) = (app.storage_id, other.storage_id)
                && storage_id == other_storage_id
            {
                return Err(format!(
                    "Portal app catalog contains duplicate storage ID '{storage_id}'."
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::PortalPaths;

    #[test]
    fn catalog_ids_and_navigation_indices_are_unique() {
        validate_catalog().expect("the built-in app catalog must be valid");

        for (index, app) in PORTAL_APPS.iter().enumerate() {
            assert_eq!(app.id.navigation_index(), index as i32);
            assert_eq!(
                PortalAppId::from_navigation_index(index as i32),
                Some(app.id)
            );
        }
        assert_eq!(PortalAppId::from_navigation_index(-1), None);
        assert_eq!(PortalAppId::from_navigation_index(4), None);
    }

    #[test]
    fn stable_app_and_storage_ids_do_not_drift() {
        let expected = [
            (PortalAppId::Home, "home", "Home", None),
            (PortalAppId::Nexus, "nexus", "Nexus", Some("nexus")),
            (PortalAppId::Compute, "compute", "Compute", None),
            (PortalAppId::Settings, "settings", "Settings", None),
        ];

        for (app, (expected_id, expected_stable_id, expected_display_name, expected_storage_id)) in
            PORTAL_APPS.iter().zip(expected)
        {
            assert_eq!(app.id, expected_id);
            assert_eq!(app.id.stable_id(), expected_stable_id);
            assert_eq!(app.display_name, expected_display_name);
            assert_eq!(app.storage_id, expected_storage_id);
        }
    }

    #[test]
    fn invalid_storage_ids_cannot_enter_a_catalog() {
        let invalid = [PortalAppDescriptor {
            id: PortalAppId::Nexus,
            display_name: "Nexus",
            subtitle: "AI Content Studio",
            storage_id: Some("../nexus"),
        }];

        assert!(validate_descriptors(&invalid).is_err());
    }

    #[test]
    fn every_catalog_storage_id_passes_portal_path_validation() {
        let paths = PortalPaths::new("portaldata");
        for storage_id in PORTAL_APPS
            .iter()
            .filter_map(|descriptor| descriptor.storage_id)
        {
            assert!(paths.app_dir(storage_id).is_ok());
        }
    }

    #[test]
    fn nexus_resolves_to_the_existing_app_scoped_directory() {
        let paths = PortalPaths::new("portaldata");
        let nexus = descriptor(PortalAppId::Nexus);
        let storage_id = nexus.storage_id.expect("Nexus owns durable storage");

        assert_eq!(storage_id, "nexus");
        assert_eq!(
            paths.app_dir(storage_id).expect("Nexus ID must be valid"),
            paths.nexus_dir()
        );
    }
}
