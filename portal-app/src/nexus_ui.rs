use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::app_state::AppState;
use crate::generation::{GenerationIntent, GenerationMode, GenerationParameters, MediaKind};
use crate::media_library::MediaLibrary;
use crate::model_catalog::{find_model, validate_model_mode};
use crate::prompt::PromptCompilerMode;
use crate::runpod::{RunPodCommand, RunPodEvent, RunPodWorker};
use crate::settings::{ApiKeySource, load_runpod_api_key, save_runpod_api_key};
use crate::storage::{CharacterReferenceRole, JobStatus, MessageRole};
use crate::worker_contract::{Dimensions, MediaWorkerRequest};
use crate::{
    CharacterListItem, ConversationListItem, JobListItem, MainWindow, MediaListItem,
    MessageListItem,
};

pub fn setup(window: &MainWindow, state: Rc<RefCell<AppState>>) -> Result<(), String> {
    setup_background_worker(window, Rc::clone(&state));
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
    let worker = RunPodWorker::start(move |event| {
        // Network work finishes on the worker thread. SQLite is updated there,
        // then `upgrade_in_event_loop` moves only a small owned closure back to
        // Slint's UI thread; Slint widgets are never touched from this thread.
        let (job_id, status_text) = match event {
            RunPodEvent::Submitted {
                local_job_id,
                remote_job_id,
                remote_status,
            } => {
                let result = store.update_job(
                    &local_job_id,
                    JobStatus::Queued,
                    Some(&remote_job_id),
                    None,
                    None,
                );
                let text = match result {
                    Ok(()) => format!("Queued on RunPod ({remote_status})."),
                    Err(error) => error,
                };
                (local_job_id, text)
            }
            RunPodEvent::Failed {
                local_job_id,
                error,
            } => {
                let _ =
                    store.update_job(&local_job_id, JobStatus::Failed, None, None, Some(&error));
                (local_job_id, error)
            }
        };
        let ui_store = store.clone();
        let _ = weak_window.upgrade_in_event_loop(move |window| {
            window.set_nexus_status(status_text.into());
            window.set_compute_status(format!("Updated job {job_id}.").into());
            if let Err(error) = refresh_jobs(&window, &ui_store) {
                window.set_compute_status(error.into());
            }
        });
    });
    state.borrow_mut().runpod_worker = Some(worker);
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
                    let _ = refresh_media(&window, &state.nexus_store);
                }
                Err(error) => window.set_media_status(error.into()),
            }
        }
    });

    let weak = window.as_weak();
    window.on_select_reference_media(move |id| {
        let id = id.to_string();
        let store = state.borrow().nexus_store.clone();
        match store.media_asset(&id) {
            Ok(Some(asset)) => {
                state.borrow_mut().selected_reference_media_id = Some(id.clone());
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
    let endpoint_id = match mode.output_kind() {
        MediaKind::Image => &borrowed.settings.runpod.image_endpoint_id,
        MediaKind::Video => &borrowed.settings.runpod.video_endpoint_id,
        MediaKind::Audio => unreachable!("generation modes do not directly output audio"),
    };
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
    let endpoint_id = endpoint_id.clone();
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
    store.update_job(&job.id, JobStatus::Preparing, None, None, None)?;
    worker.send(RunPodCommand::Submit {
        local_job_id: job.id.clone(),
        api_key,
        endpoint_id,
        request,
    })?;
    Ok(job.id)
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
    refresh_media(window, &state.nexus_store)?;
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

fn refresh_media(window: &MainWindow, store: &crate::storage::NexusStore) -> Result<(), String> {
    let items = store
        .list_media_assets()?
        .into_iter()
        .map(|asset| MediaListItem {
            id: asset.id.into(),
            kind: asset.kind.to_string().into(),
            path: asset.relative_path.display().to_string().into(),
            details: asset
                .model_id
                .unwrap_or_else(|| format!("{:?}", asset.source))
                .into(),
        })
        .collect::<Vec<_>>();
    window.set_media_assets(ModelRc::new(VecModel::from(items)));
    Ok(())
}

fn refresh_jobs(window: &MainWindow, store: &crate::storage::NexusStore) -> Result<(), String> {
    let items = store
        .list_jobs()?
        .into_iter()
        .map(|job| {
            let model = find_model(&job.model_id)
                .map(|model| model.display_name)
                .unwrap_or(&job.model_id);
            JobListItem {
                id: job.id.into(),
                status: job.status.as_str().into(),
                model: model.into(),
                details: job.error.unwrap_or(job.user_idea).into(),
            }
        })
        .collect::<Vec<_>>();
    window.set_jobs(ModelRc::new(VecModel::from(items)));
    Ok(())
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
