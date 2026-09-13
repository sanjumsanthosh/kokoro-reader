use blake3::Hasher;
use directories::BaseDirs;
use futures_util::{FutureExt, StreamExt};
use kokoro_en::{g2p, get_token_ids, split_sentences, KokoroTts, Voice};
use rmcp::{
    handler::server::wrapper::Parameters, tool, tool_router, transport::stdio,
    ErrorData as McpError, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    any::Any,
    backtrace::Backtrace,
    collections::BTreeSet,
    fs::{self, File, FileTimes, OpenOptions},
    io::Write,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, OnceLock},
    time::{Instant, SystemTime},
};
use tauri::{Emitter, Manager, State};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex as AsyncMutex;

const APP_ID: &str = "com.local.kokororeader";
const MODEL_VERSION: &str = "kokoro-82m-v1.0-fp32";
const SAMPLE_RATE: u32 = 24_000;
const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SENTENCE_BYTES: usize = 64 * 1024;
const MAX_KOKORO_TOKENS: usize = 510;
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const LOG_MAX_BYTES: u64 = 256 * 1024;
const LOG_BACKUP_COUNT: u8 = 2;
const SECTION_SEPARATOR: &str = "\n\n<!-- kokoro-reader-section -->\n\n";
const NARRATION_SEPARATOR: &str = "\n\n---\n\n";
const ACTIVE_PROJECT_FILE: &str = "active-project";
const MIGRATED_PROJECT_ID: &str = "migrated-reading";
const PROJECT_METADATA_FILE: &str = "metadata.json";
const PROJECT_CACHE_REFERENCES_FILE: &str = "cache-refs.json";
const AUDIO_CACHE_VERSION_MARKER: &str = ".cache-references-v1";

static CACHE_REFERENCES_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const MODEL_URL: &str =
    "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/onnx/model.onnx";
const MODEL_SHA256: &str = "8fbea51ea711f2af382e88c833d9e288c6dc82ce5e98421ea61c058ce21a34cb";

