#![allow(
    dead_code,
    reason = "schema vocabulary includes reference roles for future UI choices"
)]

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::generation::{GenerationMode, MediaKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Character {
    pub id: String,
    pub name: String,
    pub description: String,
    pub notes: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterReferenceRole {
    Portrait,
    FullBody,
    Style,
    Other,
}

impl CharacterReferenceRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::FullBody => "full_body",
            Self::Style => "style",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaSource {
    Imported,
    Generated,
    RemoteResult,
}

impl MediaSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Generated => "generated",
            Self::RemoteResult => "remote_result",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "imported" => Ok(Self::Imported),
            "generated" => Ok(Self::Generated),
            "remote_result" => Ok(Self::RemoteResult),
            _ => Err(format!("Unknown media source '{value}'.")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaAsset {
    pub id: String,
    pub kind: MediaKind,
    pub relative_path: PathBuf,
    pub source: MediaSource,
    pub generation_job_id: Option<String>,
    pub model_id: Option<String>,
    pub created_at: i64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_seconds: Option<f64>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewMediaAsset {
    pub id: String,
    pub kind: MediaKind,
    pub relative_path: PathBuf,
    pub source: MediaSource,
    pub generation_job_id: Option<String>,
    pub model_id: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_seconds: Option<f64>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub id: String,
    pub title: Option<String>,
    pub project_folder: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl MessageRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            _ => Err(format!("Unknown message role '{value}'.")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub role: MessageRole,
    pub content: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Draft,
    Preparing,
    Queued,
    Running,
    Downloading,
    Completed,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Preparing => "preparing",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Downloading => "downloading",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "draft" => Ok(Self::Draft),
            "preparing" => Ok(Self::Preparing),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "downloading" => Ok(Self::Downloading),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("Unknown job status '{value}'.")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GenerationJob {
    pub id: String,
    pub mode: GenerationMode,
    pub model_id: String,
    pub provider: String,
    pub status: JobStatus,
    pub user_idea: String,
    pub compiled_prompt: Option<String>,
    pub seed: Option<u64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub error: Option<String>,
    pub output_media_id: Option<String>,
    pub remote_job_id: Option<String>,
    pub gpu_profile: Option<String>,
    pub advertised_hourly_rate_usd: Option<f64>,
    pub execution_time_ms: Option<u64>,
}

/// `NexusStore` owns only a database path, not a long-lived SQLite connection.
/// Each short operation opens its own connection. This keeps connection
/// lifetimes obvious and lets a background thread use a cloned store safely
/// without wrapping `rusqlite::Connection` in shared mutable state.
#[derive(Debug, Clone)]
pub struct NexusStore {
    database_path: PathBuf,
}

impl NexusStore {
    pub fn open(database_path: impl Into<PathBuf>) -> Result<Self, String> {
        let store = Self {
            database_path: database_path.into(),
        };
        store.initialize_schema()?;
        Ok(store)
    }

    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection = Connection::open(&self.database_path).map_err(|error| {
            format!(
                "Could not open Nexus database '{}': {error}",
                self.database_path.display()
            )
        })?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|error| format!("Could not enable SQLite foreign keys: {error}"))?;
        Ok(connection)
    }

    fn initialize_schema(&self) -> Result<(), String> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not begin schema transaction: {error}"))?;

        // Schema changes stay as small ordered SQL migrations rather than an
        // ORM. Version 2 adds cost-estimation inputs; it still stores metadata
        // only, never generated media bytes.
        transaction
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_version (
                    version INTEGER NOT NULL
                );
                INSERT INTO schema_version(version)
                    SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version);

                CREATE TABLE IF NOT EXISTS characters (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    description TEXT NOT NULL,
                    notes TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS media_assets (
                    id TEXT PRIMARY KEY,
                    media_kind TEXT NOT NULL,
                    relative_path TEXT NOT NULL UNIQUE,
                    source TEXT NOT NULL,
                    generation_job_id TEXT,
                    model_id TEXT,
                    created_at INTEGER NOT NULL,
                    width INTEGER,
                    height INTEGER,
                    duration_seconds REAL,
                    mime_type TEXT
                );

                CREATE TABLE IF NOT EXISTS character_references (
                    character_id TEXT NOT NULL REFERENCES characters(id) ON DELETE CASCADE,
                    media_asset_id TEXT NOT NULL REFERENCES media_assets(id) ON DELETE CASCADE,
                    reference_role TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    PRIMARY KEY(character_id, media_asset_id, reference_role)
                );

                CREATE TABLE IF NOT EXISTS conversations (
                    id TEXT PRIMARY KEY,
                    title TEXT,
                    project_folder TEXT,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id TEXT PRIMARY KEY,
                    conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
                    role TEXT NOT NULL,
                    content TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS generation_jobs (
                    id TEXT PRIMARY KEY,
                    generation_mode TEXT NOT NULL,
                    model_id TEXT NOT NULL,
                    provider TEXT NOT NULL,
                    status TEXT NOT NULL,
                    user_idea TEXT NOT NULL,
                    compiled_prompt TEXT,
                    seed INTEGER,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    error TEXT,
                    output_media_id TEXT REFERENCES media_assets(id),
                    remote_job_id TEXT
                );

                CREATE INDEX IF NOT EXISTS messages_by_conversation
                    ON messages(conversation_id, created_at);
                CREATE INDEX IF NOT EXISTS jobs_by_updated_at
                    ON generation_jobs(updated_at DESC);",
            )
            .map_err(|error| format!("Could not initialize Nexus schema: {error}"))?;

        let version: i64 = transaction
            .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            })
            .map_err(|error| format!("Could not read Nexus schema version: {error}"))?;
        if version == 1 {
            transaction
                .execute_batch(
                    "ALTER TABLE generation_jobs ADD COLUMN gpu_profile TEXT;
                     ALTER TABLE generation_jobs ADD COLUMN advertised_hourly_rate_usd REAL;
                     ALTER TABLE generation_jobs ADD COLUMN execution_time_ms INTEGER;
                     UPDATE schema_version SET version=2;",
                )
                .map_err(|error| format!("Could not migrate Nexus schema to version 2: {error}"))?;
        } else if version != 2 {
            return Err(format!(
                "Nexus database schema version {version} is not supported by this Portal build."
            ));
        }

        transaction
            .commit()
            .map_err(|error| format!("Could not commit Nexus schema: {error}"))
    }

    pub fn create_character(
        &self,
        name: &str,
        description: &str,
        notes: Option<&str>,
    ) -> Result<Character, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(String::from("Character name must not be empty."));
        }

        let now = now_timestamp()?;
        let character = Character {
            id: Uuid::new_v4().to_string(),
            name: name.to_string(),
            description: description.trim().to_string(),
            notes: non_empty(notes),
            created_at: now,
            updated_at: now,
        };
        self.connection()?
            .execute(
                "INSERT INTO characters(id, name, description, notes, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    character.id,
                    character.name,
                    character.description,
                    character.notes,
                    character.created_at,
                    character.updated_at
                ],
            )
            .map_err(|error| format!("Could not create character: {error}"))?;
        Ok(character)
    }

    pub fn update_character(
        &self,
        id: &str,
        name: &str,
        description: &str,
        notes: Option<&str>,
    ) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err(String::from("Character name must not be empty."));
        }
        let changed = self
            .connection()?
            .execute(
                "UPDATE characters SET name=?2, description=?3, notes=?4, updated_at=?5 WHERE id=?1",
                params![id, name.trim(), description.trim(), non_empty(notes), now_timestamp()?],
            )
            .map_err(|error| format!("Could not update character: {error}"))?;
        if changed == 0 {
            return Err(format!("Character '{id}' was not found."));
        }
        Ok(())
    }

    pub fn list_characters(&self) -> Result<Vec<Character>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, description, notes, created_at, updated_at
                 FROM characters ORDER BY lower(name)",
            )
            .map_err(|error| format!("Could not prepare character query: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(Character {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    notes: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .map_err(|error| format!("Could not query characters: {error}"))?;
        collect_rows(rows, "characters")
    }

    pub fn insert_media_asset(&self, new_asset: &NewMediaAsset) -> Result<MediaAsset, String> {
        if new_asset.relative_path.is_absolute()
            || new_asset
                .relative_path
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err(String::from(
                "Media metadata requires a safe relative path.",
            ));
        }
        let created_at = now_timestamp()?;
        let path_text = new_asset.relative_path.to_string_lossy().to_string();
        self.connection()?
            .execute(
                "INSERT INTO media_assets(
                    id, media_kind, relative_path, source, generation_job_id, model_id,
                    created_at, width, height, duration_seconds, mime_type
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    new_asset.id,
                    new_asset.kind.to_string(),
                    path_text,
                    new_asset.source.as_str(),
                    new_asset.generation_job_id,
                    new_asset.model_id,
                    created_at,
                    new_asset.width,
                    new_asset.height,
                    new_asset.duration_seconds,
                    new_asset.mime_type,
                ],
            )
            .map_err(|error| format!("Could not save media metadata: {error}"))?;
        Ok(MediaAsset {
            id: new_asset.id.clone(),
            kind: new_asset.kind,
            relative_path: new_asset.relative_path.clone(),
            source: new_asset.source,
            generation_job_id: new_asset.generation_job_id.clone(),
            model_id: new_asset.model_id.clone(),
            created_at,
            width: new_asset.width,
            height: new_asset.height,
            duration_seconds: new_asset.duration_seconds,
            mime_type: new_asset.mime_type.clone(),
        })
    }

    pub fn media_asset(&self, id: &str) -> Result<Option<MediaAsset>, String> {
        let connection = self.connection()?;
        let raw = connection
            .query_row(
                "SELECT id, media_kind, relative_path, source, generation_job_id, model_id,
                        created_at, width, height, duration_seconds, mime_type
                 FROM media_assets WHERE id=?1",
                [id],
                media_row,
            )
            .optional()
            .map_err(|error| format!("Could not load media asset: {error}"))?;
        raw.map(parse_media_row).transpose()
    }

    pub fn list_media_assets(&self) -> Result<Vec<MediaAsset>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, media_kind, relative_path, source, generation_job_id, model_id,
                        created_at, width, height, duration_seconds, mime_type
                 FROM media_assets ORDER BY created_at DESC, id DESC",
            )
            .map_err(|error| format!("Could not prepare media query: {error}"))?;
        let rows = statement
            .query_map([], media_row)
            .map_err(|error| format!("Could not query media: {error}"))?;
        let raw = collect_rows(rows, "media")?;
        raw.into_iter().map(parse_media_row).collect()
    }

    pub fn attach_character_reference(
        &self,
        character_id: &str,
        media_asset_id: &str,
        role: CharacterReferenceRole,
    ) -> Result<(), String> {
        let asset = self
            .media_asset(media_asset_id)?
            .ok_or_else(|| format!("Media asset '{media_asset_id}' was not found."))?;
        if asset.kind != MediaKind::Image {
            return Err(String::from(
                "Only image assets can be character references.",
            ));
        }

        let mut connection = self.connection()?;
        // Linking the media and touching the character timestamp are one
        // logical operation, so a transaction ensures both changes commit or
        // neither does if an error occurs.
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not begin reference transaction: {error}"))?;
        let now = now_timestamp()?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO character_references(
                    character_id, media_asset_id, reference_role, created_at
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![character_id, media_asset_id, role.as_str(), now],
            )
            .map_err(|error| format!("Could not attach character reference: {error}"))?;
        transaction
            .execute(
                "UPDATE characters SET updated_at=?2 WHERE id=?1",
                params![character_id, now],
            )
            .map_err(|error| format!("Could not update character timestamp: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit character reference: {error}"))
    }

    pub fn remove_character_reference(
        &self,
        character_id: &str,
        media_asset_id: &str,
    ) -> Result<(), String> {
        // Only the association is deleted. The media_assets row and its file
        // remain intact so other characters and generations can still use it.
        self.connection()?
            .execute(
                "DELETE FROM character_references WHERE character_id=?1 AND media_asset_id=?2",
                params![character_id, media_asset_id],
            )
            .map_err(|error| format!("Could not remove character reference: {error}"))?;
        Ok(())
    }

    pub fn character_references(&self, character_id: &str) -> Result<Vec<MediaAsset>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT m.id, m.media_kind, m.relative_path, m.source, m.generation_job_id,
                        m.model_id, m.created_at, m.width, m.height, m.duration_seconds, m.mime_type
                 FROM media_assets m
                 JOIN character_references r ON r.media_asset_id=m.id
                 WHERE r.character_id=?1 ORDER BY r.created_at",
            )
            .map_err(|error| format!("Could not prepare reference query: {error}"))?;
        let rows = statement
            .query_map([character_id], media_row)
            .map_err(|error| format!("Could not query references: {error}"))?;
        let raw = collect_rows(rows, "character references")?;
        raw.into_iter().map(parse_media_row).collect()
    }

    pub fn create_conversation(
        &self,
        title: Option<&str>,
        project_folder: Option<&str>,
    ) -> Result<Conversation, String> {
        let now = now_timestamp()?;
        let conversation = Conversation {
            id: Uuid::new_v4().to_string(),
            title: non_empty(title),
            project_folder: non_empty(project_folder),
            created_at: now,
            updated_at: now,
        };
        self.connection()?
            .execute(
                "INSERT INTO conversations(id, title, project_folder, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    conversation.id,
                    conversation.title,
                    conversation.project_folder,
                    conversation.created_at,
                    conversation.updated_at
                ],
            )
            .map_err(|error| format!("Could not create conversation: {error}"))?;
        Ok(conversation)
    }

    pub fn list_conversations(&self) -> Result<Vec<Conversation>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, title, project_folder, created_at, updated_at
                 FROM conversations ORDER BY updated_at DESC, id DESC",
            )
            .map_err(|error| format!("Could not prepare conversation query: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    project_folder: row.get(2)?,
                    created_at: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            })
            .map_err(|error| format!("Could not query conversations: {error}"))?;
        collect_rows(rows, "conversations")
    }

    pub fn add_message(
        &self,
        conversation_id: &str,
        role: MessageRole,
        content: &str,
    ) -> Result<Message, String> {
        let content = content.trim();
        if content.is_empty() {
            return Err(String::from("Message must not be empty."));
        }
        let message = Message {
            id: Uuid::new_v4().to_string(),
            conversation_id: conversation_id.to_string(),
            role,
            content: content.to_string(),
            created_at: now_timestamp()?,
        };
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("Could not begin message transaction: {error}"))?;
        transaction
            .execute(
                "INSERT INTO messages(id, conversation_id, role, content, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    message.id,
                    message.conversation_id,
                    message.role.as_str(),
                    message.content,
                    message.created_at
                ],
            )
            .map_err(|error| format!("Could not save message: {error}"))?;
        transaction
            .execute(
                "UPDATE conversations SET updated_at=?2 WHERE id=?1",
                params![conversation_id, message.created_at],
            )
            .map_err(|error| format!("Could not update conversation: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("Could not commit message: {error}"))?;
        Ok(message)
    }

    pub fn list_messages(&self, conversation_id: &str) -> Result<Vec<Message>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, conversation_id, role, content, created_at
                 FROM messages WHERE conversation_id=?1 ORDER BY created_at, id",
            )
            .map_err(|error| format!("Could not prepare message query: {error}"))?;
        let rows = statement
            .query_map([conversation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|error| format!("Could not query messages: {error}"))?;
        let raw = collect_rows(rows, "messages")?;
        raw.into_iter()
            .map(|(id, conversation_id, role, content, created_at)| {
                Ok(Message {
                    id,
                    conversation_id,
                    role: MessageRole::parse(&role)?,
                    content,
                    created_at,
                })
            })
            .collect()
    }

    pub fn create_job(
        &self,
        mode: GenerationMode,
        model_id: &str,
        user_idea: &str,
        compiled_prompt: Option<&str>,
        seed: Option<u64>,
    ) -> Result<GenerationJob, String> {
        let now = now_timestamp()?;
        let job = GenerationJob {
            id: Uuid::new_v4().to_string(),
            mode,
            model_id: model_id.to_string(),
            provider: String::from("runpod"),
            status: JobStatus::Draft,
            user_idea: user_idea.to_string(),
            compiled_prompt: non_empty(compiled_prompt),
            seed,
            created_at: now,
            updated_at: now,
            error: None,
            output_media_id: None,
            remote_job_id: None,
            gpu_profile: None,
            advertised_hourly_rate_usd: None,
            execution_time_ms: None,
        };
        let seed_i64 = seed.map(seed_to_i64).transpose()?;
        self.connection()?
            .execute(
                "INSERT INTO generation_jobs(
                    id, generation_mode, model_id, provider, status, user_idea,
                    compiled_prompt, seed, created_at, updated_at, error,
                    output_media_id, remote_job_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    job.id,
                    job.mode.to_string(),
                    job.model_id,
                    job.provider,
                    job.status.as_str(),
                    job.user_idea,
                    job.compiled_prompt,
                    seed_i64,
                    job.created_at,
                    job.updated_at,
                    job.error,
                    job.output_media_id,
                    job.remote_job_id,
                ],
            )
            .map_err(|error| format!("Could not create generation job: {error}"))?;
        Ok(job)
    }

    pub fn update_job(
        &self,
        id: &str,
        status: JobStatus,
        remote_job_id: Option<&str>,
        output_media_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<(), String> {
        let changed = self
            .connection()?
            .execute(
                "UPDATE generation_jobs SET status=?2, remote_job_id=COALESCE(?3, remote_job_id),
                    output_media_id=COALESCE(?4, output_media_id), error=?5, updated_at=?6
                 WHERE id=?1",
                params![
                    id,
                    status.as_str(),
                    remote_job_id,
                    output_media_id,
                    error,
                    now_timestamp()?
                ],
            )
            .map_err(|error| format!("Could not update generation job: {error}"))?;
        if changed == 0 {
            return Err(format!("Generation job '{id}' was not found."));
        }
        Ok(())
    }

    pub fn update_job_metrics(
        &self,
        id: &str,
        gpu_profile: Option<&str>,
        advertised_hourly_rate_usd: Option<f64>,
        execution_time_ms: Option<u64>,
    ) -> Result<(), String> {
        let execution_time_ms = execution_time_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| String::from("Execution time is too large for local storage."))?;
        let changed = self
            .connection()?
            .execute(
                "UPDATE generation_jobs SET gpu_profile=COALESCE(?2, gpu_profile),
                    advertised_hourly_rate_usd=COALESCE(?3, advertised_hourly_rate_usd),
                    execution_time_ms=COALESCE(?4, execution_time_ms), updated_at=?5
                 WHERE id=?1",
                params![
                    id,
                    gpu_profile,
                    advertised_hourly_rate_usd,
                    execution_time_ms,
                    now_timestamp()?
                ],
            )
            .map_err(|error| format!("Could not update generation job metrics: {error}"))?;
        if changed == 0 {
            return Err(format!("Generation job '{id}' was not found."));
        }
        Ok(())
    }

    pub fn list_jobs(&self) -> Result<Vec<GenerationJob>, String> {
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT id, generation_mode, model_id, provider, status, user_idea,
                        compiled_prompt, seed, created_at, updated_at, error,
                        output_media_id, remote_job_id, gpu_profile,
                        advertised_hourly_rate_usd, execution_time_ms
                 FROM generation_jobs ORDER BY updated_at DESC, id DESC",
            )
            .map_err(|error| format!("Could not prepare job query: {error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<f64>>(14)?,
                    row.get::<_, Option<i64>>(15)?,
                ))
            })
            .map_err(|error| format!("Could not query jobs: {error}"))?;
        let raw = collect_rows(rows, "generation jobs")?;
        raw.into_iter()
            .map(|row| {
                Ok(GenerationJob {
                    id: row.0,
                    mode: GenerationMode::from_str(&row.1)?,
                    model_id: row.2,
                    provider: row.3,
                    status: JobStatus::parse(&row.4)?,
                    user_idea: row.5,
                    compiled_prompt: row.6,
                    seed: row.7.map(|value| value as u64),
                    created_at: row.8,
                    updated_at: row.9,
                    error: row.10,
                    output_media_id: row.11,
                    remote_job_id: row.12,
                    gpu_profile: row.13,
                    advertised_hourly_rate_usd: row.14,
                    execution_time_ms: row.15.map(|value| value as u64),
                })
            })
            .collect()
    }

    pub fn non_terminal_remote_jobs(&self) -> Result<Vec<GenerationJob>, String> {
        Ok(self
            .list_jobs()?
            .into_iter()
            .filter(|job| {
                job.remote_job_id.is_some()
                    && !matches!(
                        job.status,
                        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled
                    )
            })
            .collect())
    }
}

type RawMediaRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    Option<u32>,
    Option<u32>,
    Option<f64>,
    Option<String>,
);

fn media_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMediaRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
    ))
}

fn parse_media_row(row: RawMediaRow) -> Result<MediaAsset, String> {
    Ok(MediaAsset {
        id: row.0,
        kind: MediaKind::from_str(&row.1)?,
        relative_path: PathBuf::from(row.2),
        source: MediaSource::parse(&row.3)?,
        generation_job_id: row.4,
        model_id: row.5,
        created_at: row.6,
        width: row.7,
        height: row.8,
        duration_seconds: row.9,
        mime_type: row.10,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    label: &str,
) -> Result<Vec<T>, String> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read {label}: {error}"))
}

fn now_timestamp() -> Result<i64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|error| format!("System clock is before the Unix epoch: {error}"))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn seed_to_i64(seed: u64) -> Result<i64, String> {
    i64::try_from(seed).map_err(|_| String::from("Seed is too large for local storage."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, NexusStore) {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let store = NexusStore::open(directory.path().join("nexus.sqlite3"))
            .expect("temporary database should open");
        (directory, store)
    }

    fn image(store: &NexusStore, name: &str) -> MediaAsset {
        store
            .insert_media_asset(&NewMediaAsset {
                id: Uuid::new_v4().to_string(),
                kind: MediaKind::Image,
                relative_path: PathBuf::from(format!("media/images/{name}.png")),
                source: MediaSource::Imported,
                generation_job_id: None,
                model_id: None,
                width: Some(512),
                height: Some(512),
                duration_seconds: None,
                mime_type: Some(String::from("image/png")),
            })
            .expect("media metadata should save")
    }

    #[test]
    fn character_create_load_and_update() {
        let (_directory, store) = store();
        let character = store
            .create_character("Sarah", "Detective", Some("Rainy Tokyo"))
            .expect("character should save");
        store
            .update_character(&character.id, "Sarah Ito", "Detective", None)
            .expect("character should update");
        let loaded = store.list_characters().expect("characters should load");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Sarah Ito");
        assert_eq!(loaded[0].notes, None);
    }

    #[test]
    fn character_can_have_multiple_reference_images() {
        let (_directory, store) = store();
        let character = store
            .create_character("Sarah", "", None)
            .expect("character should save");
        let portrait = image(&store, "portrait");
        let full_body = image(&store, "full-body");
        store
            .attach_character_reference(
                &character.id,
                &portrait.id,
                CharacterReferenceRole::Portrait,
            )
            .expect("portrait should attach");
        store
            .attach_character_reference(
                &character.id,
                &full_body.id,
                CharacterReferenceRole::FullBody,
            )
            .expect("full body should attach");
        assert_eq!(
            store
                .character_references(&character.id)
                .expect("references should load")
                .len(),
            2
        );
        store
            .remove_character_reference(&character.id, &portrait.id)
            .expect("association should be removed");
        assert!(
            store
                .media_asset(&portrait.id)
                .expect("lookup should work")
                .is_some()
        );
    }

    #[test]
    fn conversations_and_messages_survive_reopening() {
        let (directory, store) = store();
        let conversation = store
            .create_conversation(Some("Tokyo scene"), Some("my-project"))
            .expect("conversation should save");
        store
            .add_message(&conversation.id, MessageRole::User, "Make it cinematic.")
            .expect("message should save");
        drop(store);
        let reopened = NexusStore::open(directory.path().join("nexus.sqlite3"))
            .expect("database should reopen");
        let messages = reopened
            .list_messages(&conversation.id)
            .expect("messages should load");
        assert_eq!(messages[0].content, "Make it cinematic.");
    }

    #[test]
    fn media_paths_stay_relative() {
        let (_directory, store) = store();
        let asset = image(&store, "reference");
        let loaded = store
            .media_asset(&asset.id)
            .expect("lookup should work")
            .expect("asset should exist");
        assert_eq!(
            loaded.relative_path,
            PathBuf::from("media/images/reference.png")
        );
        assert!(!loaded.relative_path.is_absolute());
    }

    #[test]
    fn job_state_survives_reopening() {
        let (directory, store) = store();
        let job = store
            .create_job(
                GenerationMode::TextToImage,
                "flux-2-dev",
                "Rainy Tokyo",
                Some("Cinematic rainy Tokyo"),
                Some(42),
            )
            .expect("job should save");
        store
            .update_job(&job.id, JobStatus::Queued, Some("remote-123"), None, None)
            .expect("job should update");
        store
            .update_job_metrics(&job.id, Some("NVIDIA L40S"), Some(1.25), Some(12_000))
            .expect("job cost inputs should update");
        drop(store);
        let reopened = NexusStore::open(directory.path().join("nexus.sqlite3"))
            .expect("database should reopen");
        let loaded = reopened.list_jobs().expect("jobs should load");
        assert_eq!(loaded[0].status, JobStatus::Queued);
        assert_eq!(loaded[0].remote_job_id.as_deref(), Some("remote-123"));
        assert_eq!(loaded[0].seed, Some(42));
        assert_eq!(loaded[0].gpu_profile.as_deref(), Some("NVIDIA L40S"));
        assert_eq!(loaded[0].advertised_hourly_rate_usd, Some(1.25));
        assert_eq!(loaded[0].execution_time_ms, Some(12_000));
    }

    #[test]
    fn database_schema_has_no_api_key_column() {
        let (_directory, store) = store();
        let connection = store.connection().expect("database should open");
        let schema: String = connection
            .query_row(
                "SELECT COALESCE(group_concat(sql, ' '), '') FROM sqlite_master WHERE sql IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("schema should be readable");
        assert!(!schema.to_lowercase().contains("api_key"));
    }
}
