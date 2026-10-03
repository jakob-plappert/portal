use crate::paths::PortalPaths;
use crate::project::Project;
use crate::provisioning::{InfrastructureWorker, ProvisioningState, ProvisioningStateStore};
use crate::runpod::RunPodWorker;
use crate::settings::{ApiKeyState, PortalSettings, SettingsStore, load_runpod_api_key};
use crate::setup::SetupSession;
use crate::shot::Shot;
use crate::storage::NexusStore;
use std::sync::{Arc, Mutex};

pub struct AppState {
    // `None` represents the normal startup state in which no project has been
    // opened yet. Once present, the owned `Project` remains Portal's source of
    // truth; the Slint properties are only a view of this data.
    pub current_project: Option<Project>,

    // The application owns its shots. Slint receives a separate model built
    // from this vector rather than becoming responsible for project data.
    pub shots: Vec<Shot>,

    // Nexus owns its local metadata store. The store itself only owns a path,
    // so cloning it for background work does not clone a live SQLite
    // connection or create shared mutable database state.
    pub paths: PortalPaths,
    pub nexus_store: NexusStore,
    pub settings_store: SettingsStore,
    pub settings: PortalSettings,
    pub api_key: ApiKeyState,
    pub runpod_worker: Option<RunPodWorker>,
    pub infrastructure_worker: Option<InfrastructureWorker>,
    // The setup worker runs outside Slint's UI thread, so this small transient
    // state uses `Arc<Mutex<_>>`: `Arc` gives both threads ownership and
    // `Mutex` ensures only one can mutate the state at a time. Durable resource
    // identity still lives in `ProvisioningState`, not in this UI session.
    pub setup_session: Arc<Mutex<SetupSession>>,
    pub provisioning_state_store: ProvisioningStateStore,
    pub provisioning_state: ProvisioningState,
    pub current_conversation_id: Option<String>,
    pub selected_character_id: Option<String>,
    pub selected_reference_media_id: Option<String>,
}

impl AppState {
    pub fn initialize() -> Result<Self, String> {
        let paths = PortalPaths::discover();
        paths.initialize()?;
        let nexus_store = NexusStore::open(paths.database_path())?;
        let settings_store = SettingsStore::new(paths.settings_path());
        let settings = settings_store.load()?;
        let provisioning_state_store = ProvisioningStateStore::new(paths.infrastructure_path());
        let provisioning_state = provisioning_state_store.load()?;
        let api_key = load_runpod_api_key();
        let setup_session = Arc::new(Mutex::new(SetupSession::new(api_key.value.is_some())));

        Ok(Self {
            current_project: None,
            shots: Vec::new(),
            paths,
            nexus_store,
            settings_store,
            settings,
            api_key,
            runpod_worker: None,
            infrastructure_worker: None,
            setup_session,
            provisioning_state_store,
            provisioning_state,
            current_conversation_id: None,
            selected_character_id: None,
            selected_reference_media_id: None,
        })
    }
}
