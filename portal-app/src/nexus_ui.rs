use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;

use slint::{ComponentHandle, Image, ModelRc, VecModel};

use crate::app_state::AppState;
use crate::generation::{GenerationIntent, GenerationMode, GenerationParameters, MediaKind};
use crate::media_library::MediaLibrary;
use crate::model_catalog::{FLUX_2_DEV_ID, find_model, validate_model_mode};
use crate::prompt::PromptCompilerMode;
use crate::provisioning::{
    FLUX_WORKER_IMAGE, GpuPolicy, InfrastructureCommand, InfrastructureEvent, InfrastructureWorker,
    ProvisioningAction,
};
use crate::runpod::{RunPodCommand, RunPodEvent, RunPodWorker};
use crate::settings::{ApiKeySource, load_runpod_api_key, save_runpod_api_key};
use crate::setup::{DiagnosticContext, SetupState, diagnostic_report};
use crate::storage::{CharacterReferenceRole, JobStatus, MessageRole};
use crate::worker_contract::{Dimensions, MediaWorkerRequest};
use crate::{
    CharacterListItem, ConversationListItem, DiagnosticListItem, JobListItem, MainWindow,
    MediaListItem, MessageListItem,
};

pub fn setup(window: &MainWindow, state: Rc<RefCell<AppState>>) -> Result<(), String> {
    setup_background_worker(window, Rc::clone(&state));
    setup_infrastructure_worker(window, Rc::clone(&state));
    setup_conversation_callbacks(window, Rc::clone(&state));
    setup_character_callbacks(window, Rc::clone(&state));
    setup_media_callbacks(window, Rc::clone(&state));
    setup_generation_callback(window, Rc::clone(&state));
    setup_settings_callback(window, Rc::clone(&state));
    refresh_all(window, &state.borrow())
}

fn setup_background_worker(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak_window = window.as_weak();
    let store = state.borrow().nexus_store.clone();
    let paths = state.borrow().paths.clone();
    let ui_paths = paths.clone();
    let setup_session = state.borrow().setup_session.clone();
    let worker = RunPodWorker::start(store.clone(), paths, move |event| {
        // Network work finishes on the worker thread. SQLite is updated there,
        // then `upgrade_in_event_loop` moves only a small owned closure back to
        // Slint's UI thread; Slint widgets are never touched from this thread.
        let (job_id, status_text, durable_status) = match event {
            RunPodEvent::Updated {
                local_job_id,
                status,
                message,
            } => (local_job_id, message, status),
            RunPodEvent::Failed {
                local_job_id,
                error,
            } => (local_job_id, error, JobStatus::Failed),
        };
        let (setup_label, setup_index) = if let Ok(mut setup) = setup_session.lock() {
            setup.update_test_job(
                &job_id,
                durable_status.as_str(),
                durable_status == JobStatus::Completed,
            );
            (setup.state.label().to_string(), setup.state.ui_index())
        } else {
            (String::from("Setup state unavailable"), -1)
        };
        let ui_store = store.clone();
        let ui_paths = ui_paths.clone();
        let _ = weak_window.upgrade_in_event_loop(move |window| {
            window.set_nexus_status(status_text.into());
            window.set_compute_status(format!("Updated job {job_id}.").into());
            window.set_setup_state_label(setup_label.into());
            window.set_setup_stage(setup_index);
            if let Err(error) = refresh_jobs(&window, &ui_store) {
                window.set_compute_status(error.into());
            }
            if let Err(error) = refresh_media(&window, &ui_store, &ui_paths) {
                window.set_media_status(error.into());
            }
        });
    });
    state.borrow_mut().runpod_worker = Some(worker.clone());

    // Jobs with a persisted remote ID can be reconciled after a restart. No
    // unknown job is guessed or resumed without that explicit provider ID.
    let borrowed = state.borrow();
    let image_endpoint_id = borrowed
        .provisioning_state
        .image_endpoint_id
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| borrowed.settings.runpod.image_endpoint_id.clone());
    let video_endpoint_id = borrowed.settings.runpod.video_endpoint_id.clone();
    if let Some(api_key) = borrowed.api_key.value.clone()
        && let Ok(jobs) = borrowed.nexus_store.non_terminal_remote_jobs()
    {
        for job in jobs {
            let endpoint_id = match job.mode.output_kind() {
                MediaKind::Image => image_endpoint_id.clone(),
                MediaKind::Video => video_endpoint_id.clone(),
                MediaKind::Audio => continue,
            };
            if endpoint_id.is_empty() {
                continue;
            }
            if let Some(remote_job_id) = job.remote_job_id {
                let _ = worker.send(RunPodCommand::Reconcile {
                    local_job_id: job.id,
                    api_key: api_key.clone(),
                    endpoint_id,
                    remote_job_id,
                    model_id: job.model_id,
                });
            }
        }
    }
}