const VOICE_ASSETS: &[AssetSpec] = &[
    AssetSpec {
        name: "af_bella",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/voices/af_bella.bin",
        sha256: "f69d836209b78eb8c66e75e3cda491e26ea838a3674257e9d4e5703cbaf55c8b",
    },
    AssetSpec {
        name: "af_nicole",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/voices/af_nicole.bin",
        sha256: "cd2191ab31b914ed7b318416b0e4440fdf392ddad9106a060819aa600a64f59a",
    },
    AssetSpec {
        name: "am_fenrir",
        url: "https://huggingface.co/onnx-community/Kokoro-82M-v1.0-ONNX/resolve/main/voices/am_fenrir.bin",
        sha256: "c27989f741f7ee34d273a39d8a595cc0837d35f5ced9a29b7cc162614616df43",
    },
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpeechMode {
    Automatic,
    Custom,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentSection {
    pub markdown: String,
    pub speech_text: String,
    pub speech_mode: SpeechMode,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub title: String,
    pub sections: Vec<DocumentSection>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimeStatus {
    pub model_ready: bool,
    pub engine_ready: bool,
    pub downloading: bool,
    pub progress: Option<DownloadProgress>,
    pub backend: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DownloadProgress {
    pub asset: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AudioAsset {
    pub path: String,
    pub duration_ms: u64,
    pub cache_hit: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectSummary {
    pub project_id: String,
    pub title: String,
    pub updated_at: u64,
    pub active: bool,
    pub read: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProjectMetadata {
    pub read: bool,
    pub codex_url: Option<String>,
}

impl Default for ProjectMetadata {
    fn default() -> Self {
        Self {
            read: false,
            codex_url: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectDocument {
    pub project_id: String,
    pub document: Document,
    pub metadata: ProjectMetadata,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectLocation {
    pub project_id: String,
    pub title: String,
    pub directory: String,
    pub document_path: String,
    pub source_path: String,
    pub narration_path: String,
}

#[derive(Clone, Debug, Serialize)]
struct DeleteProjectResult {
    deleted_project_id: String,
    active_project_id: Option<String>,
    launched_or_refreshed: bool,
}

#[derive(Clone)]
pub struct AppState {
    document: Arc<Mutex<Document>>,
    runtime: Arc<Mutex<RuntimeStatus>>,
    engine: Arc<AsyncMutex<Option<Arc<KokoroTts>>>>,
    synthesis: Arc<AsyncMutex<()>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            document: Arc::new(Mutex::new(default_document())),
            runtime: Arc::new(Mutex::new(RuntimeStatus {
                model_ready: false,
                engine_ready: false,
                downloading: false,
                progress: None,
                backend: None,
                error: None,
            })),
            engine: Arc::new(AsyncMutex::new(None)),
            synthesis: Arc::new(AsyncMutex::new(())),
        }
    }
}

#[derive(Clone, Copy)]
struct AssetSpec {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
}

struct StoragePaths {
    data_dir: PathBuf,
    document_dir: PathBuf,
    model_dir: PathBuf,
    cache_dir: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IncomingSection {
    #[schemars(description = "Markdown to render for this visual section")]
    markdown: String,
    #[schemars(
        description = "Required narration-ready plain text for the section; automatic narration is disabled for MCP transfers"
    )]
    speech_text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendToReaderParams {
    #[schemars(description = "Required Codex backlink in the form codex://threads/<thread-id>")]
    codex_url: String,
    #[schemars(description = "Optional stable project ID returned by an earlier transfer")]
    project_id: Option<String>,
    #[schemars(description = "Optional document title")]
    title: Option<String>,
    #[schemars(description = "Ordered visual Markdown and matching narration sections")]
    sections: Vec<IncomingSection>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendFileToReaderParams {
    #[schemars(description = "Required Codex backlink in the form codex://threads/<thread-id>")]
    codex_url: String,
    #[schemars(description = "Optional stable project ID returned by an earlier transfer")]
    project_id: Option<String>,
    #[schemars(description = "Optional document title")]
    title: Option<String>,
    #[schemars(description = "Absolute path to source Markdown under ~/.kokoro_reader")]
    source_path: String,
    #[schemars(
        description = "Required absolute path to matching narration text under ~/.kokoro_reader; every section must be non-empty"
    )]
    narration_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PrecheckReaderFilesParams {
    #[schemars(description = "Absolute path to source Markdown under ~/.kokoro_reader")]
    source_path: String,
    #[schemars(description = "Absolute path to matching narration text under ~/.kokoro_reader")]
    narration_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AppendToReaderFilesParams {
    #[schemars(
        description = "Absolute path to staging source Markdown under ~/.kokoro_reader/inbox"
    )]
    source_path: String,
    #[schemars(
        description = "Absolute path to matching staging narration text under ~/.kokoro_reader/inbox"
    )]
    narration_path: String,
    #[schemars(description = "One or more aligned Markdown and narration sections to append")]
    sections: Vec<IncomingSection>,
    #[schemars(
        description = "Optional count of sections already in the staging files; mismatches reject the append"
    )]
    expected_section_count: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectIdParams {
    #[schemars(description = "Stable project ID")]
    project_id: String,
}

#[derive(Clone, Debug, Serialize)]
struct TransferResult {
    project_id: String,
    directory: String,
    accepted_sections: usize,
    title: String,
    launched_or_refreshed: bool,
    automatic_narration_sections: usize,
}

#[derive(Clone, Debug, Serialize)]
struct FilePrecheckResult {
    source_sections: usize,
    narration_sections: usize,
    document_bytes: usize,
    max_document_bytes: usize,
    explicit_source_markers: bool,
    warnings: Vec<String>,
    ready_for_send: bool,
}

#[derive(Clone, Debug, Serialize)]
struct StagedAppendResult {
    source_path: String,
    narration_path: String,
    appended_sections: usize,
    total_sections: usize,
    document_bytes: usize,
    ready_for_send: bool,
}

fn default_document() -> Document {
    Document {
        title: "Untitled reading".to_string(),
        sections: vec![DocumentSection {
            markdown: "# Kokoro Reader\n\nPaste Markdown from Codex, then press Play.".to_string(),
            speech_text: "Kokoro Reader. Paste Markdown from Codex, then press Play.".to_string(),
            speech_mode: SpeechMode::Automatic,
        }],
    }
}

fn app_paths(app: &tauri::AppHandle) -> Result<StoragePaths, String> {
    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|error| error.to_string())?;
    let cache_dir = app
        .path()
        .app_cache_dir()
        .map_err(|error| error.to_string())?;
    Ok(StoragePaths {
        model_dir: data_dir.join("models"),
        document_dir: shared_document_dir()?,
        data_dir,
        cache_dir: cache_dir.join("audio"),
    })
}

fn shared_root_dir() -> Result<PathBuf, String> {
    BaseDirs::new()
        .map(|base| base.home_dir().join(".kokoro_reader"))
        .ok_or_else(|| "Could not locate the home directory".to_string())
}

fn shared_document_dir() -> Result<PathBuf, String> {
    Ok(shared_root_dir()?.join("documents"))
}

fn mcp_data_dir() -> Result<PathBuf, String> {
    let document_dir = shared_document_dir()?;
    let legacy_dir = BaseDirs::new()
        .map(|base| base.data_dir().join(APP_ID))
        .ok_or_else(|| "Could not locate the macOS application-support directory".to_string())?;
    migrate_legacy_documents(&StoragePaths {
        data_dir: legacy_dir,
        document_dir: document_dir.clone(),
        model_dir: PathBuf::new(),
        cache_dir: PathBuf::new(),
    })?;
    Ok(document_dir)
}

fn mcp_cache_dir() -> Result<PathBuf, String> {
    BaseDirs::new()
        .map(|base| base.cache_dir().join(APP_ID).join("audio"))
        .ok_or_else(|| "Could not locate the cache directory".to_string())
}

fn active_project_path(document_dir: &Path) -> PathBuf {
    document_dir.join(ACTIVE_PROJECT_FILE)
}

fn current_document_path(data_dir: &Path) -> PathBuf {
    data_dir.join("document.json")
}

fn previous_document_path(data_dir: &Path) -> PathBuf {
    data_dir.join("previous-document.json")
}

fn source_document_path(data_dir: &Path) -> PathBuf {
    data_dir.join("source.md")
}

fn narration_document_path(data_dir: &Path) -> PathBuf {
    data_dir.join("narration.txt")
}

fn project_metadata_path(project: &Path) -> PathBuf {
    project.join(PROJECT_METADATA_FILE)
}

fn valid_project_id(project_id: &str) -> bool {
    !project_id.is_empty()
        && project_id.len() <= 128
        && project_id != "."
        && project_id != ".."
        && project_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_codex_thread_id(thread_id: &str) -> bool {
    !thread_id.is_empty()
        && thread_id.len() <= 128
        && thread_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn codex_url_for_thread_id(thread_id: &str) -> Option<String> {
    let thread_id = thread_id.trim();
    valid_codex_thread_id(thread_id).then(|| format!("codex://threads/{thread_id}"))
}

fn valid_codex_url(url: &str) -> bool {
    url.strip_prefix("codex://threads/")
        .and_then(codex_url_for_thread_id)
        .is_some_and(|candidate| candidate == url)
}

fn project_metadata_for_transfer(codex_url: String) -> ProjectMetadata {
    ProjectMetadata {
        read: false,
        codex_url: Some(codex_url),
    }
}

fn required_codex_url(url: &str) -> Result<String, McpError> {
    let normalized = url.trim();
    if valid_codex_url(normalized) {
        Ok(normalized.to_string())
    } else {
        Err(McpError::invalid_params(
            "codex_url must be a valid codex://threads/<thread-id> URL",
            None,
        ))
    }
}

fn project_path(document_dir: &Path, project_id: &str) -> Result<PathBuf, String> {
    if !valid_project_id(project_id) {
        return Err("project_id must be a safe non-empty identifier".to_string());
    }
    Ok(document_dir.join(project_id))
}

fn project_cache_references_path(project: &Path) -> PathBuf {
    project.join(PROJECT_CACHE_REFERENCES_FILE)
}

fn read_cache_references(project: &Path) -> Result<BTreeSet<String>, String> {
    match fs::read(project_cache_references_path(project)) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("Could not parse cache references: {error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(format!("Could not read cache references: {error}")),
    }
}

fn write_cache_references(project: &Path, references: &BTreeSet<String>) -> Result<(), String> {
    let contents = serde_json::to_vec(references)
        .map_err(|error| format!("Could not serialize cache references: {error}"))?;
    atomic_write(&project_cache_references_path(project), &contents)
}

fn record_cache_references<I>(project: &Path, keys: I) -> Result<(), String>
where
    I: IntoIterator<Item = String>,
{
    let _lock = CACHE_REFERENCES_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Cache reference lock poisoned".to_string())?;
    if !project.is_dir() {
        return Err(format!("Project does not exist: {}", project.display()));
    }
    let mut references = read_cache_references(project)?;
    let changed = keys
        .into_iter()
        .fold(false, |changed, key| references.insert(key) || changed);
    if changed {
        write_cache_references(project, &references)?;
    }
    Ok(())
}

fn record_cache_reference(project: &Path, key: &str) -> Result<(), String> {
    record_cache_references(project, std::iter::once(key.to_string()))
}

fn read_active_project_id(document_dir: &Path) -> Result<Option<String>, String> {
    let contents = match fs::read_to_string(active_project_path(document_dir)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let project_id = contents.trim();
    if !valid_project_id(project_id) {
        return Ok(None);
    }
    let path = project_path(document_dir, project_id)?;
    if path.is_dir() && current_document_path(&path).is_file() {
        Ok(Some(project_id.to_string()))
    } else {
        Ok(None)
    }
}

fn set_active_project_id(document_dir: &Path, project_id: &str) -> Result<(), String> {
    let path = project_path(document_dir, project_id)?;
    if !path.is_dir() {
        return Err(format!("Project does not exist: {project_id}"));
    }
    atomic_write(
        &active_project_path(document_dir),
        format!("{project_id}\n").as_bytes(),
    )
}

fn clear_active_project_id(document_dir: &Path) -> Result<(), String> {
    match fs::remove_file(active_project_path(document_dir)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn list_project_ids(document_dir: &Path) -> Result<Vec<String>, String> {
    fs::create_dir_all(document_dir).map_err(|error| error.to_string())?;
    let mut projects = Vec::new();
    for entry in fs::read_dir(document_dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let project_id = entry.file_name().to_string_lossy().to_string();
        if !entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
            || !valid_project_id(&project_id)
            || !current_document_path(&path).is_file()
        {
            continue;
        }
        let modified = fs::metadata(current_document_path(&path))
            .map_err(|error| error.to_string())?
            .modified()
            .map_err(|error| error.to_string())?;
        projects.push((project_id, modified));
    }
    projects.sort_by(|left, right| right.1.cmp(&left.1));
    Ok(projects
        .into_iter()
        .map(|(project_id, _)| project_id)
        .collect())
}

fn create_project_dir(document_dir: &Path) -> Result<String, String> {
    fs::create_dir_all(document_dir).map_err(|error| error.to_string())?;
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    for attempt in 0..100_u32 {
        let project_id = format!("project-{timestamp}-{}-{attempt}", std::process::id());
        match fs::create_dir(project_path(document_dir, &project_id)?) {
            Ok(()) => return Ok(project_id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Could not allocate a unique project ID".to_string())
}

fn read_project(document_dir: &Path, project_id: &str) -> Result<Document, String> {
    let path = project_path(document_dir, project_id)?;
    ensure_project_exists(&path, project_id)?;
    read_document(&current_document_path(&path))?
        .ok_or_else(|| format!("Project has no document: {project_id}"))
}

fn read_project_metadata(project: &Path) -> Result<ProjectMetadata, String> {
    match fs::read(project_metadata_path(project)) {
        Ok(contents) => {
            let mut metadata: ProjectMetadata = serde_json::from_slice(&contents)
                .map_err(|error| format!("Could not parse project metadata: {error}"))?;
            if metadata
                .codex_url
                .as_deref()
                .is_some_and(|url| !valid_codex_url(url))
            {
                metadata.codex_url = None;
            }
            Ok(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(ProjectMetadata::default())
        }
        Err(error) => Err(format!("Could not read project metadata: {error}")),
    }
}

fn write_project_metadata(project: &Path, metadata: &ProjectMetadata) -> Result<(), String> {
    if metadata
        .codex_url
        .as_deref()
        .is_some_and(|url| !valid_codex_url(url))
    {
        return Err("Project metadata contains an invalid Codex URL".to_string());
    }
    let contents = serde_json::to_vec_pretty(metadata)
        .map_err(|error| format!("Could not serialize project metadata: {error}"))?;
    atomic_write(&project_metadata_path(project), &contents)
}

fn ensure_project_exists(project: &Path, project_id: &str) -> Result<(), String> {
    if project.is_dir() {
        Ok(())
    } else {
        Err(format!("Project does not exist: {project_id}"))
    }
}

fn project_summaries(
    document_dir: &Path,
    active_project_id: Option<&str>,
) -> Result<Vec<ProjectSummary>, String> {
    list_project_ids(document_dir)?
        .into_iter()
        .map(|project_id| {
            let path = project_path(document_dir, &project_id)?;
            let document = read_project(document_dir, &project_id)?;
            let updated_at = fs::metadata(current_document_path(&path))
                .map_err(|error| error.to_string())?
                .modified()
                .map_err(|error| error.to_string())?
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            Ok(ProjectSummary {
                active: active_project_id == Some(project_id.as_str()),
                project_id,
                title: document.title,
                updated_at,
                read: read_project_metadata(&path)?.read,
            })
        })
        .collect()
}

fn project_documents(document_dir: &Path) -> Result<Vec<ProjectDocument>, String> {
    list_project_ids(document_dir)?
        .into_iter()
        .map(|project_id| project_document(document_dir, &project_id))
        .collect()
}

fn project_document(document_dir: &Path, project_id: &str) -> Result<ProjectDocument, String> {
    let project = project_path(document_dir, project_id)?;
    ensure_project_exists(&project, project_id)?;
    Ok(ProjectDocument {
        project_id: project_id.to_string(),
        document: read_project(document_dir, project_id)?,
        metadata: read_project_metadata(&project)?,
    })
}

fn project_location(document_dir: &Path, project_id: &str) -> Result<ProjectLocation, String> {
    let directory = project_path(document_dir, project_id)?;
    let document = read_project(document_dir, project_id)?;
    Ok(ProjectLocation {
        project_id: project_id.to_string(),
        title: document.title,
        directory: directory.to_string_lossy().to_string(),
        document_path: current_document_path(&directory)
            .to_string_lossy()
            .to_string(),
        source_path: source_document_path(&directory)
            .to_string_lossy()
            .to_string(),
        narration_path: narration_document_path(&directory)
            .to_string_lossy()
            .to_string(),
    })
}

fn copy_first_file(sources: &[PathBuf], destination: &Path) -> Result<bool, String> {
    for source in sources {
        if source.is_file() {
            atomic_write(
                destination,
                &fs::read(source).map_err(|error| error.to_string())?,
            )?;
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_legacy_documents(paths: &StoragePaths) -> Result<(), String> {
    fs::create_dir_all(&paths.document_dir).map_err(|error| error.to_string())?;
    let project_ids = list_project_ids(&paths.document_dir)?;
    if !project_ids.is_empty() {
        if read_active_project_id(&paths.document_dir)?.is_none() {
            set_active_project_id(&paths.document_dir, &project_ids[0])?;
        }
        return Ok(());
    }

    let legacy_current_paths = vec![
        current_document_path(&paths.document_dir),
        current_document_path(&paths.data_dir),
    ];
    let legacy_previous_paths = vec![
        previous_document_path(&paths.document_dir),
        previous_document_path(&paths.data_dir),
    ];
    let legacy_source_paths = vec![
        source_document_path(&paths.document_dir),
        source_document_path(&paths.data_dir),
    ];
    let legacy_narration_paths = vec![
        narration_document_path(&paths.document_dir),
        narration_document_path(&paths.data_dir),
    ];
    let has_legacy_files = legacy_current_paths.iter().any(|path| path.is_file())
        || legacy_source_paths.iter().any(|path| path.is_file());
    if !has_legacy_files {
        return Ok(());
    }

    let project_id = if project_path(&paths.document_dir, MIGRATED_PROJECT_ID)?.exists() {
        create_project_dir(&paths.document_dir)?
    } else {
        fs::create_dir(project_path(&paths.document_dir, MIGRATED_PROJECT_ID)?)
            .map_err(|error| error.to_string())?;
        MIGRATED_PROJECT_ID.to_string()
    };
    let project = project_path(&paths.document_dir, &project_id)?;
    let copied_current = copy_first_file(&legacy_current_paths, &current_document_path(&project))?;
    let copied_source = copy_first_file(&legacy_source_paths, &source_document_path(&project))?;
    copy_first_file(&legacy_narration_paths, &narration_document_path(&project))?;
    copy_first_file(&legacy_previous_paths, &previous_document_path(&project))?;

    if !copied_current && copied_source {
        let source = fs::read_to_string(source_document_path(&project))
            .map_err(|error| error.to_string())?;
        let narration = fs::read_to_string(narration_document_path(&project)).ok();
        let (document, _) = document_from_shared_files(
            "Migrated reading".to_string(),
            &source,
            narration.as_deref(),
        )?;
        write_document(&current_document_path(&project), &document)?;
    } else if copied_current && !source_document_path(&project).is_file() {
        let document = read_project(&paths.document_dir, &project_id)?;
        write_shared_document_files(&project, &document)?;
    }
    set_active_project_id(&paths.document_dir, &project_id)
}

fn active_or_first_project_id(document_dir: &Path) -> Result<Option<String>, String> {
    if let Some(project_id) = read_active_project_id(document_dir)? {
        return Ok(Some(project_id));
    }
    if let Some(project_id) = list_project_ids(document_dir)?.into_iter().next() {
        set_active_project_id(document_dir, &project_id)?;
        return Ok(Some(project_id));
    }
    clear_active_project_id(document_dir)?;
    Ok(None)
}

fn delete_stored_project(
    document_dir: &Path,
    cache_dir: &Path,
    project_id: &str,
) -> Result<Option<ProjectDocument>, String> {
    let _lock = CACHE_REFERENCES_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Cache reference lock poisoned".to_string())?;
    let project = project_path(document_dir, project_id)?;
    ensure_project_exists(&project, &project_id)?;
    fs::remove_dir_all(&project).map_err(|error| error.to_string())?;
    prune_orphaned_audio(document_dir, cache_dir)?;
    let Some(next_project_id) = active_or_first_project_id(document_dir)? else {
        return Ok(None);
    };
    Ok(Some(project_document(document_dir, &next_project_id)?))
}

fn log_path() -> Result<PathBuf, String> {
    Ok(shared_root_dir()?.join("logs").join("kokoro-reader.log"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("No parent directory for {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

fn write_document_file_temp(path: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("No parent directory for {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary).map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    Ok(temporary)
}

fn write_document_files_atomically(
    source_path: &Path,
    narration_path: &Path,
    source: &[u8],
    narration: &[u8],
) -> Result<(), String> {
    let previous_source = match fs::read(source_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.to_string()),
    };
    let source_temp = write_document_file_temp(source_path, source)?;
    let narration_temp = match write_document_file_temp(narration_path, narration) {
        Ok(path) => path,
        Err(error) => {
            let _ = fs::remove_file(&source_temp);
            return Err(error);
        }
    };

    if let Err(error) = fs::rename(&source_temp, source_path) {
        let _ = fs::remove_file(&source_temp);
        let _ = fs::remove_file(&narration_temp);
        return Err(error.to_string());
    }
    if let Err(error) = fs::rename(&narration_temp, narration_path) {
        let rollback = match previous_source {
            Some(bytes) => atomic_write(source_path, &bytes),
            None => {
                fs::remove_file(source_path).map_err(|rollback_error| rollback_error.to_string())
            }
        };
        let _ = fs::remove_file(&narration_temp);
        return match rollback {
            Ok(()) => Err(error.to_string()),
            Err(rollback_error) => Err(format!(
                "Could not write narration file: {error}; rollback failed: {rollback_error}"
            )),
        };
    }
    Ok(())
}

fn serialize_document(document: &Document) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(document).map_err(|error| error.to_string())
}

fn document_byte_size(document: &Document) -> usize {
    let (source, narration) = shared_document_file_contents(document);
    source.len() + narration.len()
}

fn validate_document(document: &Document) -> Result<(), String> {
    let size = document_byte_size(document);
    if size > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "Document is too large ({} bytes; limit is {} bytes)",
            size, MAX_DOCUMENT_BYTES
        ));
    }
    Ok(())
}

fn read_document(path: &Path) -> Result<Option<Document>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("Could not parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_document(path: &Path, document: &Document) -> Result<(), String> {
    validate_document(document)?;
    atomic_write(path, &serialize_document(document)?)
}

fn shared_document_file_contents(document: &Document) -> (String, String) {
    let source = document
        .sections
        .iter()
        .map(|section| section.markdown.trim())
        .collect::<Vec<_>>()
        .join(SECTION_SEPARATOR);
    let narration = document
        .sections
        .iter()
        .map(|section| section.speech_text.trim())
        .collect::<Vec<_>>()
        .join(NARRATION_SEPARATOR);
    (source, narration)
}

fn write_document_files(
    source_path: &Path,
    narration_path: &Path,
    document: &Document,
) -> Result<(), String> {
    validate_document(document)?;
    let (source, narration) = shared_document_file_contents(document);
    write_document_files_atomically(
        source_path,
        narration_path,
        source.as_bytes(),
        narration.as_bytes(),
    )
}

fn write_shared_document_files(data_dir: &Path, document: &Document) -> Result<(), String> {
    write_document_files(
        &source_document_path(data_dir),
        &narration_document_path(data_dir),
        document,
    )
}

fn rotate_log_if_needed(path: &Path, incoming_bytes: u64) -> Result<(), String> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if length + incoming_bytes <= LOG_MAX_BYTES {
        return Ok(());
    }
    for index in (1..=LOG_BACKUP_COUNT).rev() {
        let destination = path.with_extension(format!("log.{index}"));
        if destination.is_file() {
            fs::remove_file(&destination).map_err(|error| error.to_string())?;
        }
        let source = if index == 1 {
            path.to_path_buf()
        } else {
            path.with_extension(format!("log.{}", index - 1))
        };
        if source.is_file() {
            fs::rename(source, destination).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn log_event(message: impl AsRef<str>) {
    let message = message.as_ref();
    eprintln!("kokoro reader | {message}");
    let result = (|| -> Result<(), String> {
        let path = log_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| "Log path has no parent directory".to_string())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis();
        let entry = format!("[{timestamp}ms] {message}\n");
        rotate_log_if_needed(&path, entry.len() as u64)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.write_all(entry.as_bytes())
            .map_err(|error| error.to_string())
    })();
    if let Err(error) = result {
        eprintln!("kokoro reader | could not write log: {error}");
    }
}

fn model_assets_present(paths: &StoragePaths) -> bool {
    paths.model_dir.join("model.onnx").is_file()
        && VOICE_ASSETS.iter().all(|asset| {
            paths
                .model_dir
                .join("voices")
                .join(format!("{}.bin", asset.name))
                .is_file()
        })
}

fn set_runtime(state: &AppState, update: impl FnOnce(&mut RuntimeStatus)) {
    if let Ok(mut runtime) = state.runtime.lock() {
        update(&mut runtime);
    }
}

fn set_runtime_error(state: &AppState, error: String) {
    set_runtime(state, |runtime| {
        runtime.error = Some(error);
        runtime.downloading = false;
        runtime.progress = None;
        runtime.engine_ready = false;
    });
}

fn emit_runtime_status(app: &tauri::AppHandle, state: &AppState) {
    match state.runtime.lock() {
        Ok(runtime) => {
            if let Err(error) = app.emit("runtime-updated", runtime.clone()) {
                log_event(format!("could not emit runtime update: {error}"));
            }
        }
        Err(_) => log_event("runtime lock poisoned while emitting update"),
    }
}

fn backend_label() -> String {
    match std::env::var("KOKORO_ORT_PROVIDER")
        .unwrap_or_else(|_| "auto".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "cpu" => "CPU (forced)".to_string(),
        "coreml" => "CoreML (strict)".to_string(),
        _ => "CoreML preferred; automatic CPU fallback enabled".to_string(),
    }
}

fn configure_default_backend() {
    let configured = std::env::var("KOKORO_ORT_PROVIDER").unwrap_or_else(|_| "auto".to_string());
    if configured.eq_ignore_ascii_case("auto") {
        std::env::set_var("KOKORO_ORT_PROVIDER", "cpu");
        log_event("defaulting to CPU inference for reliable packaged-app startup");
    }
}

async fn ensure_engine(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let mut engine = state.engine.lock().await;
    if engine.is_some() {
        return Ok(());
    }
    let paths = app_paths(app)?;
    if !model_assets_present(&paths) {
        return Err("The Kokoro model is not installed yet".to_string());
    }
    set_runtime(state, |runtime| {
        runtime.model_ready = true;
        runtime.engine_ready = false;
        runtime.error = None;
        runtime.backend = Some(backend_label());
    });
    emit_runtime_status(app, state);
    let started = Instant::now();
    log_event("initializing CPU inference engine");
    let model_path = paths.model_dir.join("model.onnx");
    let voices_path = paths.model_dir.join("voices");
    let model = model_path.to_string_lossy().to_string();
    let voices = voices_path.to_string_lossy().to_string();
    let tts = KokoroTts::new(model.as_str(), voices.as_str())
        .await
        .map_err(|error| error.to_string())?;
    *engine = Some(Arc::new(tts));
    set_runtime(state, |runtime| {
        runtime.model_ready = true;
        runtime.engine_ready = true;
        runtime.downloading = false;
        runtime.error = None;
    });
    emit_runtime_status(app, state);
    log_event(format!(
        "CPU inference engine ready in {:.2?}",
        started.elapsed()
    ));
    Ok(())
}

fn split_sections_on_marker(contents: &str, marker: &str, keep_empty: bool) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    for line in contents.lines() {
        if line.trim() == marker {
            let section = current.trim().to_string();
            if keep_empty || !section.is_empty() {
                sections.push(section);
            }
            current.clear();
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }
    let section = current.trim().to_string();
    if keep_empty || !section.is_empty() {
        sections.push(section);
    }
    sections
}

fn split_paragraph_sections(contents: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    let mut fenced = false;
    for line in contents.replace("\r\n", "\n").lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        }
        if !fenced && trimmed.is_empty() {
            if !current.is_empty() {
                blocks.push(current.join("\n").trim().to_string());
                current.clear();
            }
        } else {
            current.push(line.to_string());
        }
    }
    if !current.is_empty() {
        blocks.push(current.join("\n").trim().to_string());
    }

    let mut sections = Vec::new();
    let mut index = 0;
    while index < blocks.len() {
        let block = &blocks[index];
        let is_heading_only = block.lines().count() == 1
            && block
                .lines()
                .next()
                .is_some_and(|line| line.trim_start().starts_with('#'));
        if is_heading_only && index + 1 < blocks.len() {
            sections.push(format!("{block}\n\n{}", blocks[index + 1]));
            index += 2;
        } else {
            sections.push(block.clone());
            index += 1;
        }
    }
    sections
}

fn document_from_shared_files(
    title: String,
    source: &str,
    narration: Option<&str>,
) -> Result<(Document, usize), String> {
    let markdown_sections = if source
        .lines()
        .any(|line| line.trim() == "<!-- kokoro-reader-section -->")
    {
        split_sections_on_marker(source, "<!-- kokoro-reader-section -->", false)
    } else {
        split_paragraph_sections(source)
    };
    if markdown_sections.is_empty() {
        return Err("source Markdown must contain at least one non-empty section".to_string());
    }
    let narration_sections = narration.map(|text| split_sections_on_marker(text, "---", true));
    if let Some(sections) = &narration_sections {
        if sections.len() != markdown_sections.len() {
            return Err(format!(
                "narration.txt has {} sections but source.md has {}",
                sections.len(),
                markdown_sections.len()
            ));
        }
    }
    let mut automatic_narration_sections = 0;
    let sections = markdown_sections
        .into_iter()
        .enumerate()
        .map(|(index, markdown)| {
            let explicit = narration_sections
                .as_ref()
                .and_then(|sections| sections.get(index))
                .filter(|text| !text.trim().is_empty());
            let automatic = markdown_to_speech(&markdown);
            let (speech_text, speech_mode) = match explicit {
                Some(text) if text.trim() != automatic.trim() => (text.clone(), SpeechMode::Custom),
                _ => {
                    automatic_narration_sections += 1;
                    (automatic, SpeechMode::Automatic)
                }
            };
            DocumentSection {
                markdown,
                speech_text,
                speech_mode,
            }
        })
        .collect();
    Ok((Document { title, sections }, automatic_narration_sections))
}

fn document_from_mcp_files(
    title: String,
    source: &str,
    narration: &str,
) -> Result<Document, String> {
    let markdown_sections = if source
        .lines()
        .any(|line| line.trim() == "<!-- kokoro-reader-section -->")
    {
        split_sections_on_marker(source, "<!-- kokoro-reader-section -->", false)
    } else {
        split_paragraph_sections(source)
    };
    if markdown_sections.is_empty() {
        return Err("source Markdown must contain at least one non-empty section".to_string());
    }
    let narration_sections = split_sections_on_marker(narration, "---", true);
    if narration_sections.len() != markdown_sections.len() {
        return Err(format!(
            "narration.txt has {} sections but source.md has {}",
            narration_sections.len(),
            markdown_sections.len()
        ));
    }
    let mut sections = Vec::with_capacity(markdown_sections.len());
    for (index, markdown) in markdown_sections.into_iter().enumerate() {
        let speech_text = narration_sections[index].trim().to_string();
        if speech_text.is_empty() {
            return Err(
                "MCP narration is required for every section; narration.txt must contain one non-empty section per source section"
                    .to_string(),
            );
        }
        sections.push(DocumentSection {
            markdown,
            speech_text,
            speech_mode: SpeechMode::Custom,
        });
    }
    Ok(Document { title, sections })
}

fn replace_current_document(data_dir: &Path, document: &Document) -> Result<(), String> {
    validate_document(document)?;
    write_shared_document_files(data_dir, document)?;
    let current_path = current_document_path(data_dir);
    let previous_path = previous_document_path(data_dir);
    match fs::read(&current_path) {
        Ok(current_bytes) => atomic_write(&previous_path, &current_bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    write_document(&current_path, document)
}

fn readable_shared_file(path: &str) -> Result<String, String> {
    let root = fs::canonicalize(shared_root_dir()?).map_err(|error| error.to_string())?;
    let candidate = fs::canonicalize(path).map_err(|error| error.to_string())?;
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return Err("file paths must name a file inside ~/.kokoro_reader".to_string());
    }
    let metadata = fs::metadata(&candidate).map_err(|error| error.to_string())?;
    if metadata.len() as usize > MAX_DOCUMENT_BYTES {
        return Err("file exceeds the 2 MiB limit".to_string());
    }
    fs::read_to_string(candidate).map_err(|error| error.to_string())
}

fn readable_staging_file(path: &str, suffix: &str, inbox: &Path) -> Result<PathBuf, String> {
    let candidate = Path::new(path);
    if !candidate.is_absolute() {
        return Err("staging file paths must be absolute".to_string());
    }
    let parent = candidate
        .parent()
        .ok_or_else(|| format!("No parent directory for {path}"))?;
    let parent = fs::canonicalize(parent).map_err(|error| error.to_string())?;
    if parent != inbox {
        return Err("staging files must be direct children of ~/.kokoro_reader/inbox".to_string());
    }
    let name = candidate
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("Invalid staging file name: {path}"))?;
    if !name.ends_with(suffix) {
        return Err(format!("staging file must end with {suffix}"));
    }
    match fs::symlink_metadata(candidate) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            Err(format!("staging path is not a regular file: {path}"))
        }
        Ok(_) => {
            let canonical = fs::canonicalize(candidate).map_err(|error| error.to_string())?;
            if canonical.parent() != Some(inbox) || !canonical.is_file() {
                return Err("staging files must remain inside ~/.kokoro_reader/inbox".to_string());
            }
            Ok(canonical)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(inbox.join(name)),
        Err(error) => Err(error.to_string()),
    }
}

fn read_staging_file(path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if metadata.len() as usize > MAX_DOCUMENT_BYTES {
        return Err("staging file exceeds the 2 MiB limit".to_string());
    }
    fs::read_to_string(path).map_err(|error| error.to_string())
}

fn staging_file_pair_in(
    inbox: &Path,
    source_path: &str,
    narration_path: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let inbox = fs::canonicalize(inbox).map_err(|error| error.to_string())?;
    let source = readable_staging_file(source_path, "-source.md", &inbox)?;
    let narration = readable_staging_file(narration_path, "-narration.txt", &inbox)?;
    let source_name = source
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix("-source.md"))
        .ok_or_else(|| "Invalid staging source filename".to_string())?;
    let narration_name = narration
        .file_name()
        .and_then(|value| value.to_str())
        .and_then(|value| value.strip_suffix("-narration.txt"))
        .ok_or_else(|| "Invalid staging narration filename".to_string())?;
    if source_name.is_empty() || source_name != narration_name {
        return Err(
            "source_path and narration_path must use the same <slug>-source/narration pair"
                .to_string(),
        );
    }
    Ok((source, narration))
}

fn normalize_section_text(value: String) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_string()
}

fn document_sections_from_incoming(
    sections: Vec<IncomingSection>,
) -> Result<Vec<DocumentSection>, String> {
    if sections.is_empty() {
        return Err("sections must not be empty".to_string());
    }
    sections
        .into_iter()
        .map(|section| {
            let markdown = normalize_section_text(section.markdown);
            let speech_text = normalize_section_text(section.speech_text);
            if markdown.is_empty() {
                return Err("section markdown must not be empty".to_string());
            }
            if speech_text.is_empty() {
                return Err(
                    "section speech_text must not be empty; automatic narration is disabled for MCP transfers"
                        .to_string(),
                );
            }
            Ok(DocumentSection {
                markdown,
                speech_text,
                speech_mode: SpeechMode::Custom,
            })
        })
        .collect()
}

fn precheck_document_contents(source: &str, narration: &str) -> Result<FilePrecheckResult, String> {
    let explicit_source_markers = source
        .lines()
        .any(|line| line.trim() == "<!-- kokoro-reader-section -->");
    let source_sections = if explicit_source_markers {
        split_sections_on_marker(&source, "<!-- kokoro-reader-section -->", false).len()
    } else {
        split_paragraph_sections(&source).len()
    };
    let narration_sections = split_sections_on_marker(&narration, "---", true).len();
    let document = document_from_mcp_files("Precheck".to_string(), &source, &narration)?;
    validate_document(&document)?;
    let mut warnings = Vec::new();
    if !explicit_source_markers {
        warnings.push(
            "source Markdown has no explicit section markers; paragraph fallback was used"
                .to_string(),
        );
    }
    Ok(FilePrecheckResult {
        source_sections,
        narration_sections,
        document_bytes: document_byte_size(&document),
        max_document_bytes: MAX_DOCUMENT_BYTES,
        explicit_source_markers,
        warnings,
        ready_for_send: true,
    })
}

fn precheck_document_files(
    source_path: &str,
    narration_path: &str,
) -> Result<FilePrecheckResult, String> {
    let source = readable_shared_file(source_path)?;
    let narration = readable_shared_file(narration_path)?;
    precheck_document_contents(&source, &narration)
}

fn append_to_staging_files_in(
    inbox: &Path,
    params: AppendToReaderFilesParams,
) -> Result<StagedAppendResult, String> {
    let (source_path, narration_path) =
        staging_file_pair_in(inbox, &params.source_path, &params.narration_path)?;
    let source_exists = source_path.is_file();
    let narration_exists = narration_path.is_file();
    if source_exists != narration_exists {
        return Err(
            "source and narration staging files must be created or present together".to_string(),
        );
    }

    let mut document = if source_exists {
        let source = read_staging_file(&source_path)?;
        let narration = read_staging_file(&narration_path)?;
        document_from_mcp_files("Staged reading".to_string(), &source, &narration)?
    } else {
        Document {
            title: "Staged reading".to_string(),
            sections: Vec::new(),
        }
    };
    if let Some(expected) = params.expected_section_count {
        if expected != document.sections.len() {
            return Err(format!(
                "expected {expected} existing sections but found {}",
                document.sections.len()
            ));
        }
    }
    let additions = document_sections_from_incoming(params.sections)?;
    let appended_sections = additions.len();
    document.sections.extend(additions);
    validate_document(&document)?;
    write_document_files(&source_path, &narration_path, &document)?;
    Ok(StagedAppendResult {
        source_path: source_path.to_string_lossy().to_string(),
        narration_path: narration_path.to_string_lossy().to_string(),
        appended_sections,
        total_sections: document.sections.len(),
        document_bytes: document_byte_size(&document),
        ready_for_send: true,
    })
}

fn append_to_staging_files(
    params: AppendToReaderFilesParams,
) -> Result<StagedAppendResult, String> {
    let inbox = shared_root_dir()?.join("inbox");
    fs::create_dir_all(&inbox).map_err(|error| error.to_string())?;
    let inbox = fs::canonicalize(inbox).map_err(|error| error.to_string())?;
    append_to_staging_files_in(&inbox, params)
}

async fn download_one(
    client: &reqwest::Client,
    spec: AssetSpec,
    destination: &Path,
    downloaded_before: u64,
    total_bytes: u64,
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<u64, String> {
    let temporary = destination.with_extension("part");
    let response = client
        .get(spec.url)
        .send()
        .await
        .map_err(|error| format!("Could not download {}: {error}", spec.name))?
        .error_for_status()
        .map_err(|error| format!("Could not download {}: {error}", spec.name))?;
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&temporary)
        .await
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut asset_bytes = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        file.write_all(&chunk)
            .await
            .map_err(|error| error.to_string())?;
        hasher.update(&chunk);
        asset_bytes += chunk.len() as u64;
        let progress = DownloadProgress {
            asset: spec.name.to_string(),
            downloaded_bytes: downloaded_before + asset_bytes,
            total_bytes: Some(total_bytes),
        };
        set_runtime(state, |runtime| runtime.progress = Some(progress.clone()));
        let _ = app.emit("model-progress", progress);
    }
    file.flush().await.map_err(|error| error.to_string())?;
    file.sync_all().await.map_err(|error| error.to_string())?;
    let actual = format!("{:x}", hasher.finalize());
    if actual != spec.sha256 {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(format!("Checksum mismatch for {}", spec.name));
    }
    tokio::fs::rename(&temporary, destination)
        .await
        .map_err(|error| error.to_string())?;
    Ok(asset_bytes)
}

async fn download_assets(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let paths = app_paths(app)?;
    tokio::fs::create_dir_all(paths.model_dir.join("voices"))
        .await
        .map_err(|error| error.to_string())?;
    let client = reqwest::Client::new();
    let mut downloaded = 0_u64;
    let total_bytes = 325_532_232_u64 + (522_000_u64 * VOICE_ASSETS.len() as u64);
    let model_spec = AssetSpec {
        name: "model.onnx",
        url: MODEL_URL,
        sha256: MODEL_SHA256,
    };
    downloaded += download_one(
        &client,
        model_spec,
        &paths.model_dir.join("model.onnx"),
        downloaded,
        total_bytes,
        app,
        state,
    )
    .await?;
    for asset in VOICE_ASSETS {
        downloaded += download_one(
            &client,
            *asset,
            &paths
                .model_dir
                .join("voices")
                .join(format!("{}.bin", asset.name)),
            downloaded,
            total_bytes,
            app,
            state,
        )
        .await?;
    }
    Ok(())
}

fn valid_voice(voice: &str) -> bool {
    VOICE_ASSETS.iter().any(|asset| asset.name == voice)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SynthesisChunk {
    text: String,
    token_count: usize,
}

#[derive(Clone, Debug)]
struct ActiveSynthesis {
    request_id: String,
    voice: String,
    chunk_index: usize,
    chunk_total: usize,
    token_count: usize,
    stage: &'static str,
}

static ACTIVE_SYNTHESIS: OnceLock<Mutex<Option<ActiveSynthesis>>> = OnceLock::new();

struct ActiveSynthesisGuard;

impl ActiveSynthesisGuard {
    fn new(context: ActiveSynthesis) -> Self {
        if let Ok(mut active) = ACTIVE_SYNTHESIS.get_or_init(|| Mutex::new(None)).lock() {
            *active = Some(context);
        }
        Self
    }
}

impl Drop for ActiveSynthesisGuard {
    fn drop(&mut self) {
        if let Some(active) = ACTIVE_SYNTHESIS.get() {
            if let Ok(mut active) = active.lock() {
                *active = None;
            }
        }
    }
}

fn active_synthesis_context() -> Option<ActiveSynthesis> {
    ACTIVE_SYNTHESIS
        .get()
        .and_then(|active| active.lock().ok().and_then(|active| active.clone()))
}

fn token_count_for_synthesis(text: &str) -> Result<usize, String> {
    let phonemes =
        g2p(text, false).map_err(|error| format!("Could not prepare speech: {error}"))?;
    Ok(get_token_ids(&phonemes, false).len())
}

fn checked_synthesis_chunk(text: &str) -> Result<SynthesisChunk, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Speech segment is empty".to_string());
    }
    let token_count = token_count_for_synthesis(text)?;
    if token_count > MAX_KOKORO_TOKENS {
        return Err(format!(
            "Speech segment is too long for Kokoro ({token_count} tokens; limit is {MAX_KOKORO_TOKENS})"
        ));
    }
    Ok(SynthesisChunk {
        text: text.to_string(),
        token_count,
    })
}

fn split_oversized_sentence(sentence: &str) -> Result<Vec<SynthesisChunk>, String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for word in sentence.split_whitespace() {
        let word_token_count = token_count_for_synthesis(word)?;
        if current.is_empty() && word_token_count > MAX_KOKORO_TOKENS {
            return Err(format!(
                "Speech contains an unbreakable token that exceeds the Kokoro limit ({word_token_count} tokens)"
            ));
        }
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if token_count_for_synthesis(&candidate)? <= MAX_KOKORO_TOKENS {
            current = candidate;
            continue;
        }
        if current.is_empty() {
            return Err(format!(
                "Speech contains an unbreakable token that exceeds the Kokoro limit ({word_token_count} tokens)"
            ));
        }
        chunks.push(checked_synthesis_chunk(&current)?);
        if word_token_count > MAX_KOKORO_TOKENS {
            return Err(format!(
                "Speech contains an unbreakable token that exceeds the Kokoro limit ({word_token_count} tokens)"
            ));
        }
        current = word.to_string();
    }
    if !current.is_empty() {
        chunks.push(checked_synthesis_chunk(&current)?);
    }
    Ok(chunks)
}

fn safe_synthesis_chunks(text: &str) -> Result<Vec<SynthesisChunk>, String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for sentence in split_sentences(text) {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        let candidate = if current.is_empty() {
            sentence.to_string()
        } else {
            format!("{current} {sentence}")
        };
        if token_count_for_synthesis(&candidate)? <= MAX_KOKORO_TOKENS {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            chunks.push(checked_synthesis_chunk(&current)?);
            current.clear();
        }
        if token_count_for_synthesis(sentence)? <= MAX_KOKORO_TOKENS {
            current = sentence.to_string();
        } else {
            chunks.extend(split_oversized_sentence(sentence)?);
        }
    }
    if !current.is_empty() {
        chunks.push(checked_synthesis_chunk(&current)?);
    }
    Ok(chunks)
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_string()
}

fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".to_string());
        let backtrace = Backtrace::force_capture();
        let synthesis_context = active_synthesis_context()
            .map(|context| {
                format!(
                    " request={} voice={} chunk={}/{} tokens={} stage={}",
                    context.request_id,
                    context.voice,
                    context.chunk_index,
                    context.chunk_total,
                    context.token_count,
                    context.stage
                )
            })
            .unwrap_or_default();
        log_event(format!(
            "panic{synthesis_context} location={location} message={info}\n{backtrace}"
        ));
        previous(info);
    }));
}

fn initialize_audio_cache(cache_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(cache_dir).map_err(|error| error.to_string())?;
    let marker = cache_dir.join(AUDIO_CACHE_VERSION_MARKER);
    if marker.is_file() {
        return Ok(());
    }
    for entry in fs::read_dir(cache_dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let is_cache_artifact = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("wav") | Some("tmp")
        );
        if is_cache_artifact
            && entry
                .metadata()
                .map_err(|error| error.to_string())?
                .is_file()
        {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    atomic_write(&marker, b"1\n")
}

fn prune_orphaned_audio(document_dir: &Path, cache_dir: &Path) -> Result<(), String> {
    let mut referenced_keys = BTreeSet::new();
    for project_id in list_project_ids(document_dir)? {
        let project = project_path(document_dir, &project_id)?;
        referenced_keys.extend(read_cache_references(&project)?);
    }

    let entries = match fs::read_dir(cache_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        if !entry
            .metadata()
            .map_err(|error| error.to_string())?
            .is_file()
            || path.extension().and_then(|extension| extension.to_str()) != Some("wav")
        {
            continue;
        }
        let referenced = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|key| referenced_keys.contains(key));
        if !referenced {
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn cache_key(text: &str, voice: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(MODEL_VERSION.as_bytes());
    hasher.update(&[0]);
    hasher.update(voice.as_bytes());
    hasher.update(&[0]);
    hasher.update(text.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn touch_cache_file(path: &Path) -> Result<(), String> {
    let file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.set_times(FileTimes::new().set_modified(SystemTime::now()))
        .map_err(|error| error.to_string())
}

fn write_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    let data_len = samples
        .len()
        .checked_mul(2)
        .ok_or_else(|| "Audio output is too large".to_string())? as u32;
    let riff_len = 36_u32
        .checked_add(data_len)
        .ok_or_else(|| "Audio output is too large".to_string())?;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&riff_len.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let pcm = (clamped * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&pcm.to_le_bytes());
    }
    atomic_write(path, &bytes)
}

fn enforce_cache_limit(cache_dir: &Path) -> Result<(), String> {
    enforce_cache_limit_with_limit(cache_dir, MAX_CACHE_BYTES)
}

fn enforce_cache_limit_with_limit(cache_dir: &Path, max_bytes: u64) -> Result<(), String> {
    let mut entries = Vec::new();
    let mut total = 0_u64;
    for item in fs::read_dir(cache_dir).map_err(|error| error.to_string())? {
        let item = item.map_err(|error| error.to_string())?;
        let metadata = item.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || item.path().extension().and_then(|ext| ext.to_str()) != Some("wav")
        {
            continue;
        }
        let size = metadata.len();
        total += size;
        entries.push((
            item.path(),
            size,
            metadata.modified().map_err(|error| error.to_string())?,
        ));
    }
    entries.sort_by_key(|entry| entry.2);
    for (path, size, _) in entries {
        if total <= max_bytes {
            break;
        }
        fs::remove_file(path).map_err(|error| error.to_string())?;
        total -= size;
    }
    Ok(())
}

fn cached_audio_size(path: &Path) -> Result<Option<u64>, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > 44 => Ok(Some(metadata.len())),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Could not inspect audio cache: {error}")),
    }
}

fn cached_audio_asset(path: &Path) -> Result<Option<AudioAsset>, String> {
    if let Some(size) = cached_audio_size(path)? {
        touch_cache_file(path)?;
        let duration_ms = ((size - 44) / 2 * 1000) / SAMPLE_RATE as u64;
        Ok(Some(AudioAsset {
            path: path.to_string_lossy().to_string(),
            duration_ms,
            cache_hit: true,
        }))
    } else {
        Ok(None)
    }
}

#[tauri::command]
fn audio_cache_status(
    app: tauri::AppHandle,
    project_id: String,
    texts: Vec<String>,
    voice: String,
) -> Result<Vec<bool>, String> {
    if !valid_voice(&voice) {
        return Err(format!("Unsupported voice: {voice}"));
    }
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    ensure_project_exists(&project, &project_id)?;
    let mut statuses = Vec::with_capacity(texts.len());
    let mut cached_keys = Vec::new();
    for text in texts {
        let text = text.trim().to_string();
        if text.is_empty() {
            statuses.push(false);
            continue;
        }
        if text.len() > MAX_SENTENCE_BYTES {
            return Err("Sentence is too large".to_string());
        }
        let key = cache_key(&text, &voice);
        let cache_path = paths.cache_dir.join(format!("{key}.wav"));
        let cached = cached_audio_size(&cache_path)?.is_some();
        statuses.push(cached);
        if cached {
            cached_keys.push(key);
        }
    }
    record_cache_references(&project, cached_keys)?;
    Ok(statuses)
}

#[tauri::command]
fn list_projects(app: tauri::AppHandle) -> Result<Vec<ProjectSummary>, String> {
    let paths = app_paths(&app)?;
    let active_project_id = read_active_project_id(&paths.document_dir)?;
    project_summaries(&paths.document_dir, active_project_id.as_deref())
}

#[tauri::command]
fn list_project_documents(app: tauri::AppHandle) -> Result<Vec<ProjectDocument>, String> {
    let paths = app_paths(&app)?;
    project_documents(&paths.document_dir)
}

#[tauri::command]
fn get_document(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<ProjectDocument>, String> {
    let paths = app_paths(&app)?;
    migrate_legacy_documents(&paths)?;
    let Some(project_id) = active_or_first_project_id(&paths.document_dir)? else {
        return Ok(None);
    };
    let loaded = project_document(&paths.document_dir, &project_id)?;
    set_state_document(&state, &loaded.document)?;
    Ok(Some(loaded))
}

#[tauri::command]
fn select_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectDocument, String> {
    let paths = app_paths(&app)?;
    let document = read_project(&paths.document_dir, &project_id)?;
    set_active_project_id(&paths.document_dir, &project_id)?;
    set_state_document(&state, &document)?;
    project_document(&paths.document_dir, &project_id)
}

fn set_state_document(state: &AppState, document: &Document) -> Result<(), String> {
    *state
        .document
        .lock()
        .map_err(|_| "Document lock poisoned".to_string())? = document.clone();
    Ok(())
}

#[tauri::command]
fn save_document(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    document: Document,
) -> Result<ProjectDocument, String> {
    validate_document(&document)?;
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    ensure_project_exists(&project, &project_id)?;
    write_document(&current_document_path(&project), &document)?;
    write_shared_document_files(&project, &document)?;
    if read_active_project_id(&paths.document_dir)?.as_deref() == Some(project_id.as_str()) {
        set_state_document(&state, &document)?;
    }
    project_document(&paths.document_dir, &project_id)
}

#[tauri::command]
fn restore_previous(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectDocument, String> {
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    let current_path = current_document_path(&project);
    let previous_path = previous_document_path(&project);
    let previous = read_document(&previous_path)?
        .ok_or_else(|| "No previous document is available".to_string())?;
    match fs::read(&current_path) {
        Ok(current_bytes) => atomic_write(&previous_path, &current_bytes)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    write_document(&current_path, &previous)?;
    write_shared_document_files(&project, &previous)?;
    if read_active_project_id(&paths.document_dir)?.as_deref() == Some(project_id.as_str()) {
        set_state_document(&state, &previous)?;
    }
    project_document(&paths.document_dir, &project_id)
}

#[tauri::command]
fn has_previous(app: tauri::AppHandle, project_id: String) -> Result<bool, String> {
    let paths = app_paths(&app)?;
    Ok(previous_document_path(&project_path(&paths.document_dir, &project_id)?).is_file())
}

#[tauri::command]
fn reload_shared_document(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<ProjectDocument, String> {
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    let source = fs::read_to_string(source_document_path(&project))
        .map_err(|error| format!("Could not read source.md: {error}"))?;
    let narration = match fs::read_to_string(narration_document_path(&project)) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("Could not read narration.txt: {error}")),
    };
    let title = read_project(&paths.document_dir, &project_id)?.title;
    let (document, _) = document_from_shared_files(title, &source, narration.as_deref())?;
    replace_current_document(&project, &document)?;
    set_active_project_id(&paths.document_dir, &project_id)?;
    set_state_document(&state, &document)?;
    log_event(format!("reloaded shared files for project {project_id}"));
    project_document(&paths.document_dir, &project_id)
}

#[tauri::command]
fn set_project_read(
    app: tauri::AppHandle,
    project_id: String,
    read: bool,
) -> Result<ProjectMetadata, String> {
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    ensure_project_exists(&project, &project_id)?;
    let metadata = ProjectMetadata {
        read,
        ..read_project_metadata(&project)?
    };
    write_project_metadata(&project, &metadata)?;
    log_event(format!(
        "marked project {project_id} {}",
        if read { "read" } else { "unread" }
    ));
    Ok(metadata)
}

#[tauri::command]
async fn delete_project(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Option<ProjectDocument>, String> {
    let _synthesis_guard = state.synthesis.lock().await;
    let paths = app_paths(&app)?;
    let next = delete_stored_project(&paths.document_dir, &paths.cache_dir, &project_id)?;
    if let Some(project) = &next {
        set_state_document(&state, &project.document)?;
    }
    log_event(format!("deleted project {project_id}"));
    Ok(next)
}

#[tauri::command]
fn runtime_status(state: State<'_, AppState>) -> Result<RuntimeStatus, String> {
    state
        .runtime
        .lock()
        .map(|runtime| runtime.clone())
        .map_err(|_| "Runtime lock poisoned".to_string())
}

#[tauri::command]
async fn download_model(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let state = state.inner().clone();
    {
        let mut runtime = state
            .runtime
            .lock()
            .map_err(|_| "Runtime lock poisoned".to_string())?;
        if runtime.downloading {
            return Err("A model download is already running".to_string());
        }
        runtime.downloading = true;
        runtime.error = None;
        runtime.progress = None;
    }
    emit_runtime_status(&app, &state);
    if let Err(error) = download_assets(&app, &state).await {
        set_runtime_error(&state, error.clone());
        emit_runtime_status(&app, &state);
        return Err(error);
    }
    if let Err(error) = ensure_engine(&app, &state).await {
        set_runtime_error(&state, error.clone());
        emit_runtime_status(&app, &state);
        return Err(error);
    }
    set_runtime(&state, |runtime| {
        runtime.model_ready = true;
        runtime.downloading = false;
        runtime.progress = None;
    });
    emit_runtime_status(&app, &state);
    Ok(())
}

#[tauri::command]
async fn synthesize_sentence(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_id: String,
    text: String,
    voice: String,
) -> Result<AudioAsset, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("Cannot synthesize an empty sentence".to_string());
    }
    if text.len() > MAX_SENTENCE_BYTES {
        return Err("Sentence is too large".to_string());
    }
    if !valid_voice(&voice) {
        return Err(format!("Unsupported voice: {voice}"));
    }
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    if !project.is_dir() {
        return Err(format!("Project does not exist: {project_id}"));
    }
    let key = cache_key(&text, &voice);
    fs::create_dir_all(&paths.cache_dir).map_err(|error| error.to_string())?;
    let cache_path = paths.cache_dir.join(format!("{key}.wav"));
    let request_id = cache_path
        .file_stem()
        .and_then(|value| value.to_str())
        .map(|value| value.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "unknown".to_string());
    let started = Instant::now();
    log_event(format!(
        "synthesis start request={request_id} voice={voice} bytes={} words={}",
        text.len(),
        text.split_whitespace().count()
    ));
    let _synthesis_guard = state.synthesis.lock().await;
    record_cache_reference(&project, &key)?;
    if let Some(asset) = cached_audio_asset(&cache_path)? {
        log_event(format!(
            "synthesis cache_hit request={request_id} duration_ms={}",
            asset.duration_ms
        ));
        return Ok(asset);
    }
    if let Some(asset) = cached_audio_asset(&cache_path)? {
        log_event(format!(
            "synthesis cache_hit_after_lock request={request_id} duration_ms={}",
            asset.duration_ms
        ));
        return Ok(asset);
    }
    let chunks = match safe_synthesis_chunks(&text) {
        Ok(chunks) => chunks,
        Err(error) => {
            log_event(format!(
                "synthesis error request={request_id} stage=preflight error={error}"
            ));
            return Err(format!(
                "Speech preparation failed (request {request_id}): {error}"
            ));
        }
    };
    if chunks.is_empty() {
        let error = "Speech preparation produced no playable chunks".to_string();
        log_event(format!(
            "synthesis error request={request_id} stage=preflight error={error}"
        ));
        return Err(format!(
            "Speech preparation failed (request {request_id}): {error}"
        ));
    }
    let max_tokens = chunks
        .iter()
        .map(|chunk| chunk.token_count)
        .max()
        .unwrap_or(0);
    log_event(format!(
        "synthesis preflight request={request_id} chunks={} max_tokens={max_tokens}",
        chunks.len()
    ));
    if let Err(error) = ensure_engine(&app, state.inner()).await {
        log_event(format!(
            "synthesis error request={request_id} stage=engine_init error={error}"
        ));
        let error = format!("Speech engine unavailable (request {request_id}): {error}");
        set_runtime_error(state.inner(), error.clone());
        emit_runtime_status(&app, state.inner());
        return Err(error);
    }
    let engine = match state.engine.lock().await.clone() {
        Some(engine) => engine,
        None => {
            let error = format!("Speech engine is not ready (request {request_id})");
            log_event(format!(
                "synthesis error request={request_id} stage=engine_state error=not_ready"
            ));
            set_runtime_error(state.inner(), error.clone());
            emit_runtime_status(&app, state.inner());
            return Err(error);
        }
    };
    let mut samples = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        log_event(format!(
            "synthesis chunk_start request={request_id} chunk={}/{} tokens={}",
            index + 1,
            chunks.len(),
            chunk.token_count
        ));
        let result = {
            let _active_synthesis = ActiveSynthesisGuard::new(ActiveSynthesis {
                request_id: request_id.clone(),
                voice: voice.clone(),
                chunk_index: index + 1,
                chunk_total: chunks.len(),
                token_count: chunk.token_count,
                stage: "inference",
            });
            AssertUnwindSafe(engine.synth(chunk.text.as_str(), Voice::new(voice.as_str())))
                .catch_unwind()
                .await
        };
        match result {
            Ok(Ok((chunk_samples, elapsed))) => {
                log_event(format!(
                    "synthesis chunk_complete request={request_id} chunk={}/{} elapsed_ms={} samples={}",
                    index + 1,
                    chunks.len(),
                    elapsed.as_millis(),
                    chunk_samples.len()
                ));
                samples.extend_from_slice(&chunk_samples);
            }
            Ok(Err(error)) => {
                let error = error.to_string();
                log_event(format!(
                    "synthesis error request={request_id} stage=inference chunk={}/{} error={error}",
                    index + 1,
                    chunks.len()
                ));
                return Err(format!(
                    "Speech synthesis failed (request {request_id}): {error}"
                ));
            }
            Err(payload) => {
                let panic_message = panic_payload_message(payload.as_ref());
                log_event(format!(
                    "synthesis panic request={request_id} stage=inference chunk={}/{} tokens={} message={panic_message}",
                    index + 1,
                    chunks.len(),
                    chunk.token_count
                ));
                *state.engine.lock().await = None;
                let error = format!(
                    "Speech engine stopped after an internal error (request {request_id}); retry the engine"
                );
                set_runtime_error(state.inner(), error.clone());
                emit_runtime_status(&app, state.inner());
                return Err(error);
            }
        }
    }
    if samples.is_empty() {
        let error = "Inference returned no audio samples".to_string();
        log_event(format!(
            "synthesis error request={request_id} stage=output error={error}"
        ));
        return Err(format!(
            "Speech synthesis produced no audio (request {request_id})"
        ));
    }
    if let Err(error) = ensure_project_exists(&project, &project_id) {
        log_event(format!(
            "synthesis error request={request_id} stage=project_deleted error={error}"
        ));
        return Err(error);
    }
    if let Err(error) = write_wav(&cache_path, &samples) {
        log_event(format!(
            "synthesis error request={request_id} stage=cache_write error={error}"
        ));
        return Err(format!(
            "Could not cache speech audio (request {request_id}): {error}"
        ));
    }
    if let Err(error) = enforce_cache_limit(&paths.cache_dir) {
        log_event(format!(
            "synthesis error request={request_id} stage=cache_eviction error={error}"
        ));
        return Err(format!(
            "Could not maintain speech cache (request {request_id}): {error}"
        ));
    }
    log_event(format!(
        "synthesis complete request={request_id} chunks={} samples={} duration_ms={} elapsed_ms={}",
        chunks.len(),
        samples.len(),
        ((samples.len() as u64) * 1000) / SAMPLE_RATE as u64,
        started.elapsed().as_millis()
    ));
    Ok(AudioAsset {
        path: cache_path.to_string_lossy().to_string(),
        duration_ms: ((samples.len() as u64) * 1000) / SAMPLE_RATE as u64,
        cache_hit: false,
    })
}

fn strip_html_tags(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' if !in_tag => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => output.push(character),
            _ => {}
        }
    }
    output
}

fn replace_links(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(open) = rest.find('[') {
        output.push_str(&rest[..open]);
        let Some(close_offset) = rest[open..].find(']') else {
            output.push_str(&rest[open..]);
            break;
        };
        let close = open + close_offset;
        let after = &rest[close + 1..];
        if let Some(url_end) = after.strip_prefix('(').and_then(|value| value.find(')')) {
            output.push_str(&rest[open + 1..close]);
            rest = &after[url_end + 1..];
        } else {
            output.push_str(&rest[open..=close]);
            rest = after;
        }
    }
    output.push_str(rest);
    output
}

fn markdown_to_speech(markdown: &str) -> String {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw_line in markdown.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        let mut line = strip_html_tags(trimmed);
        if !in_fence {
            while line.starts_with('#') {
                line.remove(0);
            }
            line = line
                .trim_start_matches(['-', '*', '+'])
                .trim_start()
                .trim_start_matches('>')
                .trim_start()
                .to_string();
            line = replace_links(&line);
            line = line.replace("![", "[");
            for marker in ["`", "**", "__", "~~", "*", "_"] {
                line = line.replace(marker, "");
            }
            for (from, to) in [
                ("\\rightarrow", " leads to "),
                ("\\Rightarrow", " implies "),
                ("\\leftrightarrow", " corresponds to "),
                ("\\leq", " less than or equal to "),
                ("\\geq", " greater than or equal to "),
                ("\\times", " times "),
                ("\\cdot", " times "),
                ("\\pm", " plus or minus "),
                ("\\approx", " approximately "),
                ("\\in", " in "),
                ("→", " leads to "),
                ("⇒", " implies "),
                ("↔", " corresponds to "),
                ("≤", " less than or equal to "),
                ("≥", " greater than or equal to "),
                ("×", " times "),
                ("±", " plus or minus "),
                ("≈", " approximately "),
            ] {
                line = line.replace(from, to);
            }
            line = line.replace(['$', '{', '}', '\\'], " ");
        }
        if !line.trim().is_empty() {
            lines.push(line);
        }
    }
    lines
        .join(". ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone)]
struct ReaderMcp {
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

#[tool_router(server_handler)]
impl ReaderMcp {
    #[tool(
        description = "Create a new isolated Kokoro Reader project, or update an existing project when project_id is supplied. Send aligned Markdown and required custom narration sections. codex_url is required and must be a valid codex://threads/<thread-id> backlink. The app is focused but does not autoplay."
    )]
    fn send_to_reader(
        &self,
        Parameters(params): Parameters<SendToReaderParams>,
    ) -> Result<String, McpError> {
        let codex_url = required_codex_url(&params.codex_url)?;
        let title = params
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Codex reading".to_string());
        let sections = document_sections_from_incoming(params.sections)
            .map_err(|error| McpError::invalid_params(error, None))?;
        let document = Document { title, sections };
        let (project_id, newly_created) = match params.project_id.as_deref() {
            Some(project_id) => {
                let project = project_path(&self.data_dir, project_id)
                    .map_err(|error| McpError::invalid_params(error, None))?;
                if !project.is_dir() {
                    return Err(McpError::invalid_params(
                        format!("Project does not exist: {project_id}"),
                        None,
                    ));
                }
                (project_id.to_string(), false)
            }
            None => (
                create_project_dir(&self.data_dir)
                    .map_err(|error| McpError::internal_error(error, None))?,
                true,
            ),
        };
        let project = project_path(&self.data_dir, &project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        if let Err(error) = replace_current_document(&project, &document) {
            if newly_created {
                let _ = fs::remove_dir_all(&project);
            }
            return Err(McpError::internal_error(error, None));
        }
        if let Err(error) =
            write_project_metadata(&project, &project_metadata_for_transfer(codex_url))
        {
            if newly_created {
                let _ = fs::remove_dir_all(&project);
            }
            return Err(McpError::internal_error(error, None));
        }
        set_active_project_id(&self.data_dir, &project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        log_event(format!(
            "received {} sections through MCP project={project_id}",
            document.sections.len()
        ));
        self.launch_reader(&project_id, &document, 0)
    }

    #[tool(
        description = "Load source Markdown and required matching custom narration text from files inside ~/.kokoro_reader into a new isolated Kokoro Reader project, or update the project_id supplied. codex_url is required and must be a valid codex://threads/<thread-id> backlink. Automatic narration is disabled for MCP transfers. Explicit source sections use <!-- kokoro-reader-section --> on its own line; narration sections use a line containing --- and must all be non-empty."
    )]
    fn send_file_to_reader(
        &self,
        Parameters(params): Parameters<SendFileToReaderParams>,
    ) -> Result<String, McpError> {
        let codex_url = required_codex_url(&params.codex_url)?;
        let source = readable_shared_file(&params.source_path)
            .map_err(|error| McpError::invalid_params(error, None))?;
        let narration = readable_shared_file(&params.narration_path)
            .map_err(|error| McpError::invalid_params(error, None))?;
        let title = params
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "Codex reading".to_string());
        let document = document_from_mcp_files(title, &source, &narration)
            .map_err(|error| McpError::invalid_params(error, None))?;
        let (project_id, newly_created) = match params.project_id.as_deref() {
            Some(project_id) => {
                let project = project_path(&self.data_dir, project_id)
                    .map_err(|error| McpError::invalid_params(error, None))?;
                if !project.is_dir() {
                    return Err(McpError::invalid_params(
                        format!("Project does not exist: {project_id}"),
                        None,
                    ));
                }
                (project_id.to_string(), false)
            }
            None => (
                create_project_dir(&self.data_dir)
                    .map_err(|error| McpError::internal_error(error, None))?,
                true,
            ),
        };
        let project = project_path(&self.data_dir, &project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        if let Err(error) = replace_current_document(&project, &document) {
            if newly_created {
                let _ = fs::remove_dir_all(&project);
            }
            return Err(McpError::internal_error(error, None));
        }
        if let Err(error) =
            write_project_metadata(&project, &project_metadata_for_transfer(codex_url))
        {
            if newly_created {
                let _ = fs::remove_dir_all(&project);
            }
            return Err(McpError::internal_error(error, None));
        }
        set_active_project_id(&self.data_dir, &project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        log_event(format!(
            "loaded {} sections from MCP file transfer project={project_id}",
            document.sections.len()
        ));
        self.launch_reader(&project_id, &document, 0)
    }

    #[tool(
        description = "Read-only validation for a source Markdown and matching narration file inside ~/.kokoro_reader. Returns section counts, size, marker mode, warnings, and ready_for_send without creating a project or refreshing the app."
    )]
    fn precheck_reader_files(
        &self,
        Parameters(params): Parameters<PrecheckReaderFilesParams>,
    ) -> Result<String, McpError> {
        let result = precheck_document_files(&params.source_path, &params.narration_path)
            .map_err(|error| McpError::invalid_params(error, None))?;
        serde_json::to_string(&result)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "Append one or more aligned Markdown and narration sections to a matching staging file pair directly under ~/.kokoro_reader/inbox. Creates the pair on the first call, normalizes harmless formatting, keeps both files synchronized, and does not create or refresh a Reader project."
    )]
    fn append_to_reader_files(
        &self,
        Parameters(params): Parameters<AppendToReaderFilesParams>,
    ) -> Result<String, McpError> {
        let result = append_to_staging_files(params)
            .map_err(|error| McpError::invalid_params(error, None))?;
        serde_json::to_string(&result)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "List isolated Kokoro Reader projects, newest first, with their stable IDs and titles."
    )]
    fn list_reader_projects(&self) -> Result<String, McpError> {
        let active_project_id = read_active_project_id(&self.data_dir)
            .map_err(|error| McpError::internal_error(error, None))?;
        let projects = project_summaries(&self.data_dir, active_project_id.as_deref())
            .map_err(|error| McpError::internal_error(error, None))?;
        serde_json::to_string(&projects)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "Get the isolated folder and editable document, source, and narration paths for one Kokoro Reader project."
    )]
    fn get_reader_project_location(
        &self,
        Parameters(params): Parameters<ProjectIdParams>,
    ) -> Result<String, McpError> {
        let location = project_location(&self.data_dir, &params.project_id)
            .map_err(|error| McpError::invalid_params(error, None))?;
        serde_json::to_string(&location)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "Permanently delete one exact Kokoro Reader project folder and all files inside it, then prune orphaned shared audio cache files. The shared model cache is not affected."
    )]
    fn delete_reader_project(
        &self,
        Parameters(params): Parameters<ProjectIdParams>,
    ) -> Result<String, McpError> {
        let project = project_path(&self.data_dir, &params.project_id)
            .map_err(|error| McpError::invalid_params(error, None))?;
        if !project.is_dir() {
            return Err(McpError::invalid_params(
                format!("Project does not exist: {}", params.project_id),
                None,
            ));
        }
        let next = delete_stored_project(&self.data_dir, &self.cache_dir, &params.project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        let launched_or_refreshed = self.refresh_reader();
        log_event(format!("deleted project {} through MCP", params.project_id));
        serde_json::to_string(&DeleteProjectResult {
            deleted_project_id: params.project_id,
            active_project_id: next.map(|project| project.project_id),
            launched_or_refreshed,
        })
        .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    fn launch_reader(
        &self,
        project_id: &str,
        document: &Document,
        automatic_narration_sections: usize,
    ) -> Result<String, McpError> {
        let launched_or_refreshed = Command::new(
            std::env::current_exe()
                .map_err(|error| McpError::internal_error(error.to_string(), None))?,
        )
        .arg("--refresh-document")
        .spawn()
        .is_ok();
        let directory = project_path(&self.data_dir, project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        serde_json::to_string(&TransferResult {
            project_id: project_id.to_string(),
            directory: directory.to_string_lossy().to_string(),
            accepted_sections: document.sections.len(),
            title: document.title.clone(),
            launched_or_refreshed,
            automatic_narration_sections,
        })
        .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    fn refresh_reader(&self) -> bool {
        Command::new(std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kokoro-reader")))
            .arg("--refresh-document")
            .spawn()
            .is_ok()
    }
}

pub fn run_mcp() -> Result<(), Box<dyn std::error::Error>> {
    install_panic_hook();
    let data_dir = mcp_data_dir()?;
    let cache_dir = mcp_cache_dir()?;
    initialize_audio_cache(&cache_dir)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let service = ReaderMcp {
            data_dir,
            cache_dir,
        }
        .serve(stdio())
        .await?;
        service.waiting().await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_panic_hook();
    configure_default_backend();
    let state = AppState::default();
    tauri::Builder::default()
        .manage(state)
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            let _ = app.emit("document-updated", ());
        }))
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            list_projects,
            list_project_documents,
            get_document,
            select_project,
            save_document,
            restore_previous,
            reload_shared_document,
            has_previous,
            delete_project,
            set_project_read,
            runtime_status,
            download_model,
            audio_cache_status,
            synthesize_sentence
        ])
        .setup(|app| {
            let app_handle = app.handle().clone();
            let state = app.state::<AppState>().inner().clone();
            let paths = app_paths(&app_handle).map_err(std::io::Error::other)?;
            fs::create_dir_all(&paths.data_dir).map_err(std::io::Error::other)?;
            initialize_audio_cache(&paths.cache_dir).map_err(std::io::Error::other)?;
            migrate_legacy_documents(&paths).map_err(std::io::Error::other)?;
            if let Some(project_id) =
                active_or_first_project_id(&paths.document_dir).map_err(std::io::Error::other)?
            {
                let document = read_project(&paths.document_dir, &project_id)
                    .map_err(std::io::Error::other)?;
                set_state_document(&state, &document).map_err(std::io::Error::other)?;
            }
            if model_assets_present(&paths) {
                set_runtime(&state, |runtime| runtime.model_ready = true);
                tauri::async_runtime::spawn(async move {
                    if let Err(error) = ensure_engine(&app_handle, &state).await {
                        log_event(format!("engine initialization failed: {error}"));
                        set_runtime_error(&state, error);
                        emit_runtime_status(&app_handle, &state);
                    }
                });
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn test_directory() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "kokoro-reader-test-{}-{nonce}-{counter}",
            std::process::id()
        ))
    }

    #[test]
    fn markdown_conversion_keeps_content_and_reads_symbols() {
        let spoken = markdown_to_speech("# Heading\n\n**x** → y, where $x \\leq y$.");
        assert!(spoken.contains("Heading"));
        assert!(spoken.contains("leads to"));
        assert!(spoken.contains("less than or equal to"));
        assert!(!spoken.contains("**"));
    }

    #[test]
    fn codex_thread_ids_become_safe_deep_links() {
        assert_eq!(
            codex_url_for_thread_id("  thread-123_abc  "),
            Some("codex://threads/thread-123_abc".to_string())
        );
        assert_eq!(codex_url_for_thread_id("thread/with-slash"), None);
        assert_eq!(codex_url_for_thread_id(""), None);
    }

    #[test]
    fn project_metadata_defaults_and_round_trips_without_touching_document() {
        let project = test_directory();
        fs::create_dir_all(&project).expect("create project directory");
        let document = default_document();
        write_document(&current_document_path(&project), &document).expect("write document");
        let document_bytes = fs::read(current_document_path(&project)).expect("read document");

        assert_eq!(
            read_project_metadata(&project).expect("read default metadata"),
            ProjectMetadata::default()
        );
        let metadata = ProjectMetadata {
            read: true,
            codex_url: Some("codex://threads/thread-123".to_string()),
        };
        write_project_metadata(&project, &metadata).expect("write metadata");
        assert_eq!(
            read_project_metadata(&project).expect("read metadata"),
            metadata
        );
        fs::write(
            project_metadata_path(&project),
            serde_json::to_vec(&ProjectMetadata {
                read: true,
                codex_url: Some("https://example.com".to_string()),
            })
            .expect("serialize invalid URL metadata"),
        )
        .expect("write invalid URL metadata");
        assert_eq!(
            read_project_metadata(&project).expect("read sanitized metadata"),
            ProjectMetadata {
                read: true,
                codex_url: None,
            }
        );
        assert_eq!(
            fs::read(current_document_path(&project)).expect("read unchanged document"),
            document_bytes
        );
        fs::remove_dir_all(project).expect("remove metadata fixture");
    }

    #[test]
    fn project_summaries_include_read_metadata() {
        let directory = test_directory();
        let project_id = create_project_dir(&directory).expect("create project");
        let project = project_path(&directory, &project_id).expect("resolve project");
        replace_current_document(&project, &default_document()).expect("write project");
        write_project_metadata(
            &project,
            &ProjectMetadata {
                read: true,
                codex_url: None,
            },
        )
        .expect("write read metadata");

        let summaries = project_summaries(&directory, None).expect("list summaries");
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].read);
        fs::remove_dir_all(directory).expect("remove summary fixture");
    }

    #[test]
    fn transfer_metadata_resets_read_state_and_relinks_origin() {
        let project = test_directory();
        fs::create_dir_all(&project).expect("create project directory");
        let previous = ProjectMetadata {
            read: true,
            codex_url: Some("codex://threads/old-thread".to_string()),
        };
        let incoming = project_metadata_for_transfer("codex://threads/new-thread".to_string());
        write_project_metadata(&project, &previous).expect("write previous metadata");
        write_project_metadata(&project, &incoming).expect("write incoming metadata");
        assert_eq!(
            read_project_metadata(&project).expect("read incoming metadata"),
            ProjectMetadata {
                read: false,
                codex_url: Some("codex://threads/new-thread".to_string()),
            }
        );
        fs::remove_dir_all(project).expect("remove transfer metadata fixture");
    }

    #[test]
    fn cache_key_changes_with_text_and_voice() {
        assert_eq!(
            cache_key("hello", "af_bella"),
            cache_key("hello", "af_bella")
        );
        assert_ne!(
            cache_key("hello", "af_bella"),
            cache_key("hello!", "af_bella")
        );
        assert_ne!(
            cache_key("hello", "af_bella"),
            cache_key("hello", "am_fenrir")
        );
    }

    #[test]
    fn cache_status_accepts_only_complete_audio_without_touching_lru_time() {
        let directory = test_directory();
        fs::create_dir_all(&directory).expect("create test directory");
        let missing = directory.join("missing.wav");
        let truncated = directory.join("truncated.wav");
        let complete = directory.join("complete.wav");
        fs::write(&truncated, vec![0_u8; 44]).expect("write truncated cache");
        fs::write(&complete, vec![0_u8; 45]).expect("write complete cache");

        assert_eq!(
            cached_audio_size(&missing).expect("inspect missing cache"),
            None
        );
        assert_eq!(
            cached_audio_size(&truncated).expect("inspect truncated cache"),
            None
        );
        let before = fs::metadata(&complete)
            .expect("stat complete cache")
            .modified()
            .expect("read complete cache time");
        assert_eq!(
            cached_audio_size(&complete).expect("inspect complete cache"),
            Some(45)
        );
        let after = fs::metadata(&complete)
            .expect("stat complete cache again")
            .modified()
            .expect("read complete cache time again");
        assert_eq!(before, after);

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn audio_cache_migration_clears_legacy_artifacts_only_once() {
        let directory = test_directory();
        fs::create_dir_all(&directory).expect("create cache directory");
        let legacy_wav = directory.join("legacy.wav");
        let legacy_tmp = directory.join("legacy.tmp");
        let unrelated = directory.join("unrelated.txt");
        fs::write(&legacy_wav, vec![0_u8; 45]).expect("write legacy wav");
        fs::write(&legacy_tmp, vec![0_u8; 1]).expect("write legacy temp");
        fs::write(&unrelated, vec![0_u8; 1]).expect("write unrelated cache file");

        initialize_audio_cache(&directory).expect("initialize audio cache");
        assert!(!legacy_wav.exists());
        assert!(!legacy_tmp.exists());
        assert!(unrelated.exists());
        assert!(directory.join(AUDIO_CACHE_VERSION_MARKER).is_file());

        let current_wav = directory.join("current.wav");
        fs::write(&current_wav, vec![0_u8; 45]).expect("write current wav");
        initialize_audio_cache(&directory).expect("reinitialize audio cache");
        assert!(current_wav.exists());

        fs::remove_dir_all(directory).expect("remove cache fixture");
    }

    #[test]
    fn deleting_projects_prunes_only_unreferenced_audio() {
        let directory = test_directory();
        let cache_directory = directory.join("audio");
        fs::create_dir_all(&cache_directory).expect("create cache directory");
        let first_id = create_project_dir(&directory).expect("create first project");
        let second_id = create_project_dir(&directory).expect("create second project");
        let first_path = project_path(&directory, &first_id).expect("resolve first project");
        let second_path = project_path(&directory, &second_id).expect("resolve second project");
        replace_current_document(&first_path, &default_document()).expect("write first project");
        replace_current_document(&second_path, &default_document()).expect("write second project");
        set_active_project_id(&directory, &first_id).expect("select first project");

        let first_key = cache_key("first only", "af_bella");
        let shared_key = cache_key("shared", "af_bella");
        let orphan_key = cache_key("orphan", "af_bella");
        let first_audio = cache_directory.join(format!("{first_key}.wav"));
        let shared_audio = cache_directory.join(format!("{shared_key}.wav"));
        let orphan_audio = cache_directory.join(format!("{orphan_key}.wav"));
        fs::write(&first_audio, vec![0_u8; 45]).expect("write first audio");
        fs::write(&shared_audio, vec![0_u8; 45]).expect("write shared audio");
        fs::write(&orphan_audio, vec![0_u8; 45]).expect("write orphan audio");
        record_cache_references(&first_path, vec![first_key, shared_key.clone()])
            .expect("record first references");
        record_cache_reference(&second_path, &shared_key).expect("record second reference");

        let next = delete_stored_project(&directory, &cache_directory, &first_id)
            .expect("delete first project")
            .expect("select remaining project");
        assert_eq!(next.project_id, second_id);
        assert!(!first_audio.exists());
        assert!(shared_audio.exists());
        assert!(!orphan_audio.exists());

        assert!(
            delete_stored_project(&directory, &cache_directory, &second_id)
                .expect("delete final project")
                .is_none()
        );
        assert!(!shared_audio.exists());
        fs::remove_dir_all(directory).expect("remove project fixture");
    }

    #[test]
    fn cache_reference_is_safe_before_audio_output_exists() {
        let directory = test_directory();
        let cache_directory = directory.join("audio");
        fs::create_dir_all(&cache_directory).expect("create cache directory");
        let project_id = create_project_dir(&directory).expect("create project");
        let project = project_path(&directory, &project_id).expect("resolve project");
        replace_current_document(&project, &default_document()).expect("write project");
        let key = cache_key("in progress", "af_bella");
        record_cache_reference(&project, &key).expect("record in-progress reference");

        prune_orphaned_audio(&directory, &cache_directory).expect("prune without output");
        let audio = cache_directory.join(format!("{key}.wav"));
        fs::write(&audio, vec![0_u8; 45]).expect("write synthesized audio");
        prune_orphaned_audio(&directory, &cache_directory).expect("prune with reference");
        assert!(audio.exists());

        assert!(
            delete_stored_project(&directory, &cache_directory, &project_id)
                .expect("delete project")
                .is_none()
        );
        assert!(!audio.exists());
        fs::remove_dir_all(directory).expect("remove project fixture");
    }

    #[test]
    fn long_section_leaves_room_for_kokoro_boundary_tokens() {
        let text = "The smallest fix is to extend the existing safe property bridge for one validated activity psi base URL, then expose it as GRL DSL artifact URI base URL project property after the checkout properties are isolated. Rerun the direct Java seventeen Horton test. The configuration problem is fixed when settings evaluation completes and Gradle proceeds to dependency resolution or tests. A full pass still requires exit code zero and restored true.";
        let phonemes = g2p(text, false).expect("G2P should succeed");
        let internal_chunks = kokoro_en::chunk_phonemes(&phonemes, 510);
        assert!(
            get_token_ids(&internal_chunks[0], false).len() > MAX_KOKORO_TOKENS,
            "regression fixture should reproduce the old boundary overflow"
        );

        let safe_chunks = safe_synthesis_chunks(text).expect("safe chunking should succeed");
        assert!(safe_chunks.len() > 1);
        assert!(safe_chunks
            .iter()
            .all(|chunk| chunk.token_count <= MAX_KOKORO_TOKENS));
    }

    #[test]
    fn exact_token_budget_boundary_is_accepted() {
        let boundary = format!("{} cache", ["audio"].repeat(56).join(" "));
        let chunk = checked_synthesis_chunk(&boundary).expect("exact budget should be accepted");
        assert_eq!(chunk.token_count, MAX_KOKORO_TOKENS);
        let over_boundary = format!("{boundary} audio");
        assert!(
            token_count_for_synthesis(&over_boundary).expect("G2P should succeed")
                > MAX_KOKORO_TOKENS
        );
    }

    #[test]
    fn unbreakable_oversized_sentence_returns_error() {
        let text = format!("{}.", "x".repeat(512));
        let error =
            split_oversized_sentence(&text).expect_err("oversized token should be rejected");
        assert!(error.contains("unbreakable token"));
    }

    #[test]
    fn panic_payloads_are_described_without_panicking_again() {
        let string_payload: Box<dyn Any + Send> = Box::new("inference panic");
        assert_eq!(
            panic_payload_message(string_payload.as_ref()),
            "inference panic"
        );

        let owned_payload: Box<dyn Any + Send> = Box::new(String::from("owned panic"));
        assert_eq!(panic_payload_message(owned_payload.as_ref()), "owned panic");

        let other_payload: Box<dyn Any + Send> = Box::new(42_u8);
        assert_eq!(
            panic_payload_message(other_payload.as_ref()),
            "non-string panic payload"
        );
    }

    #[tokio::test]
    async fn inference_panic_can_be_caught() {
        let result = AssertUnwindSafe(async {
            panic!("injected inference panic");
        })
        .catch_unwind()
        .await;
        assert!(result.is_err());
    }

    #[test]
    fn atomic_document_write_and_recovery_rotation_are_readable() {
        let directory = test_directory();
        let current = directory.join("document.json");
        let previous = directory.join("previous-document.json");
        let first = Document {
            title: "First".to_string(),
            sections: vec![DocumentSection {
                markdown: "First".to_string(),
                speech_text: "First".to_string(),
                speech_mode: SpeechMode::Automatic,
            }],
        };
        let second = Document {
            title: "Second".to_string(),
            sections: vec![DocumentSection {
                markdown: "Second".to_string(),
                speech_text: "Second".to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        write_document(&current, &first).expect("write first document");
        atomic_write(&previous, &fs::read(&current).expect("read first document"))
            .expect("rotate first document");
        write_document(&current, &second).expect("write second document");
        assert_eq!(
            read_document(&previous)
                .expect("read previous")
                .expect("previous exists")
                .title,
            "First"
        );
        assert_eq!(
            read_document(&current)
                .expect("read current")
                .expect("current exists")
                .title,
            "Second"
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn legacy_single_document_is_migrated_into_an_isolated_project() {
        let base = test_directory();
        let document_dir = base.join("documents");
        let legacy_dir = base.join("legacy");
        fs::create_dir_all(&document_dir).expect("create document directory");
        fs::create_dir_all(&legacy_dir).expect("create legacy directory");
        let legacy_document = Document {
            title: "Migrated title".to_string(),
            sections: vec![DocumentSection {
                markdown: "# Migrated".to_string(),
                speech_text: "Migrated narration".to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        write_document(&current_document_path(&document_dir), &legacy_document)
            .expect("write legacy document");
        write_shared_document_files(&document_dir, &legacy_document)
            .expect("write legacy shared files");

        migrate_legacy_documents(&StoragePaths {
            data_dir: legacy_dir,
            document_dir: document_dir.clone(),
            model_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
        })
        .expect("migrate legacy document");

        let project_ids = list_project_ids(&document_dir).expect("list migrated projects");
        assert_eq!(project_ids, vec![MIGRATED_PROJECT_ID.to_string()]);
        assert_eq!(
            read_project(&document_dir, MIGRATED_PROJECT_ID)
                .expect("read migrated project")
                .title,
            "Migrated title"
        );
        assert_eq!(
            project_document(&document_dir, MIGRATED_PROJECT_ID)
                .expect("read migrated metadata defaults")
                .metadata,
            ProjectMetadata::default()
        );
        assert_eq!(
            read_active_project_id(&document_dir).expect("read active project"),
            Some(MIGRATED_PROJECT_ID.to_string())
        );
        assert!(
            source_document_path(&project_path(&document_dir, MIGRATED_PROJECT_ID).unwrap())
                .is_file()
        );
        fs::remove_dir_all(base).expect("remove migration fixture");
    }

    #[test]
    fn projects_keep_updates_and_shared_files_isolated() {
        let directory = test_directory();
        fs::create_dir_all(&directory).expect("create project directory");
        let first_id = create_project_dir(&directory).expect("create first project");
        let second_id = create_project_dir(&directory).expect("create second project");
        let first = Document {
            title: "First".to_string(),
            sections: vec![DocumentSection {
                markdown: "First markdown".to_string(),
                speech_text: "First speech".to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        let second = Document {
            title: "Second".to_string(),
            sections: vec![DocumentSection {
                markdown: "Second markdown".to_string(),
                speech_text: "Second speech".to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        replace_current_document(&project_path(&directory, &first_id).unwrap(), &first)
            .expect("write first project");
        replace_current_document(&project_path(&directory, &second_id).unwrap(), &second)
            .expect("write second project");
        let updated = Document {
            title: "Updated first".to_string(),
            ..first.clone()
        };
        replace_current_document(&project_path(&directory, &first_id).unwrap(), &updated)
            .expect("update first project");

        assert_eq!(
            read_project(&directory, &first_id)
                .expect("read first project")
                .title,
            "Updated first"
        );
        assert_eq!(
            read_project(&directory, &second_id)
                .expect("read second project")
                .title,
            "Second"
        );
        assert!(previous_document_path(&project_path(&directory, &first_id).unwrap()).is_file());
        assert!(!previous_document_path(&project_path(&directory, &second_id).unwrap()).is_file());
        fs::remove_dir_all(directory).expect("remove project fixture");
    }

    #[test]
    fn project_documents_are_newest_first_without_changing_the_active_project() {
        let directory = test_directory();
        let first_id = create_project_dir(&directory).expect("create first project");
        let second_id = create_project_dir(&directory).expect("create second project");
        replace_current_document(
            &project_path(&directory, &first_id).unwrap(),
            &default_document(),
        )
        .expect("write first project");
        std::thread::sleep(std::time::Duration::from_millis(2));
        replace_current_document(
            &project_path(&directory, &second_id).unwrap(),
            &default_document(),
        )
        .expect("write second project");
        set_active_project_id(&directory, &first_id).expect("select first project");

        let documents = project_documents(&directory).expect("list project documents");
        assert_eq!(
            documents
                .iter()
                .map(|project| &project.project_id)
                .collect::<Vec<_>>(),
            vec![&second_id, &first_id]
        );
        assert_eq!(
            read_active_project_id(&directory).expect("read active project"),
            Some(first_id)
        );
        fs::remove_dir_all(directory).expect("remove project fixture");
    }

    #[test]
    fn deleted_project_is_rejected_before_cache_write() {
        let directory = test_directory();
        let project_id = create_project_dir(&directory).expect("create project");
        let project = project_path(&directory, &project_id).expect("resolve project");
        fs::remove_dir_all(&project).expect("delete project");
        assert!(ensure_project_exists(&project, &project_id).is_err());
        fs::remove_dir_all(directory).expect("remove project fixture");
    }

    #[test]
    fn empty_store_stays_empty_until_a_transfer_creates_a_project() {
        assert!(valid_project_id("project-123_abc"));
        assert!(!valid_project_id("../escape"));
        assert!(!valid_project_id(""));
        let directory = test_directory();
        assert_eq!(
            active_or_first_project_id(&directory).expect("select project"),
            None
        );
        assert!(!active_project_path(&directory).exists());
        let project_id = create_project_dir(&directory).expect("create transfer project");
        replace_current_document(
            &project_path(&directory, &project_id).unwrap(),
            &default_document(),
        )
        .expect("write transfer document");
        assert_eq!(
            active_or_first_project_id(&directory).expect("select transfer project"),
            Some(project_id)
        );
        fs::remove_dir_all(directory).expect("remove empty fixture");
    }

    #[test]
    fn deleting_projects_selects_the_next_project_then_leaves_the_store_empty() {
        let directory = test_directory();
        let first_id = create_project_dir(&directory).expect("create first project");
        let second_id = create_project_dir(&directory).expect("create second project");
        let first_path = project_path(&directory, &first_id).expect("resolve first project");
        let second_path = project_path(&directory, &second_id).expect("resolve second project");
        replace_current_document(
            &first_path,
            &Document {
                title: "First".to_string(),
                sections: vec![],
            },
        )
        .expect("write first project");
        replace_current_document(
            &second_path,
            &Document {
                title: "Second".to_string(),
                sections: vec![],
            },
        )
        .expect("write second project");
        set_active_project_id(&directory, &first_id).expect("select first project");

        let cache_directory = directory.join("audio");
        let next = delete_stored_project(&directory, &cache_directory, &first_id)
            .expect("delete first project")
            .expect("select remaining project");
        assert_eq!(next.project_id, second_id);
        assert_eq!(
            read_active_project_id(&directory).expect("read active project"),
            Some(second_id.clone())
        );
        assert!(!first_path.exists());

        assert!(
            delete_stored_project(&directory, &cache_directory, &second_id)
                .expect("delete final project")
                .is_none()
        );
        assert!(list_project_ids(&directory)
            .expect("list projects")
            .is_empty());
        assert!(!active_project_path(&directory).exists());
        assert!(!second_path.exists());
        fs::remove_dir_all(directory).expect("remove deletion fixture");
    }

    #[test]
    fn shared_files_keep_sections_aligned() {
        let directory = test_directory();
        let document = Document {
            title: "Shared".to_string(),
            sections: vec![
                DocumentSection {
                    markdown: "# First".to_string(),
                    speech_text: "First, naturally.".to_string(),
                    speech_mode: SpeechMode::Custom,
                },
                DocumentSection {
                    markdown: "# Second".to_string(),
                    speech_text: markdown_to_speech("# Second"),
                    speech_mode: SpeechMode::Automatic,
                },
            ],
        };
        write_shared_document_files(&directory, &document).expect("write shared files");
        let source = fs::read_to_string(source_document_path(&directory)).expect("read source");
        let narration =
            fs::read_to_string(narration_document_path(&directory)).expect("read narration");
        let (reloaded, automatic) =
            document_from_shared_files("Shared".to_string(), &source, Some(&narration))
                .expect("reload shared files");
        assert_eq!(reloaded.sections.len(), 2);
        assert_eq!(automatic, 1);
        assert_eq!(reloaded.sections[0].speech_text, "First, naturally.");
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn precheck_reports_alignment_and_marker_mode() {
        let source =
            "<!-- kokoro-reader-section -->\n# First\n<!-- kokoro-reader-section -->\n# Second";
        let narration = "First narration.\n---\nSecond narration.";
        let result = precheck_document_contents(source, narration).expect("precheck should pass");
        assert_eq!(result.source_sections, 2);
        assert_eq!(result.narration_sections, 2);
        assert!(result.explicit_source_markers);
        assert!(result.warnings.is_empty());
        assert!(result.ready_for_send);
    }

    #[test]
    fn staging_append_creates_and_keeps_files_aligned() {
        let base = test_directory();
        let inbox = base.join("inbox");
        fs::create_dir_all(&inbox).expect("create staging inbox");
        let source = inbox.join("lesson-source.md");
        let narration = inbox.join("lesson-narration.txt");
        let first = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: Some(0),
                sections: vec![
                    IncomingSection {
                        markdown: "# First\r\n".to_string(),
                        speech_text: "First narration.\r\n".to_string(),
                    },
                    IncomingSection {
                        markdown: "# Second".to_string(),
                        speech_text: "Second narration.".to_string(),
                    },
                ],
            },
        )
        .expect("first staging append");
        assert_eq!(first.appended_sections, 2);
        assert_eq!(first.total_sections, 2);
        let source_text = fs::read_to_string(&source).expect("read source");
        let narration_text = fs::read_to_string(&narration).expect("read narration");
        assert!(source_text.contains("<!-- kokoro-reader-section -->"));
        assert!(!source_text.contains('\r'));
        let document = document_from_mcp_files("Test".to_string(), &source_text, &narration_text)
            .expect("read synchronized staging files");
        assert_eq!(document.sections.len(), 2);

        let second = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: Some(2),
                sections: vec![IncomingSection {
                    markdown: "  # Third  ".to_string(),
                    speech_text: "  Third narration.  ".to_string(),
                }],
            },
        )
        .expect("second staging append");
        assert_eq!(second.appended_sections, 1);
        assert_eq!(second.total_sections, 3);
        let source_text = fs::read_to_string(&source).expect("read appended source");
        let narration_text = fs::read_to_string(&narration).expect("read appended narration");
        let precheck = precheck_document_contents(&source_text, &narration_text)
            .expect("precheck appended files");
        assert_eq!(precheck.source_sections, 3);
        assert_eq!(precheck.narration_sections, 3);
        fs::remove_dir_all(base).expect("remove staging fixture");
    }

    #[test]
    fn staging_append_rejects_mismatched_pair_and_stale_count() {
        let base = test_directory();
        let inbox = base.join("inbox");
        fs::create_dir_all(&inbox).expect("create staging inbox");
        let source = inbox.join("lesson-source.md");
        let narration = inbox.join("lesson-narration.txt");
        fs::write(&source, "# Existing").expect("write partial pair");
        let error = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: None,
                sections: vec![IncomingSection {
                    markdown: "# Next".to_string(),
                    speech_text: "Next narration.".to_string(),
                }],
            },
        )
        .expect_err("partial pair must be rejected");
        assert!(error.contains("created or present together"));
        assert!(!narration.exists());

        fs::remove_file(&source).expect("remove partial source");
        let first = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: Some(0),
                sections: vec![IncomingSection {
                    markdown: "# Existing".to_string(),
                    speech_text: "Existing narration.".to_string(),
                }],
            },
        )
        .expect("create pair");
        assert_eq!(first.total_sections, 1);
        let error = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: Some(0),
                sections: vec![IncomingSection {
                    markdown: "# Next".to_string(),
                    speech_text: "Next narration.".to_string(),
                }],
            },
        )
        .expect_err("stale count must be rejected");
        assert!(error.contains("expected 0 existing sections but found 1"));
        assert_eq!(
            document_from_mcp_files(
                "Test".to_string(),
                &fs::read_to_string(&source).expect("read source"),
                &fs::read_to_string(&narration).expect("read narration"),
            )
            .expect("read unchanged pair")
            .sections
            .len(),
            1
        );
        fs::remove_dir_all(base).expect("remove staging fixture");
    }

    #[test]
    fn staging_append_rejects_blank_narration_and_oversized_document() {
        let base = test_directory();
        let inbox = base.join("inbox");
        fs::create_dir_all(&inbox).expect("create staging inbox");
        let source = inbox.join("lesson-source.md");
        let narration = inbox.join("lesson-narration.txt");
        let error = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: None,
                sections: vec![IncomingSection {
                    markdown: "# First".to_string(),
                    speech_text: "  ".to_string(),
                }],
            },
        )
        .expect_err("blank narration must be rejected");
        assert!(error.contains("speech_text must not be empty"));
        assert!(!source.exists());
        assert!(!narration.exists());

        let error = append_to_staging_files_in(
            &inbox,
            AppendToReaderFilesParams {
                source_path: source.to_string_lossy().to_string(),
                narration_path: narration.to_string_lossy().to_string(),
                expected_section_count: None,
                sections: vec![IncomingSection {
                    markdown: "x".repeat(MAX_DOCUMENT_BYTES),
                    speech_text: "Narration".to_string(),
                }],
            },
        )
        .expect_err("oversized document must be rejected");
        assert!(error.contains("Document is too large"));
        assert!(!source.exists());
        assert!(!narration.exists());
        fs::remove_dir_all(base).expect("remove staging fixture");
    }

    #[test]
    fn mcp_file_transfer_rejects_blank_narration_sections() {
        let source =
            "<!-- kokoro-reader-section -->\n# First\n<!-- kokoro-reader-section -->\n# Second";
        let error =
            document_from_mcp_files("MCP".to_string(), source, "First custom narration.\n---\n")
                .expect_err("blank narration must be rejected");
        assert!(error.contains("MCP narration is required"));
    }

    #[test]
    fn mcp_file_transfer_marks_all_sections_custom() {
        let source =
            "<!-- kokoro-reader-section -->\n# First\n<!-- kokoro-reader-section -->\n# Second";
        let document = document_from_mcp_files(
            "MCP".to_string(),
            source,
            "First custom narration.\n---\nSecond custom narration.",
        )
        .expect("custom narration should be accepted");
        assert!(document
            .sections
            .iter()
            .all(|section| section.speech_mode == SpeechMode::Custom));
    }

    #[test]
    fn mcp_params_require_narration_fields() {
        assert!(serde_json::from_str::<SendToReaderParams>(
            r#"{"sections":[{"markdown":"Heading"}]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<SendFileToReaderParams>(
            r#"{"source_path":"/tmp/source.md"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<AppendToReaderFilesParams>(
            r#"{"source_path":"/tmp/source.md","narration_path":"/tmp/narration.txt"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<PrecheckReaderFilesParams>(
            r#"{"source_path":"/tmp/source.md"}"#
        )
        .is_err());
    }

    #[test]
    fn mcp_params_require_codex_backlink() {
        assert!(serde_json::from_str::<SendToReaderParams>(r#"{"sections":[]}"#).is_err());
        assert!(serde_json::from_str::<SendFileToReaderParams>(
            r#"{"source_path":"/tmp/source.md","narration_path":"/tmp/narration.txt"}"#
        )
        .is_err());
        assert!(required_codex_url("https://example.com").is_err());
        assert_eq!(
            required_codex_url(" codex://threads/thread-123 ").expect("valid backlink"),
            "codex://threads/thread-123"
        );
    }

    #[test]
    fn unmarked_source_falls_back_to_paragraph_sections_without_breaking_code() {
        let sections = split_paragraph_sections(
            "# Heading\n\nFirst paragraph.\n\n```text\nline one\n\nline two\n```\n\nLast paragraph.",
        );
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0], "# Heading\n\nFirst paragraph.");
        assert!(sections[1].contains("line one\n\nline two"));
        assert_eq!(sections[2], "Last paragraph.");
    }

    #[test]
    fn rotating_log_keeps_one_bounded_backup() {
        let directory = test_directory();
        fs::create_dir_all(&directory).expect("create test directory");
        let path = directory.join("kokoro-reader.log");
        fs::write(&path, vec![b'x'; LOG_MAX_BYTES as usize]).expect("seed log");
        rotate_log_if_needed(&path, 1).expect("rotate log");
        assert!(!path.exists());
        assert!(path.with_extension("log.1").is_file());
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn wav_writer_emits_pcm_header() {
        let directory = test_directory();
        let path = directory.join("audio.wav");
        write_wav(&path, &[0.0, 0.5, -0.5]).expect("write wav");
        let bytes = fs::read(&path).expect("read wav");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 50);
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