fn setup_infrastructure_worker(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak_window = window.as_weak();
    let state_store = state.borrow().provisioning_state_store.clone();
    let paths = state.borrow().paths.clone();
    let store = state.borrow().nexus_store.clone();
    let setup_session = state.borrow().setup_session.clone();
    let worker = InfrastructureWorker::start(state_store, paths, store, move |event| {
        let (setup_label, setup_index) = if let Ok(mut setup) = setup_session.lock() {
            match &event {
                InfrastructureEvent::PreflightFinished {
                    report,
                    infrastructure_ready,
                    ..
                } => setup.finish_preflight(report.clone(), *infrastructure_ready),
                InfrastructureEvent::PlanReady(_) => {
                    let _ = setup.plan_ready();
                    let _ = setup.await_infrastructure_confirmation();
                }
                InfrastructureEvent::Applied(_) => setup.infrastructure_ready(),
                InfrastructureEvent::Failed(_) if setup.state == SetupState::Provisioning => {
                    setup.state = SetupState::ApiKeyReady;
                }
                _ => {}
            }
            (setup.state.label().to_string(), setup.state.ui_index())
        } else {
            (String::from("Setup state unavailable"), -1)
        };
        let _ = weak_window.upgrade_in_event_loop(move |window| match event {
            InfrastructureEvent::PreflightFinished {
                report,
                infrastructure_ready,
                ready_volume,
                ready_endpoint,
            } => {
                window.set_setup_state_label(setup_label.into());
                window.set_setup_stage(setup_index);
                set_diagnostic_model(&window, &report.checks);
                set_preflight_summary(&window, &report.checks);
                if report.has_failures() {
                    window.set_settings_status(
                        "Preflight found blocking failures. No billable action occurred.".into(),
                    );
                } else if infrastructure_ready {
                    window.set_settings_status(
                        "Preflight passed and existing managed infrastructure was rediscovered."
                            .into(),
                    );
                    window.set_compute_status(
                        "Infrastructure Ready. You may run an explicitly confirmed test generation."
                            .into(),
                    );
                } else {
                    window.set_settings_status(
                        "Preflight passed. Review an infrastructure plan before creation.".into(),
                    );
                }
                if let Some(volume) = ready_volume {
                    window.set_volume_resource(
                        format!(
                            "{} · {} GB · {} · ID {}",
                            volume.name, volume.size, volume.data_center, volume.id
                        )
                        .into(),
                    );
                }
                if let Some(endpoint) = ready_endpoint {
                    window.set_endpoint_resource(
                        format!(
                            "{} · verified · min {} / max {} · ID {}",
                            endpoint.name, endpoint.workers.min, endpoint.workers.max, endpoint.id
                        )
                        .into(),
                    );
                }
            }
            InfrastructureEvent::PlanReady(plan) => {
                window.set_setup_state_label(setup_label.into());
                window.set_setup_stage(setup_index);
                window.set_infrastructure_plan_visible(true);
                window.set_infrastructure_plan_summary(plan_summary(&plan).into());
                window.set_compute_status(
                    "Discovery complete. Review the plan before creating billable resources."
                        .into(),
                );
                window.set_cost_status(cost_summary(&plan).into());
            }
            InfrastructureEvent::Progress(message) => {
                let previous = window.get_apply_progress().to_string();
                let progress = if previous.is_empty() {
                    message
                } else {
                    format!("{previous}\n{message}")
                };
                window.set_apply_progress(progress.into());
            }
            InfrastructureEvent::Applied(result) => {
                window.set_setup_state_label(setup_label.into());
                window.set_setup_stage(setup_index);
                window.set_infrastructure_plan_visible(false);
                window.set_compute_status(result.messages.join(" ").into());
                window.set_volume_resource(
                    format!(
                        "{} · {} GB · {} · ID {}",
                        result.network_volume.name,
                        result.network_volume.size,
                        result.network_volume.data_center,
                        result.network_volume.id
                    )
                    .into(),
                );
                window.set_endpoint_resource(
                    format!(
                        "{} · verified · min {} / max {} · ID {}",
                        result.image_endpoint.name,
                        result.image_endpoint.workers.min,
                        result.image_endpoint.workers.max,
                        result.image_endpoint.id
                    )
                    .into(),
                );
            }
            InfrastructureEvent::Failed(error) => {
                window.set_setup_state_label(setup_label.into());
                window.set_setup_stage(setup_index);
                let previous = window.get_apply_progress().to_string();
                if !previous.is_empty() {
                    window.set_apply_progress(format!("{previous}\n✗ {error}").into());
                }
                window.set_compute_status(error.clone().into());
                window.set_settings_status(error.into());
            }
        });
    });
    state.borrow_mut().infrastructure_worker = Some(worker.clone());

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_run_preflight(move || {
        let result = start_preflight(&callback_state);
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(()) => {
                    window.set_setup_state_label("Preflight running".into());
                    window.set_setup_stage(SetupState::PreflightRunning.ui_index());
                    window.set_settings_status(
                        "Running read-only API, permission, GHCR, storage, and discovery checks…"
                            .into(),
                    );
                }
                Err(error) => window.set_settings_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_plan_infrastructure(move |size| {
        let size = size
            .trim()
            .parse::<u32>()
            .map_err(|_| String::from("Volume size must be a whole number of GB."));
        let result = size.and_then(|volume_size_gb| {
            let state = callback_state.borrow();
            state
                .infrastructure_worker
                .as_ref()
                .ok_or_else(|| String::from("Infrastructure worker is unavailable."))?
                .send(InfrastructureCommand::PlanInfrastructure {
                    policy: GpuPolicy::Balanced,
                    volume_size_gb,
                })
        });
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(()) => {
                    window.set_infrastructure_plan_visible(false);
                    window.set_compute_status(
                        "Building a plan from the successful preflight discovery…".into(),
                    );
                }
                Err(error) => window.set_compute_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_apply_infrastructure(move || {
        let result = (|| {
            let state = callback_state.borrow();
            let api_key = state
                .api_key
                .value
                .clone()
                .ok_or_else(|| String::from("RunPod API key is not configured."))?;
            state
                .setup_session
                .lock()
                .map_err(|_| String::from("Setup state is unavailable."))?
                .confirm_infrastructure()?;
            state
                .infrastructure_worker
                .as_ref()
                .ok_or_else(|| String::from("Infrastructure worker is unavailable."))?
                .send(InfrastructureCommand::ApplyConfirmed { api_key })
        })();
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(()) => {
                    window.set_setup_state_label("Provisioning infrastructure".into());
                    window.set_setup_stage(SetupState::Provisioning.ui_index());
                    window.set_apply_progress(String::new().into());
                    window.set_compute_status(
                        "Applying the confirmed plan. Successful resources will be retained for safe retry."
                            .into(),
                    );
                }
                Err(error) => {
                    if let Ok(mut setup) = callback_state.borrow().setup_session.lock()
                        && setup.state == SetupState::Provisioning
                    {
                        setup.state = SetupState::AwaitingInfrastructureConfirmation;
                        window.set_setup_state_label(setup.state.label().into());
                        window.set_setup_stage(setup.state.ui_index());
                    }
                    window.set_compute_status(error.into());
                }
            }
        }
    });

    setup_test_generation_callbacks(window, Rc::clone(&state));
    setup_diagnostic_callbacks(window, Rc::clone(&state));

    // With a stored key, startup automatically performs only read-only checks.
    // This is how valid managed infrastructure is rediscovered after restart;
    // no create/update request can occur without the later confirmation click.
    if state.borrow().api_key.value.is_some() && start_preflight(&state).is_ok() {
        window.set_setup_state_label("Preflight running".into());
        window.set_setup_stage(SetupState::PreflightRunning.ui_index());
        window.set_settings_status("Rediscovering existing RunPod infrastructure…".into());
    }
}

fn start_preflight(state: &Rc<RefCell<AppState>>) -> Result<(), String> {
    let borrowed = state.borrow();
    let api_key = borrowed
        .api_key
        .value
        .clone()
        .ok_or_else(|| String::from("Save a RunPod API key first."))?;
    borrowed
        .setup_session
        .lock()
        .map_err(|_| String::from("Setup state is unavailable."))?
        .begin_preflight()?;
    borrowed
        .infrastructure_worker
        .as_ref()
        .ok_or_else(|| String::from("Infrastructure worker is unavailable."))?
        .send(InfrastructureCommand::RunPreflight { api_key })
}

fn setup_test_generation_callbacks(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_request_test_generation(move || {
        let result = callback_state
            .borrow()
            .setup_session
            .lock()
            .map_err(|_| String::from("Setup state is unavailable."))
            .and_then(|mut setup| setup.request_test_generation());
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(()) => {
                    window.set_setup_state_label("Awaiting test generation confirmation".into());
                    window
                        .set_setup_stage(SetupState::AwaitingTestGenerationConfirmation.ui_index());
                    window.set_test_generation_dialog_open(true);
                }
                Err(error) => window.set_compute_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_cancel_test_generation(move || {
        if let Ok(mut setup) = callback_state.borrow().setup_session.lock() {
            setup.cancel_test_generation();
            if let Some(window) = weak.upgrade() {
                window.set_setup_state_label(setup.state.label().into());
                window.set_setup_stage(setup.state.ui_index());
            }
        }
    });

    let weak = window.as_weak();
    window.on_confirm_test_generation(move |prompt| {
        let result = (|| {
            {
                let borrowed = state.borrow();
                borrowed
                    .setup_session
                    .lock()
                    .map_err(|_| String::from("Setup state is unavailable."))?
                    .confirm_test_generation()?;
            }
            // This is the normal production intent/request/job path. The only
            // test-specific behavior is the preceding human confirmation.
            let job_id = prepare_submission(
                &state,
                "text_to_image",
                FLUX_2_DEV_ID,
                &prompt,
                "",
                "",
                "1024x1024",
                "",
            )?;
            state
                .borrow()
                .setup_session
                .lock()
                .map_err(|_| String::from("Setup state is unavailable."))?
                .track_test_job(job_id.clone());
            Ok::<String, String>(job_id)
        })();
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(job_id) => {
                    window.set_setup_state_label("Test generation running".into());
                    window.set_setup_stage(SetupState::TestGenerationRunning.ui_index());
                    window.set_nexus_status(format!("Submitting test job {job_id} to RunPod…").into());
                    window.set_compute_status(
                        "Submitting to RunPod. First FLUX.2 startup may take significantly longer while model files are downloaded to the Network Volume."
                            .into(),
                    );
                }
                Err(error) => {
                    if let Ok(mut setup) = state.borrow().setup_session.lock() {
                        setup.state = SetupState::InfrastructureReady;
                        setup.last_generation_state = Some(String::from("failed"));
                        window.set_setup_state_label(setup.state.label().into());
                        window.set_setup_stage(setup.state.ui_index());
                    }
                    window.set_compute_status(error.into());
                }
            }
        }
    });
}

fn setup_diagnostic_callbacks(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_copy_diagnostic_report(move || {
        let result = (|| {
            let borrowed = callback_state.borrow();
            let setup = borrowed
                .setup_session
                .lock()
                .map_err(|_| String::from("Setup state is unavailable."))?
                .clone();
            let provisioning = borrowed.provisioning_state_store.load()?;
            let jobs = borrowed.nexus_store.list_jobs()?;
            let last_job = jobs.first();
            let api_key = borrowed.api_key.value.clone();
            let media_root = borrowed.paths.media_dir();
            drop(borrowed);
            let report = diagnostic_report(DiagnosticContext {
                setup: &setup,
                provisioning: &provisioning,
                local_media_root: &media_root,
                last_job_id: last_job.map(|job| job.id.as_str()),
                last_job_status: last_job.map(|job| job.status.as_str()),
                api_key: api_key.as_deref(),
            });
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|error| format!("Could not access the system clipboard: {error}"))?;
            clipboard
                .set_text(report)
                .map_err(|error| format!("Could not copy the diagnostic report: {error}"))
        })();
        if let Some(window) = weak.upgrade() {
            window.set_settings_status(
                result
                    .map(|()| String::from("Safe diagnostic report copied to the clipboard."))
                    .unwrap_or_else(|error| error)
                    .into(),
            );
        }
    });

    let weak = window.as_weak();
    window.on_open_data_folder(move || {
        let result = crate::platform::open_folder(&state.borrow().paths.nexus_dir());
        if let Some(window) = weak.upgrade() {
            window.set_settings_status(
                result
                    .map(|()| String::from("Opened the Nexus data folder."))
                    .unwrap_or_else(|error| error)
                    .into(),
            );
        }
    });
}

fn set_diagnostic_model(window: &MainWindow, checks: &[crate::setup::PreflightCheck]) {
    let items = checks
        .iter()
        .map(|check| DiagnosticListItem {
            status: check.status.label().into(),
            name: check.name.clone().into(),
            summary: check.summary.clone().into(),
            detail: check.detail.clone().into(),
        })
        .collect::<Vec<_>>();
    window.set_setup_diagnostics(ModelRc::new(VecModel::from(items)));
}

fn set_preflight_summary(window: &MainWindow, checks: &[crate::setup::PreflightCheck]) {
    let status_for = |name: &str| {
        checks
            .iter()
            .find(|check| check.name == name)
            .map(|check| format!("{} · {}", check.status.label(), check.summary))
            .unwrap_or_else(|| String::from("Not checked"))
    };
    window.set_runpod_status(status_for("RunPod API").into());
    window.set_worker_image_status(status_for("GHCR worker image").into());
    window.set_local_storage_status(status_for("Local Nexus storage").into());
}

fn setup_conversation_callbacks(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_new_conversation(move || {
        let result = callback_state
            .borrow()
            .nexus_store
            .create_conversation(Some("New conversation"), None);
        match result {
            Ok(conversation) => {
                callback_state.borrow_mut().current_conversation_id = Some(conversation.id.clone());
                if let Some(window) = weak.upgrade() {
                    window.set_current_conversation_id(conversation.id.into());
                    window.set_current_conversation_title("New conversation".into());
                    window
                        .set_messages(ModelRc::new(VecModel::from(Vec::<MessageListItem>::new())));
                    let _ = refresh_conversations(&window, &callback_state.borrow().nexus_store);
                }
            }
            Err(error) => set_status(&weak, error),
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_select_conversation(move |id| {
        let id = id.to_string();
        callback_state.borrow_mut().current_conversation_id = Some(id.clone());
        if let Some(window) = weak.upgrade() {
            match callback_state.borrow().nexus_store.list_messages(&id) {
                Ok(messages) => {
                    window.set_current_conversation_id(id.into());
                    set_message_model(&window, messages);
                    window.set_nexus_status("Conversation loaded from local storage.".into());
                }
                Err(error) => window.set_nexus_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    window.on_send_message(move |content| {
        let content = content.to_string();
        let conversation_id = {
            let mut state = state.borrow_mut();
            if state.current_conversation_id.is_none() {
                match state
                    .nexus_store
                    .create_conversation(Some(conversation_title(&content)), None)
                {
                    Ok(conversation) => state.current_conversation_id = Some(conversation.id),
                    Err(error) => {
                        drop(state);
                        set_status(&weak, error);
                        return;
                    }
                }
            }
            state.current_conversation_id.clone().expect("set above")
        };
        let store = state.borrow().nexus_store.clone();
        match store.add_message(&conversation_id, MessageRole::User, &content) {
            Ok(_) => {
                if let Some(window) = weak.upgrade() {
                    if let Ok(messages) = store.list_messages(&conversation_id) {
                        set_message_model(&window, messages);
                    }
                    let _ = refresh_conversations(&window, &store);
                    window.set_nexus_status(
                        "Message saved. Configure a compiler or use Advanced / Manual to generate."
                            .into(),
                    );
                }
            }
            Err(error) => set_status(&weak, error),
        }
    });
}

fn setup_character_callbacks(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_save_character(move |id, name, description, notes| {
        let store = callback_state.borrow().nexus_store.clone();
        let result = if id.is_empty() {
            store
                .create_character(&name, &description, Some(&notes))
                .map(|character| character.id)
        } else {
            store
                .update_character(&id, &name, &description, Some(&notes))
                .map(|()| id.to_string())
        };
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(id) => {
                    callback_state.borrow_mut().selected_character_id = Some(id.clone());
                    window.set_selected_character_id(id.into());
                    window.set_character_status("Character saved locally.".into());
                    let _ = refresh_characters(&window, &store);
                }
                Err(error) => window.set_character_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_select_character(move |id| {
        callback_state.borrow_mut().selected_character_id = Some(id.to_string());
        if let Some(window) = weak.upgrade() {
            window.set_selected_character_id(id);
            window.set_character_status("Character selected for generation context.".into());
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_attach_character_reference(move |character_id, media_id| {
        let store = callback_state.borrow().nexus_store.clone();
        let result = if character_id.is_empty() || media_id.is_empty() {
            Err(String::from("Select a character and an image asset first."))
        } else {
            store.attach_character_reference(
                &character_id,
                &media_id,
                CharacterReferenceRole::Portrait,
            )
        };
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(()) => {
                    window.set_character_status("Reference attached.".into());
                    let _ = refresh_characters(&window, &store);
                }
                Err(error) => window.set_character_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    window.on_remove_character_reference(move |character_id, media_id| {
        let store = state.borrow().nexus_store.clone();
        let result = if character_id.is_empty() || media_id.is_empty() {
            Err(String::from(
                "Select a character and linked media asset first.",
            ))
        } else {
            store.remove_character_reference(&character_id, &media_id)
        };
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(()) => {
                    window.set_character_status(
                        "Reference association removed; the media file was kept.".into(),
                    );
                    let _ = refresh_characters(&window, &store);
                }
                Err(error) => window.set_character_status(error.into()),
            }
        }
    });
}

fn setup_media_callbacks(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_import_media(move |path, kind| {
        let kind = MediaKind::from_str(&kind);
        let state = callback_state.borrow();
        let library = MediaLibrary::new(state.paths.clone(), state.nexus_store.clone());
        let result = kind.and_then(|kind| library.import_file(Path::new(path.as_str()), kind));
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(asset) => {
                    window.set_media_status(
                        format!(
                            "Imported {} as {}.",
                            asset.relative_path.display(),
                            asset.kind
                        )
                        .into(),
                    );
                    let _ = refresh_media(&window, &state.nexus_store, &state.paths);
                }
                Err(error) => window.set_media_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    let callback_state = Rc::clone(&state);
    window.on_select_reference_media(move |id| {
        let id = id.to_string();
        let store = callback_state.borrow().nexus_store.clone();
        match store.media_asset(&id) {
            Ok(Some(asset)) => {
                callback_state.borrow_mut().selected_reference_media_id = Some(id.clone());
                if let Some(window) = weak.upgrade() {
                    window.set_selected_reference_id(id.into());
                    window.set_selected_reference_label(
                        format!("{} · {}", asset.kind, asset.relative_path.display()).into(),
                    );
                    window.set_media_status("Selected as generation reference.".into());
                }
            }
            Ok(None) => set_media_status(&weak, "Media asset was not found."),
            Err(error) => set_media_status(&weak, &error),
        }
    });

    let weak = window.as_weak();
    window.on_select_media_preview(move |id| {
        let borrowed = state.borrow();
        let result = borrowed.nexus_store.media_asset(&id).and_then(|asset| {
            asset
                .ok_or_else(|| String::from("Media asset was not found."))
                .and_then(|asset| borrowed.paths.absolute_media_path(&asset.relative_path))
        });
        if let Some(window) = weak.upgrade() {
            match result {
                Ok(path) => window
                    .set_media_status(format!("Previewing local file {}.", path.display()).into()),
                Err(error) => window.set_media_status(error.into()),
            }
        }
    });
}

fn setup_generation_callback(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    window.on_submit_generation(
        move |mode, model_id, prompt, negative, seed, dimensions, duration| {
            let result = prepare_submission(
                &state,
                &mode,
                &model_id,
                &prompt,
                &negative,
                &seed,
                &dimensions,
                &duration,
            );
            if let Some(window) = weak.upgrade() {
                match result {
                    Ok(job_id) => {
                        window.set_nexus_status(format!("Submitting local job {job_id}…").into());
                        let store = state.borrow().nexus_store.clone();
                        let _ = refresh_jobs(&window, &store);
                    }
                    Err(error) => window.set_nexus_status(error.into()),
                }
            }
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn prepare_submission(
    state: &Rc<RefCell<AppState>>,
    mode: &str,
    model_id: &str,
    prompt: &str,
    negative: &str,
    seed: &str,
    dimensions: &str,
    duration: &str,
) -> Result<String, String> {
    let mode = GenerationMode::from_str(mode)?;
    validate_model_mode(model_id, mode)?;
    let prompt = resolve_prompt(&state.borrow(), prompt)?;
    let seed = if seed.trim().is_empty() {
        None
    } else {
        Some(
            seed.trim()
                .parse::<u64>()
                .map_err(|_| String::from("Seed must be a positive whole number."))?,
        )
    };
    let dimensions = parse_dimensions(dimensions)?;
    if model_id == FLUX_2_DEV_ID
        && (!(256..=2048).contains(&dimensions.width)
            || !(256..=2048).contains(&dimensions.height)
            || dimensions.width % 16 != 0
            || dimensions.height % 16 != 0)
    {
        return Err(String::from(
            "FLUX.2 dimensions must be 256–2048 pixels and divisible by 16.",
        ));
    }
    let duration_seconds = if mode.output_kind() == MediaKind::Video {
        Some(
            duration
                .trim()
                .parse::<f32>()
                .map_err(|_| String::from("Video duration must be a number."))?,
        )
    } else {
        None
    };

    let borrowed = state.borrow();
    if let Some(required_kind) = mode.required_input() {
        let reference_id = borrowed
            .selected_reference_media_id
            .as_deref()
            .ok_or_else(|| {
                format!(
                    "{} requires a selected {} reference.",
                    mode.display_name(),
                    required_kind
                )
            })?;
        let asset = borrowed
            .nexus_store
            .media_asset(reference_id)?
            .ok_or_else(|| String::from("Selected reference no longer exists."))?;
        if asset.kind != required_kind {
            return Err(format!(
                "{} requires a {} reference, not {}.",
                mode.display_name(),
                required_kind,
                asset.kind
            ));
        }
        return Err(String::from(
            "Reference upload transport is not implemented yet; the local selection is saved and ready for the worker milestone.",
        ));
    }
    let provisioned = borrowed.provisioning_state_store.load()?;
    let endpoint_id = generation_endpoint_id(mode.output_kind(), &provisioned, &borrowed.settings);
    if endpoint_id.trim().is_empty() {
        return Err(format!(
            "RunPod {} endpoint is not configured.",
            mode.output_kind()
        ));
    }
    let api_key = borrowed
        .api_key
        .value
        .clone()
        .ok_or_else(|| String::from("RunPod API key is not configured."))?;
    let worker = borrowed
        .runpod_worker
        .clone()
        .ok_or_else(|| String::from("RunPod background worker is unavailable."))?;
    let store = borrowed.nexus_store.clone();
    drop(borrowed);

    let character_ids = state
        .borrow()
        .selected_character_id
        .clone()
        .into_iter()
        .collect();
    let parameters = match mode.output_kind() {
        MediaKind::Image => GenerationParameters::Image {
            width: dimensions.width,
            height: dimensions.height,
        },
        MediaKind::Video => GenerationParameters::Video {
            width: dimensions.width,
            height: dimensions.height,
            duration_seconds: duration_seconds.expect("video duration was parsed above"),
            audio_input_media_id: None,
        },
        MediaKind::Audio => unreachable!("generation modes do not directly output audio"),
    };
    let intent = GenerationIntent {
        mode,
        model_id: model_id.to_string(),
        user_idea: prompt.clone(),
        compiled_prompt: Some(prompt.clone()),
        negative_prompt: optional_text(negative),
        seed,
        parameters,
        character_ids,
        reference_media_ids: Vec::new(),
    };
    let request = MediaWorkerRequest::from_intent(&intent)?;
    let job = store.create_job(
        intent.mode,
        &intent.model_id,
        &intent.user_idea,
        intent.compiled_prompt.as_deref(),
        intent.seed,
    )?;
    store.update_job_metrics(
        &job.id,
        provisioned.gpu_type_id.as_deref(),
        provisioned.serverless_usd_per_hour,
        None,
    )?;
    store.update_job(&job.id, JobStatus::Preparing, None, None, None)?;
    if let Err(error) = worker.send(RunPodCommand::Submit {
        local_job_id: job.id.clone(),
        api_key,
        endpoint_id,
        request,
    }) {
        let _ = store.update_job(&job.id, JobStatus::Failed, None, None, Some(&error));
        return Err(error);
    }
    Ok(job.id)
}

fn generation_endpoint_id(
    output_kind: MediaKind,
    provisioned: &crate::provisioning::ProvisioningState,
    settings: &crate::settings::PortalSettings,
) -> String {
    match output_kind {
        // The managed endpoint is authoritative. A manual image ID is only a
        // fallback for advanced users who have not provisioned through Nexus.
        MediaKind::Image => provisioned
            .image_endpoint_id
            .clone()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| settings.runpod.image_endpoint_id.clone()),
        MediaKind::Video => settings.runpod.video_endpoint_id.clone(),
        MediaKind::Audio => unreachable!("generation modes do not directly output audio"),
    }
}

fn setup_settings_callback(window: &MainWindow, state: Rc<RefCell<AppState>>) {
    let weak = window.as_weak();
    window.on_save_settings(
        move |api_key,
              prompt_endpoint,
              image_endpoint,
              video_endpoint,
              compiler_mode,
              local_url| {
            let result = (|| {
                let mode = PromptCompilerMode::from_str(&compiler_mode)?;
                if !api_key.trim().is_empty() {
                    save_runpod_api_key(&api_key)?;
                }
                let mut state = state.borrow_mut();
                state.settings.runpod.prompt_endpoint_id = prompt_endpoint.trim().to_string();
                state.settings.runpod.image_endpoint_id = image_endpoint.trim().to_string();
                state.settings.runpod.video_endpoint_id = video_endpoint.trim().to_string();
                state.settings.prompt_compiler.mode = mode;
                state.settings.prompt_compiler.local_http_url = local_url.trim().to_string();
                state.settings_store.save(&state.settings)?;
                state.api_key = load_runpod_api_key();
                if state.api_key.value.is_some()
                    && let Ok(mut setup) = state.setup_session.lock()
                {
                    setup.api_key_saved();
                }
                // Provisioning finishes on a background thread and persists
                // its non-secret result. Reloading here refreshes the UI-side
                // snapshot without ever sending `Rc<RefCell<_>>` across
                // threads (those types are intentionally not `Send`).
                state.provisioning_state = state.provisioning_state_store.load()?;
                Ok::<(), String>(())
            })();
            if let Some(window) = weak.upgrade() {
                match result {
                    Ok(()) => {
                        refresh_settings(&window, &state.borrow());
                        window.set_settings_status(
                            "Settings saved. The API key is not stored in plaintext.".into(),
                        );
                    }
                    Err(error) => window.set_settings_status(error.into()),
                }
            }
        },
    );
}

fn refresh_all(window: &MainWindow, state: &AppState) -> Result<(), String> {
    refresh_conversations(window, &state.nexus_store)?;
    refresh_characters(window, &state.nexus_store)?;
    refresh_media(window, &state.nexus_store, &state.paths)?;
    refresh_jobs(window, &state.nexus_store)?;
    refresh_settings(window, state);
    Ok(())
}

fn refresh_conversations(
    window: &MainWindow,
    store: &crate::storage::NexusStore,
) -> Result<(), String> {
    let items = store
        .list_conversations()?
        .into_iter()
        .map(|conversation| ConversationListItem {
            id: conversation.id.into(),
            title: conversation
                .title
                .unwrap_or_else(|| String::from("Untitled conversation"))
                .into(),
        })
        .collect::<Vec<_>>();
    window.set_conversations(ModelRc::new(VecModel::from(items)));
    Ok(())
}

fn set_message_model(window: &MainWindow, messages: Vec<crate::storage::Message>) {
    let items = messages
        .into_iter()
        .map(|message| MessageListItem {
            id: message.id.into(),
            role: match message.role {
                MessageRole::User => "You",
                MessageRole::Assistant => "Assistant",
                MessageRole::System => "System",
            }
            .into(),
            content: message.content.into(),
        })
        .collect::<Vec<_>>();
    window.set_messages(ModelRc::new(VecModel::from(items)));
}

fn refresh_characters(
    window: &MainWindow,
    store: &crate::storage::NexusStore,
) -> Result<(), String> {
    let mut items = Vec::new();
    for character in store.list_characters()? {
        let references = store.character_references(&character.id)?;
        let reference_text = if references.is_empty() {
            String::from("No linked references")
        } else {
            references
                .iter()
                .map(|asset| asset.relative_path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        };
        items.push(CharacterListItem {
            id: character.id.into(),
            name: character.name.into(),
            description: character.description.into(),
            notes: character.notes.unwrap_or_default().into(),
            references: reference_text.into(),
        });
    }
    window.set_characters(ModelRc::new(VecModel::from(items)));
    Ok(())
}

fn refresh_media(
    window: &MainWindow,
    store: &crate::storage::NexusStore,
    paths: &crate::paths::PortalPaths,
) -> Result<(), String> {
    let jobs = store.list_jobs()?;
    let mut items = Vec::new();
    for asset in store.list_media_assets()? {
        let absolute_path = paths.absolute_media_path(&asset.relative_path)?;
        let preview = if asset.kind == MediaKind::Image {
            Image::load_from_path(&absolute_path).unwrap_or_default()
        } else {
            Image::default()
        };
        let job = asset
            .generation_job_id
            .as_deref()
            .and_then(|job_id| jobs.iter().find(|candidate| candidate.id == job_id));
        let job_status = job
            .map(|job| format!("Generation job: {}", job.status.as_str()))
            .unwrap_or_else(|| String::from("Local import"));
        let prompt_summary = job
            .map(|job| summarize_prompt(&job.user_idea))
            .unwrap_or_default();
        items.push(MediaListItem {
            id: asset.id.into(),
            kind: asset.kind.to_string().into(),
            path: absolute_path.display().to_string().into(),
            details: asset
                .model_id
                .unwrap_or_else(|| format!("{:?}", asset.source))
                .into(),
            preview,
            job_status: job_status.into(),
            prompt_summary: prompt_summary.into(),
        });
    }
    window.set_media_assets(ModelRc::new(VecModel::from(items)));
    Ok(())
}

fn summarize_prompt(prompt: &str) -> String {
    const MAX_CHARS: usize = 140;
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_string();
    }
    format!("{}…", trimmed.chars().take(MAX_CHARS).collect::<String>())
}

fn refresh_jobs(window: &MainWindow, store: &crate::storage::NexusStore) -> Result<(), String> {
    let items = store
        .list_jobs()?
        .into_iter()
        .map(|job| {
            let model = find_model(&job.model_id)
                .map(|model| model.display_name)
                .unwrap_or(&job.model_id);
            let details = job
                .error
                .clone()
                .or_else(|| job_cost_estimate(&job))
                .unwrap_or_else(|| job.user_idea.clone());
            JobListItem {
                id: job.id.into(),
                status: job.status.as_str().into(),
                model: model.into(),
                details: details.into(),
            }
        })
        .collect::<Vec<_>>();
    window.set_jobs(ModelRc::new(VecModel::from(items)));
    Ok(())
}

fn job_cost_estimate(job: &crate::storage::GenerationJob) -> Option<String> {
    let rate = job.advertised_hourly_rate_usd?;
    let milliseconds = job.execution_time_ms?;
    let estimate = rate * milliseconds as f64 / 3_600_000.0;
    Some(format!(
        "Estimated compute ${estimate:.5} from {:.1}s at ${rate:.4}/hour (rate snapshot; excludes storage).",
        milliseconds as f64 / 1000.0
    ))
}

fn refresh_settings(window: &MainWindow, state: &AppState) {
    window.set_storage_path(state.paths.nexus_dir().display().to_string().into());
    window.set_prompt_endpoint_id(state.settings.runpod.prompt_endpoint_id.clone().into());
    window.set_image_endpoint_id(state.settings.runpod.image_endpoint_id.clone().into());
    window.set_video_endpoint_id(state.settings.runpod.video_endpoint_id.clone().into());
    window.set_local_http_url(state.settings.prompt_compiler.local_http_url.clone().into());
    window.set_prompt_compiler_index(match state.settings.prompt_compiler.mode {
        PromptCompilerMode::Manual => 0,
        PromptCompilerMode::LocalHttp => 1,
        PromptCompilerMode::RunPod => 2,
    });
    let status = match state.api_key.source {
        ApiKeySource::Keyring => "Configured in operating system keyring",
        ApiKeySource::Environment => "Configured through RUNPOD_API_KEY",
        ApiKeySource::Missing => "Not configured",
    };
    window.set_runpod_status(status.into());
    window.set_worker_image(FLUX_WORKER_IMAGE.into());
    if let Ok(setup) = state.setup_session.lock() {
        window.set_setup_state_label(setup.state.label().into());
        window.set_setup_stage(setup.state.ui_index());
        set_diagnostic_model(window, &setup.diagnostics);
    }
    window.set_volume_resource(
        match (
            state.provisioning_state.network_volume_name.as_deref(),
            state.provisioning_state.network_volume_size_gb,
            state.provisioning_state.data_center_id.as_deref(),
            state.provisioning_state.network_volume_id.as_deref(),
        ) {
            (Some(name), Some(size), Some(data_center), Some(id)) => {
                format!("{name} · {size} GB · {data_center} · ID {id}")
            }
            _ => String::from("Not configured"),
        }
        .into(),
    );
    window.set_endpoint_resource(
        match (
            state.provisioning_state.image_endpoint_name.as_deref(),
            state.provisioning_state.image_endpoint_id.as_deref(),
            state.provisioning_state.gpu_type_id.as_deref(),
            state.provisioning_state.last_verified_unix,
        ) {
            (Some(name), Some(id), Some(gpu), Some(verified)) => {
                format!("{name} · {gpu} · last verified Unix {verified} · min 0 / max 1 · ID {id}")
            }
            _ => String::from("Not configured"),
        }
        .into(),
    );
}

fn resolve_prompt(state: &AppState, prompt: &str) -> Result<String, String> {
    if !prompt.trim().is_empty() {
        return Ok(prompt.trim().to_string());
    }
    if let Some(conversation_id) = &state.current_conversation_id {
        let messages = state.nexus_store.list_messages(conversation_id)?;
        if let Some(message) = messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
        {
            return Ok(message.content.clone());
        }
    }
    Err(String::from(
        "Enter a manual prompt or send a chat message first.",
    ))
}

fn parse_dimensions(value: &str) -> Result<Dimensions, String> {
    let (width, height) = value
        .trim()
        .split_once('x')
        .ok_or_else(|| String::from("Dimensions must look like 1024x1024."))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| String::from("Width must be a whole number."))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| String::from("Height must be a whole number."))?;
    if width == 0 || height == 0 {
        return Err(String::from("Dimensions must be greater than zero."));
    }
    Ok(Dimensions { width, height })
}

fn optional_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn plan_summary(plan: &crate::provisioning::ProvisioningPlan) -> String {
    let network_volume = plan
        .actions
        .iter()
        .find_map(|action| match action {
            ProvisioningAction::ReuseNetworkVolume {
                name,
                size_gb,
                data_center_id,
                ..
            } => Some(format!(
                "Action: Reuse\nName: {name}\nSize: {size_gb} GB\nData center: {data_center_id}"
            )),
            ProvisioningAction::CreateNetworkVolume(request) => Some(format!(
                "Action: Create\nName: {}\nSize: {} GB\nData center: {}",
                request.name, request.size, request.data_center
            )),
            _ => None,
        })
        .unwrap_or_else(|| String::from("No Network Volume action"));
    let endpoint = plan
        .actions
        .iter()
        .find_map(|action| match action {
            ProvisioningAction::ReuseImageEndpoint { name, .. } => {
                Some(format!("Action: Reuse\nName: {name}"))
            }
            ProvisioningAction::CreateImageEndpoint => Some(format!(
                "Action: Create\nName: {}",
                crate::provisioning::NEXUS_IMAGE_ENDPOINT_NAME
            )),
            ProvisioningAction::UpdateImageEndpoint { id } => Some(format!(
                "Action: Update\nName: {}\nExisting ID: {id}",
                crate::provisioning::NEXUS_IMAGE_ENDPOINT_NAME
            )),
            _ => None,
        })
        .unwrap_or_else(|| String::from("No endpoint action"));
    let serverless_rate = plan
        .selected_gpu
        .serverless_usd_per_hour
        .map(|rate| format!("Current advertised Serverless rate: ${rate:.4}/hour"))
        .unwrap_or_else(|| {
            String::from("Serverless rate is not exposed by the current RunPod REST API v2.")
        });
    format!(
        "NETWORK VOLUME\n{network_volume}\n\nSERVERLESS ENDPOINT\n{endpoint}\nGPU: {}\nVRAM: {} GB\nAvailability: {}\nData center: {}\nWorkers: min 0 / max 1\nWorker image: {}\n\nCOST INFORMATION\n{}\nSecure Pod reference rate: ${:.4}/hour\nCommunity Pod reference rate: ${:.4}/hour\nPod rates are reference only; they are not Serverless prices.\nPersistent volume: billable while it exists.\n\n{}",
        plan.selected_gpu.display_name,
        plan.selected_gpu.memory_gb,
        plan.selected_gpu.availability,
        plan.selected_gpu.data_center_id,
        FLUX_WORKER_IMAGE,
        serverless_rate,
        plan.selected_gpu.secure_pod_reference_usd_per_hour,
        plan.selected_gpu.community_pod_reference_usd_per_hour,
        plan.cost_note
    )
}

fn cost_summary(plan: &crate::provisioning::ProvisioningPlan) -> String {
    let spend = plan
        .actual_recent_spend_usd
        .map(|value| format!("Actual recent spend reported by RunPod: ${value:.4}. "))
        .unwrap_or_else(|| String::from("RunPod returned no recent billing total. "));
    format!(
        "{spend}Current account balance is not exposed by the available RunPod REST API v2. Serverless rate is also unavailable, so a per-image cost estimate cannot be calculated from current catalog data."
    )
}

fn conversation_title(content: &str) -> &str {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        "New conversation"
    } else {
        trimmed
    }
}

fn set_status(weak: &slint::Weak<MainWindow>, message: String) {
    if let Some(window) = weak.upgrade() {
        window.set_nexus_status(message.into());
    }
}

fn set_media_status(weak: &slint::Weak<MainWindow>, message: &str) {
    if let Some(window) = weak.upgrade() {
        window.set_media_status(message.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provisioning::{ProvisioningPlan, ProvisioningState, SelectedGpu};

    #[test]
    fn plan_ui_does_not_present_pod_pricing_as_a_serverless_rate() {
        let plan = ProvisioningPlan {
            actions: Vec::new(),
            selected_gpu: SelectedGpu {
                type_id: String::from("NVIDIA L40S"),
                display_name: String::from("L40S"),
                pool_id: String::from("ADA_48"),
                memory_gb: 48,
                serverless_usd_per_hour: None,
                secure_pod_reference_usd_per_hour: 0.8,
                community_pod_reference_usd_per_hour: 0.6,
                data_center_id: String::from("EU-1"),
                availability: String::from("HIGH"),
            },
            volume_size_gb: 150,
            actual_recent_spend_usd: Some(4.5),
            cost_note: String::from("Persistent storage is billable."),
        };

        let summary = plan_summary(&plan);
        assert!(summary.contains("Serverless rate is not exposed"));
        assert!(summary.contains("Secure Pod reference rate: $0.8000/hour"));
        assert!(summary.contains("not Serverless prices"));
        assert!(!summary.contains("Serverless rate: $0.0000"));
    }

    #[test]
    fn managed_image_endpoint_overrides_manual_fallback() {
        let provisioned = ProvisioningState {
            image_endpoint_id: Some(String::from("managed-flux")),
            ..ProvisioningState::default()
        };
        let mut settings = crate::settings::PortalSettings::default();
        settings.runpod.image_endpoint_id = String::from("manual-fallback");
        assert_eq!(
            generation_endpoint_id(MediaKind::Image, &provisioned, &settings),
            "managed-flux"
        );
        assert_eq!(
            generation_endpoint_id(MediaKind::Image, &ProvisioningState::default(), &settings,),
            "manual-fallback"
        );
    }
}
