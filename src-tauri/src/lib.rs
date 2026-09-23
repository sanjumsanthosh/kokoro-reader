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
    collections::{BTreeMap, BTreeSet},
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
use toon_format::decode_default;

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
const PRONUNCIATIONS_FILE: &str = "pronunciations.json";
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
    pub created_at: u64,
    pub active: bool,
    pub read: bool,
    pub read_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProjectMetadata {
    pub read: bool,
    pub created_at: Option<u64>,
    pub read_at: Option<u64>,
    pub codex_url: Option<String>,
}

impl Default for ProjectMetadata {
    fn default() -> Self {
        Self {
            read: false,
            created_at: None,
            read_at: None,
            codex_url: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectDocument {
    pub project_id: String,
    pub document: Document,
    pub metadata: ProjectMetadata,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectLocation {
    pub project_id: String,
    pub title: String,
    pub directory: String,
    pub document_path: String,
    pub source_path: String,
    pub narration_path: String,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize)]
struct DeleteProjectResult {
    deleted_project_id: String,
    active_project_id: Option<String>,
    launched_or_refreshed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct StorageArea {
    pub path: String,
    pub bytes: u64,
    pub exists: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectCacheUsage {
    pub project_id: String,
    pub title: String,
    pub bytes: u64,
    pub cached_clips: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct StorageStats {
    pub total_bytes: u64,
    pub models: StorageArea,
    pub audio_cache: StorageArea,
    pub project_files: StorageArea,
    pub app_data: StorageArea,
    pub projects: Vec<ProjectCacheUsage>,
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

#[derive(Clone, Debug, Deserialize, JsonSchema)]
struct PrecheckAcknowledgement {
    #[schemars(description = "Content-bound token returned by the latest precheck")]
    token: String,
    #[schemars(
        description = "Why the remaining findings do not make the lesson missing or unreadable"
    )]
    reason: String,
    #[schemars(
        description = "Concrete checks the agent verified before acknowledging the findings"
    )]
    verified_checks: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendToReaderParams {
    #[schemars(description = "Required Codex backlink in the form codex://threads/<thread-id>")]
    codex_url: String,
    #[schemars(description = "Optional stable project ID returned by an earlier transfer")]
    project_id: Option<String>,
    #[schemars(
        description = "Required revision when safely replacing an existing project; omit only for legacy callers"
    )]
    expected_revision: Option<String>,
    #[schemars(description = "Optional document title")]
    title: Option<String>,
    #[schemars(description = "Optional acknowledgement for current precheck findings")]
    precheck_acknowledgement: Option<PrecheckAcknowledgement>,
    #[schemars(description = "Ordered visual Markdown and matching narration sections")]
    sections: Vec<IncomingSection>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendFileToReaderParams {
    #[schemars(description = "Required Codex backlink in the form codex://threads/<thread-id>")]
    codex_url: String,
    #[schemars(description = "Optional stable project ID returned by an earlier transfer")]
    project_id: Option<String>,
    #[schemars(
        description = "Required revision when safely replacing an existing project; omit only for legacy callers"
    )]
    expected_revision: Option<String>,
    #[schemars(description = "Optional document title")]
    title: Option<String>,
    #[schemars(description = "Absolute path to source Markdown under ~/.kokoro_reader")]
    source_path: String,
    #[schemars(
        description = "Required absolute path to matching narration text under ~/.kokoro_reader; every section must be non-empty"
    )]
    narration_path: String,
    #[schemars(description = "Optional acknowledgement for current precheck findings")]
    precheck_acknowledgement: Option<PrecheckAcknowledgement>,
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
struct SendToonPacketParams {
    #[schemars(description = "Required Codex backlink in the form codex://threads/<thread-id>")]
    codex_url: String,
    #[schemars(description = "Optional stable project ID returned by an earlier transfer")]
    project_id: Option<String>,
    expected_revision: Option<String>,
    #[schemars(description = "Optional document title that overrides the packet title")]
    title: Option<String>,
    #[schemars(description = "Strict TOON packet containing a title and uniform teaching cards")]
    packet_toon: String,
    #[schemars(description = "Optional acknowledgement for current precheck findings")]
    precheck_acknowledgement: Option<PrecheckAcknowledgement>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendGmailDigestToonParams {
    #[schemars(description = "Persistent Gmail batch ID, such as gmail-20260920T064421Z-c33583cf")]
    batch_id: String,
    #[schemars(description = "Required Codex backlink in the form codex://threads/<thread-id>")]
    codex_url: String,
    #[schemars(description = "Optional stable project ID returned by an earlier transfer")]
    project_id: Option<String>,
    expected_revision: Option<String>,
    #[schemars(description = "Optional document title that overrides the packet title")]
    title: Option<String>,
    #[schemars(
        description = "Strict TOON packet containing a title and uniform Gmail digest clusters"
    )]
    clusters_toon: String,
    #[schemars(description = "Optional acknowledgement for current precheck findings")]
    precheck_acknowledgement: Option<PrecheckAcknowledgement>,
}

#[derive(Debug, Deserialize)]
struct ToonPacket {
    title: Option<String>,
    cards: Vec<ToonCard>,
}

#[derive(Debug, Deserialize)]
struct ToonCard {
    id: String,
    heading: String,
    claim: String,
    connection: Option<String>,
    diagram: Option<String>,
    pause_question: Option<String>,
    answer: Option<String>,
    narration: String,
}

#[derive(Debug, Deserialize)]
struct GmailDigestPacket {
    title: Option<String>,
    clusters: Vec<GmailDigestCluster>,
}

#[derive(Debug, Deserialize)]
struct GmailDigestCluster {
    id: String,
    title: String,
    verdict: String,
    narrative: String,
    narration: String,
    primary_url: Option<String>,
    message_ids: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectIdParams {
    #[schemars(description = "Stable project ID")]
    project_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PrecheckSectionInput {
    #[schemars(description = "One-based section index")]
    section_index: usize,
    #[schemars(description = "Visual Markdown for the section")]
    markdown: String,
    #[schemars(description = "Natural narration for the section")]
    speech_text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PrecheckSectionsParams {
    sections: Vec<PrecheckSectionInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReaderSectionUpdate {
    #[schemars(description = "One-based existing section index")]
    section_index: usize,
    #[schemars(description = "Replacement visual Markdown")]
    markdown: String,
    #[schemars(description = "Replacement natural narration")]
    speech_text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateReaderProjectSectionsParams {
    #[schemars(description = "Stable project ID")]
    project_id: String,
    #[schemars(description = "Revision returned by the latest project read")]
    expected_revision: String,
    #[schemars(description = "One or more existing sections to replace")]
    updates: Vec<ReaderSectionUpdate>,
    #[schemars(description = "Optional acknowledgement for current precheck findings")]
    precheck_acknowledgement: Option<PrecheckAcknowledgement>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SetReaderPronunciationsParams {
    #[schemars(
        description = "One or more technical terms and their exact narration-ready spoken forms"
    )]
    entries: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RemoveReaderPronunciationsParams {
    #[schemars(
        description = "One or more technical terms to remove from the shared pronunciation glossary"
    )]
    terms: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ReaderPronunciations {
    pronunciation_path: String,
    entries: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
struct TransferResult {
    project_id: String,
    directory: String,
    accepted_sections: usize,
    title: String,
    launched_or_refreshed: bool,
    automatic_narration_sections: usize,
    revision: String,
    source_path: String,
    narration_path: String,
    precheck_overridden: bool,
    acknowledged_findings: Vec<String>,
    acknowledgement_reason: Option<String>,
    verified_checks: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct FilePrecheckResult {
    source_sections: usize,
    narration_sections: usize,
    document_bytes: usize,
    max_document_bytes: usize,
    explicit_source_markers: bool,
    warnings: Vec<String>,
    alignment: AlignmentSummary,
    narration_quality: NarrationQualitySummary,
    ready_for_send: bool,
    override_available: bool,
    override_token: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct AlignmentSection {
    section_index: usize,
    spoken_grounding: f32,
    visual_coverage: f32,
    shared_terms: usize,
    missing_visual_terms: Vec<String>,
    missing_line_references: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Debug, Serialize)]
struct TextRange {
    start_utf16: usize,
    end_utf16: usize,
}

#[derive(Clone, Debug, Serialize)]
struct SectionDiagnostic {
    code: String,
    severity: DiagnosticSeverity,
    message: String,
    source_range: Option<TextRange>,
    narration_range: Option<TextRange>,
}

#[derive(Clone, Debug, Serialize)]
struct CoverageBlockResult {
    kind: String,
    label: String,
    source_range: TextRange,
    narration_range: Option<TextRange>,
    shared_terms: Vec<String>,
    missing_terms: Vec<String>,
    covered: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SectionPrecheckResult {
    section_index: usize,
    spoken_grounding: f32,
    visual_coverage: f32,
    shared_terms: usize,
    visual_terms: usize,
    narration_terms: usize,
    required_visual_shared_terms: usize,
    required_grounded_shared_terms: usize,
    diagnostics: Vec<SectionDiagnostic>,
    coverage_blocks: Vec<CoverageBlockResult>,
    ready: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SectionsPrecheckResult {
    sections: Vec<SectionPrecheckResult>,
    ready_for_send: bool,
    override_available: bool,
    override_token: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct AlignmentSummary {
    sections: Vec<AlignmentSection>,
    ready: bool,
}

#[derive(Clone, Debug, Serialize)]
struct NarrationQualitySection {
    section_index: usize,
    issues: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct NarrationQualitySummary {
    sections: Vec<NarrationQualitySection>,
    ready: bool,
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

fn pronunciation_file_path(document_dir: &Path) -> Result<PathBuf, String> {
    let root = document_dir
        .parent()
        .ok_or_else(|| format!("No shared root directory for {}", document_dir.display()))?;
    Ok(root.join(PRONUNCIATIONS_FILE))
}

fn normalize_pronunciation_entries(
    entries: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if entries.is_empty() {
        return Err("At least one pronunciation entry is required".to_string());
    }
    entries
        .into_iter()
        .map(|(term, spoken)| {
            let term = term.trim().to_ascii_lowercase();
            let spoken = spoken.trim().to_string();
            if term.is_empty() || spoken.is_empty() {
                Err("Pronunciation terms and spoken forms must not be empty".to_string())
            } else {
                Ok((term, spoken))
            }
        })
        .collect()
}

fn read_pronunciations(path: &Path) -> Result<BTreeMap<String, String>, String> {
    match fs::read(path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map_err(|error| format!("Could not parse {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.to_string()),
    }
}

fn write_pronunciations(path: &Path, entries: &BTreeMap<String, String>) -> Result<(), String> {
    let contents = serde_json::to_vec_pretty(entries)
        .map_err(|error| format!("Could not serialize pronunciations: {error}"))?;
    atomic_write(path, &contents)
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
        created_at: Some(epoch_seconds()),
        read_at: None,
        codex_url: Some(codex_url),
    }
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn file_timestamp(path: &Path) -> Result<u64, String> {
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    let timestamp = metadata
        .created()
        .or_else(|_| metadata.modified())
        .map_err(|error| error.to_string())?;
    Ok(timestamp
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs())
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
    let stored = read_document(&current_document_path(&path))?
        .ok_or_else(|| format!("Project has no document: {project_id}"))?;
    match (
        fs::read_to_string(source_document_path(&path)),
        fs::read_to_string(narration_document_path(&path)),
    ) {
        (Ok(source), Ok(narration)) => {
            let (stored_source, stored_narration) = shared_document_file_contents(&stored);
            if source == stored_source && narration == stored_narration {
                return Ok(stored);
            }
            let (external, _) =
                document_from_shared_files(stored.title.clone(), &source, Some(&narration))?;
            write_document(&current_document_path(&path), &external)?;
            Ok(external)
        }
        (Err(source_error), Err(narration_error))
            if source_error.kind() == std::io::ErrorKind::NotFound
                && narration_error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(stored)
        }
        (Err(error), _) | (_, Err(error)) => {
            Err(format!("Could not read canonical project files: {error}"))
        }
    }
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
            let document_path = current_document_path(&path);
            let updated_at = fs::metadata(&document_path)
                .map_err(|error| error.to_string())?
                .modified()
                .map_err(|error| error.to_string())?
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let metadata = read_project_metadata(&path)?;
            Ok(ProjectSummary {
                active: active_project_id == Some(project_id.as_str()),
                project_id,
                title: document.title,
                updated_at,
                created_at: metadata
                    .created_at
                    .unwrap_or(file_timestamp(&document_path)?),
                read: metadata.read,
                read_at: metadata.read_at,
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
        revision: project_revision(&project)?,
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
        revision: project_revision(&directory)?,
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
    delete_stored_projects(document_dir, cache_dir, &[project_id.to_string()])
}

fn delete_stored_projects(
    document_dir: &Path,
    cache_dir: &Path,
    project_ids: &[String],
) -> Result<Option<ProjectDocument>, String> {
    if project_ids.is_empty() {
        return Err("Select at least one project to delete".to_string());
    }
    let _lock = CACHE_REFERENCES_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| "Cache reference lock poisoned".to_string())?;
    let mut paths = Vec::with_capacity(project_ids.len());
    let mut seen = BTreeSet::new();
    for project_id in project_ids {
        if !seen.insert(project_id) {
            return Err("Duplicate project ID in delete request".to_string());
        }
        let project = project_path(document_dir, project_id)?;
        ensure_project_exists(&project, project_id)?;
        paths.push(project);
    }
    for project in paths {
        fs::remove_dir_all(&project).map_err(|error| error.to_string())?;
    }
    prune_orphaned_audio(document_dir, cache_dir)?;
    let Some(next_project_id) = active_or_first_project_id(document_dir)? else {
        return Ok(None);
    };
    Ok(Some(project_document(document_dir, &next_project_id)?))
}

fn directory_size(path: &Path) -> Result<u64, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(0),
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .try_fold(0_u64, |total, entry| {
                let entry = entry.map_err(|error| error.to_string())?;
                total
                    .checked_add(directory_size(&entry.path())?)
                    .ok_or_else(|| "Storage total is too large".to_string())
            }),
    }
}

fn directory_size_excluding(path: &Path, excluded: &[&Path]) -> Result<u64, String> {
    if excluded.iter().any(|candidate| *candidate == path) {
        return Ok(0);
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(0),
        Ok(metadata) if metadata.is_file() => Ok(metadata.len()),
        Ok(_) => fs::read_dir(path)
            .map_err(|error| error.to_string())?
            .try_fold(0_u64, |total, entry| {
                let entry = entry.map_err(|error| error.to_string())?;
                total
                    .checked_add(directory_size_excluding(&entry.path(), excluded)?)
                    .ok_or_else(|| "Storage total is too large".to_string())
            }),
    }
}

fn storage_area(path: &Path, bytes: u64) -> StorageArea {
    StorageArea {
        path: path.to_string_lossy().to_string(),
        bytes,
        exists: path.exists(),
    }
}

fn project_cache_usage(
    document_dir: &Path,
    cache_dir: &Path,
) -> Result<Vec<ProjectCacheUsage>, String> {
    let mut usage = Vec::new();
    for project_id in list_project_ids(document_dir)? {
        let project = project_path(document_dir, &project_id)?;
        let mut bytes = 0_u64;
        let mut cached_clips = 0_usize;
        for key in read_cache_references(&project)? {
            if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            if let Some(size) = cached_audio_size(&cache_dir.join(format!("{key}.wav")))? {
                bytes = bytes
                    .checked_add(size)
                    .ok_or_else(|| "Storage total is too large".to_string())?;
                cached_clips += 1;
            }
        }
        usage.push(ProjectCacheUsage {
            project_id: project_id.clone(),
            title: read_project(document_dir, &project_id)?.title,
            bytes,
            cached_clips,
        });
    }
    usage.sort_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(usage)
}

fn storage_stats_for_paths(paths: &StoragePaths) -> Result<StorageStats, String> {
    let models = directory_size(&paths.model_dir)?;
    let audio_cache = directory_size(&paths.cache_dir)?;
    let project_files = directory_size(&paths.document_dir)?;
    let app_data = directory_size_excluding(&paths.data_dir, &[paths.model_dir.as_path()])?;
    let total_bytes = models
        .checked_add(audio_cache)
        .and_then(|total| total.checked_add(project_files))
        .and_then(|total| total.checked_add(app_data))
        .ok_or_else(|| "Storage total is too large".to_string())?;
    Ok(StorageStats {
        total_bytes,
        models: storage_area(&paths.model_dir, models),
        audio_cache: storage_area(&paths.cache_dir, audio_cache),
        project_files: storage_area(&paths.document_dir, project_files),
        app_data: storage_area(&paths.data_dir, app_data),
        projects: project_cache_usage(&paths.document_dir, &paths.cache_dir)?,
    })
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

fn document_revision(source: &str, narration: &str) -> String {
    blake3::hash(format!("{source}\n\0\n{narration}").as_bytes())
        .to_hex()
        .to_string()
}

fn project_revision(project: &Path) -> Result<String, String> {
    match (
        fs::read_to_string(source_document_path(project)),
        fs::read_to_string(narration_document_path(project)),
    ) {
        (Ok(source), Ok(narration)) => Ok(document_revision(&source, &narration)),
        (Err(source_error), Err(narration_error))
            if source_error.kind() == std::io::ErrorKind::NotFound
                && narration_error.kind() == std::io::ErrorKind::NotFound =>
        {
            let document = read_document(&current_document_path(project))?
                .ok_or_else(|| "Project has no document".to_string())?;
            let (source, narration) = shared_document_file_contents(&document);
            Ok(document_revision(&source, &narration))
        }
        (Err(error), _) | (_, Err(error)) => {
            Err(format!("Could not read project draft files: {error}"))
        }
    }
}

fn ensure_project_revision(project: &Path, expected_revision: Option<&str>) -> Result<(), String> {
    let Some(expected_revision) = expected_revision else {
        return Ok(());
    };
    let actual = project_revision(project)?;
    if actual == expected_revision {
        Ok(())
    } else {
        Err("STALE_DRAFT: project files changed on disk; reload before saving".to_string())
    }
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

fn required_text(value: &str, field: &str, id: &str) -> Result<String, String> {
    let value = normalize_section_text(value.to_string());
    if value.is_empty() {
        return Err(format!("TOON {field} must not be empty for card {id}"));
    }
    Ok(value)
}

fn optional_text(value: Option<String>) -> Option<String> {
    value
        .map(normalize_section_text)
        .filter(|value| !value.is_empty())
}

fn render_toon_packet(
    packet_toon: &str,
    title_override: Option<String>,
) -> Result<Document, String> {
    let packet: ToonPacket =
        decode_default(packet_toon).map_err(|error| format!("Invalid TOON packet: {error}"))?;
    if packet.cards.is_empty() {
        return Err("TOON packet cards must not be empty".to_string());
    }
    let mut ids = BTreeSet::new();
    let mut sections = Vec::with_capacity(packet.cards.len());
    for card in packet.cards {
        let id = required_text(&card.id, "card id", "<unknown>")?;
        if !ids.insert(id.clone()) {
            return Err(format!("Duplicate TOON card id: {id}"));
        }
        let heading = required_text(&card.heading, "heading", &id)?;
        let claim = required_text(&card.claim, "claim", &id)?;
        let narration = required_text(&card.narration, "narration", &id)?;
        let mut markdown = format!("# {heading}\n\n{claim}");
        if let Some(connection) = optional_text(card.connection) {
            markdown.push_str(&format!(
                "\n\n**Connection to what you already know:** {connection}"
            ));
        }
        if let Some(diagram) = optional_text(card.diagram) {
            markdown.push_str(&format!("\n\n```text\n{diagram}\n```"));
        }
        match (
            optional_text(card.pause_question),
            optional_text(card.answer),
        ) {
            (Some(question), Some(answer)) => {
                markdown.push_str(&format!(
                    "\n\n**Pause and predict:** {question}\n\n**Answer:** {answer}"
                ));
            }
            (None, None) => {}
            _ => {
                return Err(format!(
                    "TOON pause_question and answer must be supplied together for card {id}"
                ))
            }
        }
        sections.push(DocumentSection {
            markdown,
            speech_text: narration,
            speech_mode: SpeechMode::Custom,
        });
    }
    let title = optional_text(title_override)
        .or_else(|| optional_text(packet.title))
        .unwrap_or_else(|| "Codex reading".to_string());
    let document = Document { title, sections };
    validate_document(&document)?;
    let alignment = alignment_summary(
        &document
            .sections
            .iter()
            .map(|section| section.markdown.clone())
            .collect::<Vec<_>>(),
        &document
            .sections
            .iter()
            .map(|section| section.speech_text.clone())
            .collect::<Vec<_>>(),
    );
    if !alignment.ready {
        let detail = alignment
            .sections
            .iter()
            .find(|section| !section.warnings.is_empty())
            .map(|section| {
                format!(
                    "card {}: {}",
                    section.section_index,
                    section.warnings.join("; ")
                )
            })
            .unwrap_or_else(|| "unknown alignment failure".to_string());
        return Err(format!("TOON packet alignment failed: {detail}"));
    }
    Ok(document)
}

fn gmail_batch_path(batch_id: &str) -> Result<PathBuf, String> {
    if !batch_id.starts_with("gmail-")
        || !batch_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err("Invalid Gmail batch ID".to_string());
    }
    let home = BaseDirs::new().ok_or_else(|| "Could not resolve the home directory".to_string())?;
    Ok(home
        .home_dir()
        .join(".google-workspace/gmail-batches")
        .join(format!("{batch_id}.json")))
}

fn render_gmail_digest_toon(
    batch_id: &str,
    clusters_toon: &str,
    title_override: Option<String>,
) -> Result<Document, String> {
    let packet: GmailDigestPacket = decode_default(clusters_toon)
        .map_err(|error| format!("Invalid Gmail TOON packet: {error}"))?;
    if packet.clusters.is_empty() {
        return Err("Gmail TOON clusters must not be empty".to_string());
    }
    let batch: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(gmail_batch_path(batch_id)?).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("Could not parse Gmail batch: {error}"))?;
    let messages = batch
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Gmail batch has no messages array".to_string())?;
    let mut by_id = BTreeMap::new();
    for message in messages {
        let id = message
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Gmail batch message has no id".to_string())?;
        let permalink = message
            .get("gmail_permalink")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("Gmail batch message {id} has no permalink"))?;
        let subject = message
            .pointer("/headers/Subject")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("(no subject)");
        by_id.insert(id.to_string(), (subject.to_string(), permalink.to_string()));
    }
    let mut seen_cluster_ids = BTreeSet::new();
    let mut seen_message_ids = BTreeSet::new();
    let mut sections = Vec::with_capacity(packet.clusters.len());
    for cluster in packet.clusters {
        let id = required_text(&cluster.id, "cluster id", "<unknown>")?;
        if !seen_cluster_ids.insert(id.clone()) {
            return Err(format!("Duplicate Gmail TOON cluster id: {id}"));
        }
        let title = required_text(&cluster.title, "cluster title", &id)?;
        let narrative = required_text(&cluster.narrative, "cluster narrative", &id)?;
        let narration = required_text(&cluster.narration, "cluster narration", &id)?;
        if !matches!(
            cluster.verdict.as_str(),
            "open_now" | "quick_skim" | "clean" | "ignore"
        ) {
            return Err(format!("Unsupported verdict for Gmail cluster {id}"));
        }
        if cluster.message_ids.is_empty() {
            return Err(format!("Gmail cluster {id} has no message IDs"));
        }
        let mut rows = Vec::with_capacity(cluster.message_ids.len());
        for message_id in cluster.message_ids {
            if !seen_message_ids.insert(message_id.clone()) {
                return Err(format!("Duplicate Gmail message ID: {message_id}"));
            }
            let (subject, permalink) = by_id
                .get(&message_id)
                .ok_or_else(|| format!("Unknown Gmail message ID: {message_id}"))?;
            rows.push(format!("- {subject} | [Gmail mail]({permalink})"));
        }
        let mut markdown = format!(
            "# {title}\n\n{narrative}\n\n{}\n\n**Verdict:** {}",
            rows.join("\n"),
            cluster.verdict
        );
        if let Some(primary_url) = optional_text(cluster.primary_url) {
            if !primary_url.starts_with("https://") && !primary_url.starts_with("http://") {
                return Err(format!("Invalid primary_url for Gmail cluster {id}"));
            }
            markdown.push_str(&format!("\n\n[Primary source]({primary_url})"));
        }
        sections.push(DocumentSection {
            markdown,
            speech_text: narration,
            speech_mode: SpeechMode::Custom,
        });
    }
    if seen_message_ids.len() != by_id.len() {
        return Err(format!(
            "Gmail TOON coverage is incomplete: expected {}, received {} message IDs",
            by_id.len(),
            seen_message_ids.len()
        ));
    }
    let title = optional_text(title_override)
        .or_else(|| optional_text(packet.title))
        .unwrap_or_else(|| "Gmail digest".to_string());
    let document = Document { title, sections };
    validate_document(&document)?;
    Ok(document)
}

fn canonical_alignment_term(term: &str) -> String {
    match term.to_ascii_lowercase().as_str() {
        "returns" | "returned" | "returning" => "return".to_string(),
        "throws" | "threw" | "throwing" => "throw".to_string(),
        "validates" | "validated" | "validating" | "validation" => "validate".to_string(),
        "parses" | "parsed" | "parsing" => "parse".to_string(),
        "queues" | "queued" | "queuing" | "queueing" => "queue".to_string(),
        "publishes" | "published" | "publishing" => "publish".to_string(),
        _ => term.to_ascii_lowercase(),
    }
}

fn text_alignment_terms(value: &str, include_numeric: bool) -> BTreeSet<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "has", "in", "is", "it",
        "of", "on", "or", "that", "the", "their", "this", "to", "was", "with", "you", "your",
    ];
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| {
            let has_case_transition = term
                .chars()
                .zip(term.chars().skip(1))
                .any(|(left, right)| left.is_ascii_lowercase() && right.is_ascii_uppercase());
            !term.is_empty()
                && !STOP_WORDS.contains(&term)
                && (term.chars().count() >= 3
                    || (include_numeric && term.chars().all(char::is_numeric)))
                && (include_numeric || !term.chars().all(char::is_numeric))
                && !has_case_transition
        })
        .map(canonical_alignment_term)
        .collect()
}

fn numbered_code_line(line: &str) -> Option<(&str, &str)> {
    let (prefix, code) = line.trim().split_once('|')?;
    let mut parts = prefix.split_whitespace();
    let line_number = parts.next()?;
    (line_number.chars().all(char::is_numeric) && parts.all(|marker| matches!(marker, "+" | "-")))
        .then_some((line_number, code.trim()))
}

fn is_behavioral_code_line(code: &str) -> bool {
    if code.contains("<--") {
        return true;
    }
    let code = code.split_once("//").map_or(code, |(code, _)| code).trim();
    if code.is_empty() || matches!(code, "{" | "}" | ";") {
        return false;
    }
    let declaration_prefixes = [
        "import ",
        "package ",
        "use ",
        "namespace ",
        "class ",
        "interface ",
        "enum ",
        "struct ",
        "type ",
        "trait ",
        "impl ",
        "module ",
        "fn ",
        "function ",
        "func ",
        "public ",
        "private ",
        "protected ",
        "internal ",
        "@",
    ];
    if declaration_prefixes
        .iter()
        .any(|prefix| code.starts_with(prefix))
    {
        return false;
    }
    let control_prefixes = [
        "if ", "if(", "else", "match ", "match(", "switch ", "switch(", "case ", "for ", "for(",
        "while ", "while(", "catch ", "catch(", "return", "throw", "await ", "yield ", "break",
        "continue",
    ];
    control_prefixes
        .iter()
        .any(|prefix| code.starts_with(prefix))
        || code.contains('(')
        || code.contains(" = ")
        || code.contains(" += ")
        || code.contains(" -= ")
}

fn source_line_references(value: &str) -> BTreeSet<String> {
    let mut references = BTreeSet::new();
    let mut in_fence = false;
    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            continue;
        }
        if let Some((line_number, code)) = numbered_code_line(trimmed) {
            if is_behavioral_code_line(code) {
                references.insert(line_number.to_string());
            }
        }
    }
    references
}

fn spoken_number_word(word: &str) -> Option<u32> {
    match word {
        "zero" => Some(0),
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "thirteen" => Some(13),
        "fourteen" => Some(14),
        "fifteen" => Some(15),
        "sixteen" => Some(16),
        "seventeen" => Some(17),
        "eighteen" => Some(18),
        "nineteen" => Some(19),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "seventy" => Some(70),
        "eighty" => Some(80),
        "ninety" => Some(90),
        _ => None,
    }
}

fn spoken_line_number(words: &[String], start: usize) -> Option<(u32, usize)> {
    let first = words.get(start)?;
    if first.chars().all(char::is_numeric) {
        return first.parse().ok().map(|number| (number, start + 1));
    }
    let mut total = 0;
    let mut current = 0;
    let mut found_number = false;
    let mut index = start;
    while let Some(word) = words.get(index) {
        if let Some(number) = spoken_number_word(word) {
            current += number;
            found_number = true;
        } else if word == "hundred" && current > 0 {
            current *= 100;
        } else if word == "thousand" && current > 0 {
            total += current * 1000;
            current = 0;
        } else if word != "and" || !found_number {
            break;
        }
        index += 1;
    }
    found_number.then_some((total + current, index))
}

fn narration_line_references(value: &str) -> BTreeSet<String> {
    let normalized = value.replace(['–', '—'], " through ");
    let words = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut references = BTreeSet::new();
    for index in 0..words.len().saturating_sub(1) {
        if !matches!(words[index].as_str(), "line" | "lines") {
            continue;
        }
        let Some((start, next)) = spoken_line_number(&words, index + 1) else {
            continue;
        };
        let end = matches!(words.get(next).map(String::as_str), Some("through" | "to"))
            .then(|| spoken_line_number(&words, next + 1))
            .flatten()
            .map_or(start, |(end, _)| end);
        if start <= end && end - start <= 10_000 {
            references.extend((start..=end).map(|number| number.to_string()));
        }
    }
    references
}

fn narration_line_number_terms(value: &str) -> BTreeSet<String> {
    let normalized = value.replace(['–', '—'], " through ");
    let words = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let mut terms = BTreeSet::new();
    for index in 0..words.len().saturating_sub(1) {
        if !matches!(words[index].as_str(), "line" | "lines") {
            continue;
        }
        let Some((_, next)) = spoken_line_number(&words, index + 1) else {
            continue;
        };
        terms.extend(words[index + 1..next].iter().cloned());
        if matches!(words.get(next).map(String::as_str), Some("through" | "to")) {
            if let Some((_, end)) = spoken_line_number(&words, next + 1) {
                terms.extend(words[next + 1..end].iter().cloned());
            }
        }
    }
    terms
}

fn source_alignment_terms(value: &str) -> BTreeSet<String> {
    let mut fallback_markdown = String::new();
    let mut terms = BTreeSet::new();
    let mut in_fence = false;
    let mut fence_language = String::new();

    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            if in_fence {
                fence_language = trimmed[3..].trim().to_ascii_lowercase();
            }
            continue;
        }
        if !in_fence {
            fallback_markdown.push_str(line);
            fallback_markdown.push('\n');
            if trimmed.starts_with('#') {
                terms.extend(text_alignment_terms(
                    trimmed.trim_start_matches('#').trim(),
                    false,
                ));
            }
        } else if fence_language == "text" {
            terms.extend(text_alignment_terms(trimmed, false));
        }
        if let Some((_, annotation)) = line.split_once("<--") {
            terms.extend(text_alignment_terms(annotation, true));
        }
    }

    if terms.is_empty() {
        terms.extend(text_alignment_terms(
            &markdown_to_speech(&fallback_markdown),
            false,
        ));
    }
    terms
}

fn has_alignment_anchors(value: &str) -> bool {
    let mut in_fence = false;
    let mut fence_language = String::new();
    for line in value.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            if in_fence {
                fence_language = trimmed[3..].trim().to_ascii_lowercase();
            }
            continue;
        }
        if trimmed.starts_with('#')
            || line.contains("<--")
            || (in_fence && fence_language == "text")
        {
            return true;
        }
    }
    false
}

fn narration_alignment_terms(value: &str) -> BTreeSet<String> {
    let line_number_words = narration_line_number_terms(value);
    text_alignment_terms(value, true)
        .into_iter()
        .filter(|term| {
            !line_number_words.contains(term)
                && !matches!(term.as_str(), "line" | "lines" | "through")
        })
        .collect()
}

fn alignment_summary(
    source_sections: &[String],
    narration_sections: &[String],
) -> AlignmentSummary {
    let mut ready = true;
    let sections = source_sections
        .iter()
        .zip(narration_sections)
        .enumerate()
        .map(|(index, (source, narration))| {
            let visual_terms = source_alignment_terms(source);
            let narration_terms = narration_alignment_terms(narration);
            let anchor_scoped = has_alignment_anchors(source);
            let source_lines = source_line_references(source);
            let narration_lines = narration_line_references(narration);
            let missing_line_references = source_lines
                .difference(&narration_lines)
                .cloned()
                .collect::<Vec<_>>();
            let shared_terms = visual_terms.intersection(&narration_terms).count();
            let missing_visual_terms = visual_terms
                .difference(&narration_terms)
                .take(8)
                .cloned()
                .collect::<Vec<_>>();
            let spoken_grounding = if narration_terms.is_empty() {
                1.0
            } else {
                shared_terms as f32 / narration_terms.len() as f32
            };
            let visual_coverage = if visual_terms.is_empty() {
                1.0
            } else {
                shared_terms as f32 / visual_terms.len() as f32
            };
            let mut warnings = Vec::new();
            if shared_terms < 2 {
                warnings.push(
                    "fewer than two meaningful terms are shared by the visual text and narration"
                        .to_string(),
                );
            }
            if !anchor_scoped && spoken_grounding < 0.65 {
                warnings.push(format!(
                    "spoken grounding {:.0}% is below 65%",
                    spoken_grounding * 100.0
                ));
            }
            if visual_coverage < 0.55 {
                warnings.push(format!(
                    "visual coverage {:.0}% is below 55%",
                    visual_coverage * 100.0
                ));
            }
            if !missing_line_references.is_empty() {
                warnings.push(format!(
                    "missing narration line references: {}",
                    missing_line_references.join(", ")
                ));
            }
            if !warnings.is_empty() {
                ready = false;
            }
            AlignmentSection {
                section_index: index + 1,
                spoken_grounding,
                visual_coverage,
                shared_terms,
                missing_visual_terms,
                missing_line_references,
                warnings,
            }
        })
        .collect();
    AlignmentSummary { sections, ready }
}

fn utf16_offset(value: &str, byte_offset: usize) -> usize {
    value[..byte_offset].encode_utf16().count()
}

fn utf16_range(value: &str, start: usize, end: usize) -> TextRange {
    TextRange {
        start_utf16: utf16_offset(value, start),
        end_utf16: utf16_offset(value, end),
    }
}

fn coverage_blocks(source: &str) -> Vec<(String, String, usize, usize)> {
    let mut blocks = Vec::new();
    let mut paragraph_start = None;
    let mut paragraph_end = 0;
    let mut offset = 0;
    let mut fence: Option<String> = None;
    for line in source.split_inclusive('\n') {
        let line_end = offset + line.len();
        let trimmed = line.trim();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            if paragraph_start.is_some() {
                let start = paragraph_start.take().unwrap();
                blocks.push((
                    "paragraph".to_string(),
                    source[start..paragraph_end].trim().to_string(),
                    start,
                    paragraph_end,
                ));
            }
            fence = if fence.is_some() {
                None
            } else {
                Some(trimmed[3..].trim().to_ascii_lowercase())
            };
            offset = line_end;
            continue;
        }
        if let Some(language) = &fence {
            if language == "text" && !trimmed.is_empty() {
                blocks.push((
                    "diagram".to_string(),
                    trimmed.to_string(),
                    offset,
                    offset + line.trim_end().len(),
                ));
            }
            if let Some((_, annotation)) = line.split_once("<--") {
                let annotation_start = offset + line.find("<--").unwrap_or(0) + 3;
                blocks.push((
                    "annotation".to_string(),
                    annotation.trim().to_string(),
                    annotation_start,
                    line_end,
                ));
            }
            offset = line_end;
            continue;
        }
        if trimmed.is_empty() {
            if paragraph_start.is_some() {
                let start = paragraph_start.take().unwrap();
                blocks.push((
                    "paragraph".to_string(),
                    source[start..paragraph_end].trim().to_string(),
                    start,
                    paragraph_end,
                ));
            }
        } else if let Some((_, annotation)) = line.split_once("<--") {
            if paragraph_start.is_some() {
                let start = paragraph_start.take().unwrap();
                blocks.push((
                    "paragraph".to_string(),
                    source[start..paragraph_end].trim().to_string(),
                    start,
                    paragraph_end,
                ));
            }
            let annotation_start = offset + line.find("<--").unwrap_or(0) + 3;
            blocks.push((
                "annotation".to_string(),
                annotation.trim().to_string(),
                annotation_start,
                line_end,
            ));
        } else if trimmed.starts_with('#') {
            if paragraph_start.is_some() {
                let start = paragraph_start.take().unwrap();
                blocks.push((
                    "paragraph".to_string(),
                    source[start..paragraph_end].trim().to_string(),
                    start,
                    paragraph_end,
                ));
            }
            blocks.push((
                "heading".to_string(),
                trimmed.trim_start_matches('#').trim().to_string(),
                offset,
                line_end,
            ));
        } else if trimmed.starts_with("- ")
            || trimmed.starts_with("* ")
            || trimmed.starts_with("+ ")
        {
            if paragraph_start.is_some() {
                let start = paragraph_start.take().unwrap();
                blocks.push((
                    "paragraph".to_string(),
                    source[start..paragraph_end].trim().to_string(),
                    start,
                    paragraph_end,
                ));
            }
            blocks.push((
                "list".to_string(),
                trimmed[2..].to_string(),
                offset,
                line_end,
            ));
        } else if trimmed.starts_with('|')
            && trimmed.matches('|').count() >= 2
            && !trimmed.contains("---")
        {
            if paragraph_start.is_some() {
                let _ = paragraph_start.take().unwrap();
                blocks.push(("table".to_string(), trimmed.to_string(), offset, line_end));
            } else {
                blocks.push(("table".to_string(), trimmed.to_string(), offset, line_end));
            }
        } else {
            paragraph_start.get_or_insert(offset);
            paragraph_end = line_end;
        }
        offset = line_end;
    }
    if let Some(start) = paragraph_start {
        blocks.push((
            "paragraph".to_string(),
            source[start..paragraph_end].trim().to_string(),
            start,
            paragraph_end,
        ));
    }
    blocks
        .into_iter()
        .filter(|(_, text, _, _)| !source_alignment_terms(text).is_empty())
        .collect()
}

fn narration_paragraphs(narration: &str) -> Vec<(&str, usize, usize)> {
    let mut paragraphs = Vec::new();
    let mut offset = 0;
    for paragraph in narration.split("\n\n") {
        let start = narration[offset..]
            .find(paragraph)
            .map(|index| offset + index)
            .unwrap_or(offset);
        let end = start + paragraph.len();
        if !paragraph.trim().is_empty() {
            paragraphs.push((paragraph, start, end));
        }
        offset = end.saturating_add(2);
    }
    if paragraphs.is_empty() {
        paragraphs.push((narration, 0, narration.len()));
    }
    paragraphs
}

fn analyze_section(section_index: usize, source: &str, narration: &str) -> SectionPrecheckResult {
    const MIN_GROUNDING: f32 = 0.65;
    const MIN_COVERAGE: f32 = 0.55;
    let visual_terms = source_alignment_terms(source);
    let narration_terms = narration_alignment_terms(narration);
    let anchor_scoped = has_alignment_anchors(source);
    let missing_line_references = source_line_references(source)
        .difference(&narration_line_references(narration))
        .cloned()
        .collect::<Vec<_>>();
    let shared_terms = visual_terms.intersection(&narration_terms).count();
    let spoken_grounding = if narration_terms.is_empty() {
        1.0
    } else {
        shared_terms as f32 / narration_terms.len() as f32
    };
    let visual_coverage = if visual_terms.is_empty() {
        1.0
    } else {
        shared_terms as f32 / visual_terms.len() as f32
    };
    let mut diagnostics = Vec::new();
    if shared_terms < 2 {
        diagnostics.push(SectionDiagnostic {
            code: "shared-terms".to_string(),
            severity: DiagnosticSeverity::Error,
            message: "Fewer than two meaningful terms are shared".to_string(),
            source_range: Some(utf16_range(source, 0, source.len())),
            narration_range: Some(utf16_range(narration, 0, narration.len())),
        });
    }
    if !anchor_scoped && spoken_grounding < MIN_GROUNDING {
        diagnostics.push(SectionDiagnostic {
            code: "spoken-grounding".to_string(),
            severity: DiagnosticSeverity::Error,
            message: format!(
                "Spoken grounding {:.0}% is below 65%",
                spoken_grounding * 100.0
            ),
            source_range: None,
            narration_range: Some(utf16_range(narration, 0, narration.len())),
        });
    }
    if visual_coverage < MIN_COVERAGE {
        diagnostics.push(SectionDiagnostic {
            code: "visual-coverage".to_string(),
            severity: DiagnosticSeverity::Error,
            message: format!(
                "Visual coverage {:.0}% is below 55%",
                visual_coverage * 100.0
            ),
            source_range: Some(utf16_range(source, 0, source.len())),
            narration_range: None,
        });
    }
    if !missing_line_references.is_empty() {
        diagnostics.push(SectionDiagnostic {
            code: "line-references".to_string(),
            severity: DiagnosticSeverity::Error,
            message: format!(
                "Narration is missing required line references: {}",
                missing_line_references.join(", ")
            ),
            source_range: Some(utf16_range(source, 0, source.len())),
            narration_range: Some(utf16_range(narration, 0, narration.len())),
        });
    }
    for issue in narration_quality_issues(source, narration) {
        diagnostics.push(SectionDiagnostic {
            code: issue
                .split_whitespace()
                .next()
                .unwrap_or("narration-quality")
                .replace('_', "-"),
            severity: DiagnosticSeverity::Error,
            message: issue,
            source_range: None,
            narration_range: Some(utf16_range(narration, 0, narration.len())),
        });
    }
    let paragraphs = narration_paragraphs(narration);
    let coverage_blocks = coverage_blocks(source)
        .into_iter()
        .map(|(kind, text, start, end)| {
            let block_terms = source_alignment_terms(&text);
            let best = paragraphs
                .iter()
                .map(|(paragraph, paragraph_start, paragraph_end)| {
                    let shared = block_terms
                        .intersection(&narration_alignment_terms(paragraph))
                        .cloned()
                        .collect::<Vec<_>>();
                    (shared, *paragraph_start, *paragraph_end)
                })
                .max_by_key(|(shared, _, _)| shared.len());
            let (shared, narration_start, narration_end) = best.unwrap_or_default();
            let covered = if block_terms.len() == 1 {
                shared.len() == 1
            } else {
                shared.len() >= 2
            };
            let missing_terms = block_terms
                .difference(&shared.iter().cloned().collect())
                .take(8)
                .cloned()
                .collect::<Vec<_>>();
            if !covered {
                diagnostics.push(SectionDiagnostic {
                    code: "uncovered-block".to_string(),
                    severity: DiagnosticSeverity::Warning,
                    message: format!("{} has no matching narration thought", kind),
                    source_range: Some(utf16_range(source, start, end)),
                    narration_range: (narration_end > narration_start)
                        .then(|| utf16_range(narration, narration_start, narration_end)),
                });
            }
            CoverageBlockResult {
                kind,
                label: text.chars().take(80).collect(),
                source_range: utf16_range(source, start, end),
                narration_range: (narration_end > narration_start)
                    .then(|| utf16_range(narration, narration_start, narration_end)),
                shared_terms: shared,
                missing_terms,
                covered,
            }
        })
        .collect::<Vec<_>>();
    let required_visual_shared_terms =
        ((visual_terms.len() as f32 * MIN_COVERAGE).ceil() as usize).max(2.min(visual_terms.len()));
    let required_grounded_shared_terms = ((narration_terms.len() as f32 * MIN_GROUNDING).ceil()
        as usize)
        .max(2.min(narration_terms.len()));
    let ready = diagnostics
        .iter()
        .all(|diagnostic| matches!(diagnostic.severity, DiagnosticSeverity::Warning));
    SectionPrecheckResult {
        section_index,
        spoken_grounding,
        visual_coverage,
        shared_terms,
        visual_terms: visual_terms.len(),
        narration_terms: narration_terms.len(),
        required_visual_shared_terms,
        required_grounded_shared_terms,
        diagnostics,
        coverage_blocks,
        ready,
    }
}

fn narration_words(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn copied_phrase(source: &str, narration: &str) -> Option<String> {
    const COPIED_PHRASE_WORDS: usize = 12;

    let source_words = narration_words(source);
    let narration_words = narration_words(narration);
    source_words
        .windows(COPIED_PHRASE_WORDS)
        .find_map(|source_phrase| {
            narration_words
                .windows(COPIED_PHRASE_WORDS)
                .any(|narration_phrase| narration_phrase == source_phrase)
                .then(|| source_phrase.join(" "))
        })
}

fn raw_identifier_syntax(narration: &str) -> Option<String> {
    narration
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .find(|word| {
            let mut characters = word.chars();
            let starts_lowercase = characters
                .next()
                .is_some_and(|character| character.is_ascii_lowercase());
            let has_uppercase = characters.any(|character| character.is_ascii_uppercase());
            (word.contains('_')
                && word
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_'))
                || (starts_lowercase && has_uppercase)
        })
        .map(str::to_string)
}

fn narration_quality_issues(source: &str, narration: &str) -> Vec<String> {
    let mut issues = Vec::new();
    let mut has_heading = false;
    let mut has_fence = false;
    let mut has_table = false;
    let mut has_diff_line = false;

    for line in narration.lines() {
        let trimmed = line.trim();
        has_heading |=
            trimmed.starts_with('#') && trimmed.chars().nth(1).is_some_and(char::is_whitespace);
        has_fence |= trimmed.starts_with(&char::from(96).to_string().repeat(3))
            || trimmed.starts_with("~~~");
        has_table |= trimmed.matches('|').count() >= 2;

        let mut parts = trimmed.split_whitespace();
        has_diff_line |= matches!(
            (parts.next(), parts.next(), parts.next()),
            (Some(line_number), Some(marker), Some("|"))
                if line_number.chars().all(char::is_numeric)
                    && matches!(marker, "+" | "-")
        );
    }

    if has_heading {
        issues.push("Markdown heading syntax".to_string());
    }
    if has_fence {
        issues.push("code fence syntax".to_string());
    }
    if has_table {
        issues.push("table syntax".to_string());
    }
    if has_diff_line {
        issues.push("diff-line syntax".to_string());
    }
    if narration.contains(char::from(96)) {
        issues.push("inline code syntax".to_string());
    }
    if narration.split_whitespace().any(|token| {
        let token = token.trim_matches(|character: char| {
            matches!(
                character,
                '.' | ',' | ':' | ';' | '!' | '?' | ')' | '(' | '[' | ']'
            )
        });
        token.starts_with('/')
            || token.starts_with("~/")
            || (token.contains('/') && token.contains('.'))
    }) {
        issues.push("raw file path".to_string());
    }
    if let Some(identifier) = raw_identifier_syntax(narration) {
        issues.push(format!("raw identifier syntax (`{identifier}`)"));
    }
    if let Some(phrase) = copied_phrase(source, narration) {
        issues.push(format!("long copied source passage (\"{phrase}\")"));
    }

    issues
}

fn narration_quality_summary(
    source_sections: &[String],
    narration_sections: &[String],
) -> NarrationQualitySummary {
    let mut ready = true;
    let sections = source_sections
        .iter()
        .zip(narration_sections)
        .enumerate()
        .map(|(index, (source, narration))| {
            let issues = narration_quality_issues(source, narration);
            if !issues.is_empty() {
                ready = false;
            }
            NarrationQualitySection {
                section_index: index + 1,
                issues,
            }
        })
        .collect();
    NarrationQualitySummary { sections, ready }
}

#[cfg(test)]
fn validate_document_narration_quality(document: &Document) -> Result<(), String> {
    let source_sections = document
        .sections
        .iter()
        .map(|section| section.markdown.clone())
        .collect::<Vec<_>>();
    let narration_sections = document
        .sections
        .iter()
        .map(|section| section.speech_text.clone())
        .collect::<Vec<_>>();
    let quality = narration_quality_summary(&source_sections, &narration_sections);
    if let Some(section) = quality
        .sections
        .iter()
        .find(|section| !section.issues.is_empty())
    {
        return Err(format!(
            "section {} narration must use natural speech: {}",
            section.section_index,
            section.issues.join(", ")
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct TransferPrecheck {
    findings: Vec<String>,
    override_token: Option<String>,
}

#[derive(Clone, Debug)]
struct TransferValidation {
    acknowledged_findings: Vec<String>,
    acknowledgement_reason: Option<String>,
    verified_checks: Vec<String>,
}

fn transfer_precheck(document: &Document) -> TransferPrecheck {
    let source = document
        .sections
        .iter()
        .map(|section| section.markdown.clone())
        .collect::<Vec<_>>();
    let narration = document
        .sections
        .iter()
        .map(|section| section.speech_text.clone())
        .collect::<Vec<_>>();
    let alignment = alignment_summary(&source, &narration);
    let narration_quality = narration_quality_summary(&source, &narration);
    let mut findings = alignment
        .sections
        .iter()
        .filter(|section| !section.warnings.is_empty())
        .map(|section| {
            format!(
                "section {} alignment: {}",
                section.section_index,
                section.warnings.join("; ")
            )
        })
        .collect::<Vec<_>>();
    findings.extend(
        narration_quality
            .sections
            .iter()
            .filter(|section| !section.issues.is_empty())
            .map(|section| {
                format!(
                    "section {} narration quality: {}",
                    section.section_index,
                    section.issues.join(", ")
                )
            }),
    );
    let override_token = (!findings.is_empty()).then(|| {
        let mut hasher = Hasher::new();
        hasher.update(b"kokoro-precheck-override-v1\0");
        for section in &document.sections {
            hasher.update(section.markdown.as_bytes());
            hasher.update(b"\0");
            hasher.update(section.speech_text.as_bytes());
            hasher.update(b"\0");
        }
        for finding in &findings {
            hasher.update(finding.as_bytes());
            hasher.update(b"\0");
        }
        hasher.finalize().to_hex().to_string()
    });
    TransferPrecheck {
        findings,
        override_token,
    }
}

fn validate_document_for_transfer(
    document: &Document,
    acknowledgement: Option<PrecheckAcknowledgement>,
) -> Result<TransferValidation, String> {
    validate_document(document)?;
    let precheck = transfer_precheck(document);
    match (precheck.findings.is_empty(), acknowledgement) {
        (true, None) => Ok(TransferValidation {
            acknowledged_findings: Vec::new(),
            acknowledgement_reason: None,
            verified_checks: Vec::new(),
        }),
        (true, Some(_)) => {
            Err("precheck acknowledgement is only valid when findings remain".to_string())
        }
        (false, None) => Err(format!(
            "precheck findings require acknowledgement token {}: {}",
            precheck.override_token.as_deref().unwrap_or_default(),
            precheck.findings.join("; ")
        )),
        (false, Some(acknowledgement)) => {
            let reason = acknowledgement.reason.trim().to_string();
            let verified_checks = acknowledgement
                .verified_checks
                .into_iter()
                .map(|check| check.trim().to_string())
                .filter(|check| !check.is_empty())
                .collect::<Vec<_>>();
            if reason.is_empty() || verified_checks.is_empty() {
                return Err(
                    "precheck acknowledgement requires a reason and at least one verified check"
                        .to_string(),
                );
            }
            if Some(acknowledgement.token.as_str()) != precheck.override_token.as_deref() {
                return Err("precheck acknowledgement token is stale; rerun precheck".to_string());
            }
            Ok(TransferValidation {
                acknowledged_findings: precheck.findings,
                acknowledgement_reason: Some(reason),
                verified_checks,
            })
        }
    }
}

fn precheck_document_contents(source: &str, narration: &str) -> Result<FilePrecheckResult, String> {
    let explicit_source_markers = source
        .lines()
        .any(|line| line.trim() == "<!-- kokoro-reader-section -->");
    let source_section_text = if explicit_source_markers {
        split_sections_on_marker(&source, "<!-- kokoro-reader-section -->", false)
    } else {
        split_paragraph_sections(&source)
    };
    let narration_section_text = split_sections_on_marker(&narration, "---", true);
    let source_sections = source_section_text.len();
    let narration_sections = narration_section_text.len();
    let document = document_from_mcp_files("Precheck".to_string(), &source, &narration)?;
    validate_document(&document)?;
    let mut warnings = Vec::new();
    if !explicit_source_markers {
        warnings.push(
            "source Markdown has no explicit section markers; paragraph fallback was used"
                .to_string(),
        );
    }
    let alignment = alignment_summary(&source_section_text, &narration_section_text);
    let narration_quality =
        narration_quality_summary(&source_section_text, &narration_section_text);
    warnings.extend(
        alignment
            .sections
            .iter()
            .filter(|section| !section.warnings.is_empty())
            .map(|section| {
                format!(
                    "section {} alignment: {}",
                    section.section_index,
                    section.warnings.join("; ")
                )
            }),
    );
    warnings.extend(
        narration_quality
            .sections
            .iter()
            .filter(|section| !section.issues.is_empty())
            .map(|section| {
                format!(
                    "section {} narration quality: {}",
                    section.section_index,
                    section.issues.join(", ")
                )
            }),
    );
    let transfer_precheck = transfer_precheck(&document);
    Ok(FilePrecheckResult {
        source_sections,
        narration_sections,
        document_bytes: document_byte_size(&document),
        max_document_bytes: MAX_DOCUMENT_BYTES,
        explicit_source_markers,
        warnings,
        ready_for_send: alignment.ready && narration_quality.ready,
        override_available: !transfer_precheck.findings.is_empty(),
        override_token: transfer_precheck.override_token,
        alignment,
        narration_quality,
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

fn cached_audio_duration_ms(path: &Path) -> Result<Option<u64>, String> {
    cached_audio_size(path)
        .map(|size| size.map(|size| ((size - 44) / 2 * 1000) / SAMPLE_RATE as u64))
}

fn cached_audio_asset(path: &Path) -> Result<Option<AudioAsset>, String> {
    if let Some(duration_ms) = cached_audio_duration_ms(path)? {
        touch_cache_file(path)?;
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
) -> Result<Vec<Option<u64>>, String> {
    if !valid_voice(&voice) {
        return Err(format!("Unsupported voice: {voice}"));
    }
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    ensure_project_exists(&project, &project_id)?;
    let mut durations = Vec::with_capacity(texts.len());
    let mut cached_keys = Vec::new();
    for text in texts {
        let text = text.trim().to_string();
        if text.is_empty() {
            durations.push(None);
            continue;
        }
        if text.len() > MAX_SENTENCE_BYTES {
            return Err("Sentence is too large".to_string());
        }
        let key = cache_key(&text, &voice);
        let cache_path = paths.cache_dir.join(format!("{key}.wav"));
        let duration_ms = cached_audio_duration_ms(&cache_path)?;
        durations.push(duration_ms);
        if duration_ms.is_some() {
            cached_keys.push(key);
        }
    }
    record_cache_references(&project, cached_keys)?;
    Ok(durations)
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
fn storage_stats(app: tauri::AppHandle) -> Result<StorageStats, String> {
    storage_stats_for_paths(&app_paths(&app)?)
}

#[tauri::command]
fn get_project_location(
    app: tauri::AppHandle,
    project_id: String,
) -> Result<ProjectLocation, String> {
    let paths = app_paths(&app)?;
    project_location(&paths.document_dir, &project_id)
}

#[tauri::command]
fn precheck_sections(params: PrecheckSectionsParams) -> Result<SectionsPrecheckResult, String> {
    if params.sections.is_empty() {
        return Err("At least one section is required".to_string());
    }
    let document = Document {
        title: "Precheck".to_string(),
        sections: params
            .sections
            .iter()
            .map(|section| DocumentSection {
                markdown: section.markdown.clone(),
                speech_text: section.speech_text.clone(),
                speech_mode: SpeechMode::Custom,
            })
            .collect(),
    };
    validate_document(&document)?;
    let sections = params
        .sections
        .into_iter()
        .map(|section| {
            analyze_section(
                section.section_index,
                &section.markdown,
                &section.speech_text,
            )
        })
        .collect::<Vec<_>>();
    let ready_for_send = sections.iter().all(|section| section.ready);
    let transfer_precheck = transfer_precheck(&document);
    Ok(SectionsPrecheckResult {
        sections,
        ready_for_send,
        override_available: !transfer_precheck.findings.is_empty(),
        override_token: transfer_precheck.override_token,
    })
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
    expected_revision: Option<String>,
) -> Result<ProjectDocument, String> {
    validate_document(&document)?;
    let paths = app_paths(&app)?;
    let project = project_path(&paths.document_dir, &project_id)?;
    ensure_project_exists(&project, &project_id)?;
    ensure_project_revision(&project, expected_revision.as_deref())?;
    write_shared_document_files(&project, &document)?;
    write_document(&current_document_path(&project), &document)?;
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
    let previous = read_project_metadata(&project)?;
    let metadata = ProjectMetadata {
        read,
        read_at: if read {
            previous.read_at.or(Some(epoch_seconds()))
        } else {
            None
        },
        ..previous
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
async fn delete_projects(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    project_ids: Vec<String>,
) -> Result<Option<ProjectDocument>, String> {
    let _synthesis_guard = state.synthesis.lock().await;
    let paths = app_paths(&app)?;
    let next = delete_stored_projects(&paths.document_dir, &paths.cache_dir, &project_ids)?;
    if let Some(project) = &next {
        set_state_document(&state, &project.document)?;
    }
    log_event(format!("deleted {} projects", project_ids.len()));
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
    fn transfer_document(
        &self,
        codex_url: String,
        project_id: Option<String>,
        expected_revision: Option<String>,
        precheck_acknowledgement: Option<PrecheckAcknowledgement>,
        document: Document,
    ) -> Result<String, McpError> {
        let validation = validate_document_for_transfer(&document, precheck_acknowledgement)
            .map_err(|error| McpError::invalid_params(error, None))?;
        let (project_id, newly_created) = match project_id.as_deref() {
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
        ensure_project_revision(&project, expected_revision.as_deref())
            .map_err(|error| McpError::invalid_params(error, None))?;
        let existing_created_at = if newly_created {
            None
        } else {
            let metadata = read_project_metadata(&project)
                .map_err(|error| McpError::internal_error(error, None))?;
            metadata
                .created_at
                .or_else(|| file_timestamp(&current_document_path(&project)).ok())
        };
        if let Err(error) = replace_current_document(&project, &document) {
            if newly_created {
                let _ = fs::remove_dir_all(&project);
            }
            return Err(McpError::internal_error(error, None));
        }
        let mut metadata = project_metadata_for_transfer(codex_url);
        metadata.created_at = existing_created_at.or(metadata.created_at);
        if let Err(error) = write_project_metadata(&project, &metadata) {
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
        if !validation.acknowledged_findings.is_empty() {
            log_event(format!(
                "precheck override project={project_id} findings={} reason={} checks={}",
                validation
                    .acknowledged_findings
                    .join(" | ")
                    .replace(['\n', '\r'], " "),
                validation
                    .acknowledgement_reason
                    .as_deref()
                    .unwrap_or_default()
                    .replace(['\n', '\r'], " "),
                validation
                    .verified_checks
                    .join(" | ")
                    .replace(['\n', '\r'], " "),
            ));
        }
        self.launch_reader(&project_id, &document, 0, validation)
    }

    fn pronunciations(&self) -> Result<ReaderPronunciations, McpError> {
        let pronunciation_path = pronunciation_file_path(&self.data_dir)
            .map_err(|error| McpError::internal_error(error, None))?;
        let entries = read_pronunciations(&pronunciation_path)
            .map_err(|error| McpError::internal_error(error, None))?;
        Ok(ReaderPronunciations {
            pronunciation_path: pronunciation_path.to_string_lossy().to_string(),
            entries,
        })
    }

    #[tool(
        description = "Read the shared technical-term pronunciation glossary used while drafting Kokoro Reader narration. Terms are matched case-insensitively; spoken forms are narration-ready text."
    )]
    fn get_reader_pronunciations(&self) -> Result<String, McpError> {
        serde_json::to_string(&self.pronunciations()?)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "Add or update one or more shared technical-term pronunciation entries for Kokoro Reader narration. Each term is matched case-insensitively and each spoken form replaces the prior value."
    )]
    fn set_reader_pronunciations(
        &self,
        Parameters(params): Parameters<SetReaderPronunciationsParams>,
    ) -> Result<String, McpError> {
        let pronunciation_path = pronunciation_file_path(&self.data_dir)
            .map_err(|error| McpError::internal_error(error, None))?;
        let updates = normalize_pronunciation_entries(params.entries)
            .map_err(|error| McpError::invalid_params(error, None))?;
        let mut entries = read_pronunciations(&pronunciation_path)
            .map_err(|error| McpError::internal_error(error, None))?;
        entries.extend(updates);
        write_pronunciations(&pronunciation_path, &entries)
            .map_err(|error| McpError::internal_error(error, None))?;
        serde_json::to_string(&self.pronunciations()?)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "Remove one or more shared technical-term pronunciation entries from Kokoro Reader narration. Terms are matched case-insensitively."
    )]
    fn remove_reader_pronunciations(
        &self,
        Parameters(params): Parameters<RemoveReaderPronunciationsParams>,
    ) -> Result<String, McpError> {
        if params.terms.is_empty() {
            return Err(McpError::invalid_params(
                "At least one pronunciation term is required".to_string(),
                None,
            ));
        }
        let terms = params
            .terms
            .into_iter()
            .map(|term| term.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        if terms.iter().any(|term| term.is_empty()) {
            return Err(McpError::invalid_params(
                "Pronunciation terms must not be empty".to_string(),
                None,
            ));
        }
        let pronunciation_path = pronunciation_file_path(&self.data_dir)
            .map_err(|error| McpError::internal_error(error, None))?;
        let mut entries = read_pronunciations(&pronunciation_path)
            .map_err(|error| McpError::internal_error(error, None))?;
        let changed = terms
            .into_iter()
            .any(|term| entries.remove(&term).is_some());
        if changed {
            write_pronunciations(&pronunciation_path, &entries)
                .map_err(|error| McpError::internal_error(error, None))?;
        }
        serde_json::to_string(&self.pronunciations()?)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

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
        self.transfer_document(
            codex_url,
            params.project_id,
            params.expected_revision,
            params.precheck_acknowledgement,
            document,
        )
    }

    #[tool(
        description = "Decode a strict TOON packet into aligned visual Markdown and custom narration, then create or update a Kokoro Reader project. Use this for compact structured cards; use existing Markdown tools for code-heavy or bespoke documents."
    )]
    fn send_toon_packet(
        &self,
        Parameters(params): Parameters<SendToonPacketParams>,
    ) -> Result<String, McpError> {
        let codex_url = required_codex_url(&params.codex_url)?;
        let document = render_toon_packet(&params.packet_toon, params.title)
            .map_err(|error| McpError::invalid_params(error, None))?;
        self.transfer_document(
            codex_url,
            params.project_id,
            params.expected_revision,
            params.precheck_acknowledgement,
            document,
        )
    }

    #[tool(
        description = "Build and send a complete Gmail digest from a persistent Gmail batch and a strict TOON cluster packet. Every batch message must appear once; the tool renders safe routine rows from sender/subject/permalink metadata only."
    )]
    fn send_gmail_digest_toon(
        &self,
        Parameters(params): Parameters<SendGmailDigestToonParams>,
    ) -> Result<String, McpError> {
        let codex_url = required_codex_url(&params.codex_url)?;
        let document =
            render_gmail_digest_toon(&params.batch_id, &params.clusters_toon, params.title)
                .map_err(|error| McpError::invalid_params(error, None))?;
        self.transfer_document(
            codex_url,
            params.project_id,
            params.expected_revision,
            params.precheck_acknowledgement,
            document,
        )
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
        let validation = validate_document_for_transfer(&document, params.precheck_acknowledgement)
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
        ensure_project_revision(&project, params.expected_revision.as_deref())
            .map_err(|error| McpError::invalid_params(error, None))?;
        let existing_created_at = if newly_created {
            None
        } else {
            let metadata = read_project_metadata(&project)
                .map_err(|error| McpError::internal_error(error, None))?;
            metadata
                .created_at
                .or_else(|| file_timestamp(&current_document_path(&project)).ok())
        };
        if let Err(error) = replace_current_document(&project, &document) {
            if newly_created {
                let _ = fs::remove_dir_all(&project);
            }
            return Err(McpError::internal_error(error, None));
        }
        let mut metadata = project_metadata_for_transfer(codex_url);
        metadata.created_at = existing_created_at.or(metadata.created_at);
        if let Err(error) = write_project_metadata(&project, &metadata) {
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
        if !validation.acknowledged_findings.is_empty() {
            log_event(format!(
                "precheck override project={project_id} findings={} reason={} checks={}",
                validation
                    .acknowledged_findings
                    .join(" | ")
                    .replace(['\n', '\r'], " "),
                validation
                    .acknowledgement_reason
                    .as_deref()
                    .unwrap_or_default()
                    .replace(['\n', '\r'], " "),
                validation
                    .verified_checks
                    .join(" | ")
                    .replace(['\n', '\r'], " "),
            ));
        }
        self.launch_reader(&project_id, &document, 0, validation)
    }

    #[tool(
        description = "Read-only validation for a source Markdown and matching narration file inside ~/.kokoro_reader. Returns section counts, diagnostics, ready_for_send, and a content-bound override token when advisory findings remain, without creating a project or refreshing the app."
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
        description = "Read-only validation for direct Markdown and narration sections. Returns per-section diagnostics, ready_for_send, and a content-bound override token when advisory findings remain."
    )]
    fn precheck_sections(
        &self,
        Parameters(params): Parameters<PrecheckSectionsParams>,
    ) -> Result<String, McpError> {
        let result = crate::precheck_sections(params)
            .map_err(|error| McpError::invalid_params(error, None))?;
        serde_json::to_string(&result)
            .map_err(|error| McpError::internal_error(error.to_string(), None))
    }

    #[tool(
        description = "Replace one or more existing Kokoro Reader sections in place. The expected revision prevents overwriting a newer draft. Every replacement requires both visual Markdown and natural narration, and the complete updated project must pass the same transfer validation."
    )]
    fn update_reader_project_sections(
        &self,
        Parameters(params): Parameters<UpdateReaderProjectSectionsParams>,
    ) -> Result<String, McpError> {
        if params.updates.is_empty() {
            return Err(McpError::invalid_params(
                "At least one section update is required".to_string(),
                None,
            ));
        }
        let project = project_path(&self.data_dir, &params.project_id)
            .map_err(|error| McpError::invalid_params(error, None))?;
        ensure_project_exists(&project, &params.project_id)
            .map_err(|error| McpError::invalid_params(error, None))?;
        ensure_project_revision(&project, Some(&params.expected_revision))
            .map_err(|error| McpError::invalid_params(error, None))?;
        let mut document = read_project(&self.data_dir, &params.project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        let mut seen = BTreeSet::new();
        for update in &params.updates {
            if update.section_index == 0
                || update.section_index > document.sections.len()
                || !seen.insert(update.section_index)
            {
                return Err(McpError::invalid_params(
                    "Section updates must use unique existing one-based indexes".to_string(),
                    None,
                ));
            }
            let section = &mut document.sections[update.section_index - 1];
            section.markdown = update.markdown.trim().to_string();
            section.speech_text = update.speech_text.trim().to_string();
            section.speech_mode = SpeechMode::Custom;
        }
        let validation = validate_document_for_transfer(&document, params.precheck_acknowledgement)
            .map_err(|error| McpError::invalid_params(error, None))?;
        replace_current_document(&project, &document)
            .map_err(|error| McpError::internal_error(error, None))?;
        set_active_project_id(&self.data_dir, &params.project_id)
            .map_err(|error| McpError::internal_error(error, None))?;
        log_event(format!(
            "updated {} sections through MCP project={}",
            params.updates.len(),
            params.project_id
        ));
        if !validation.acknowledged_findings.is_empty() {
            log_event(format!(
                "precheck override project={} findings={} reason={} checks={}",
                params.project_id,
                validation
                    .acknowledged_findings
                    .join(" | ")
                    .replace(['\n', '\r'], " "),
                validation
                    .acknowledgement_reason
                    .as_deref()
                    .unwrap_or_default()
                    .replace(['\n', '\r'], " "),
                validation
                    .verified_checks
                    .join(" | ")
                    .replace(['\n', '\r'], " "),
            ));
        }
        self.launch_reader(&params.project_id, &document, 0, validation)
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
        validation: TransferValidation,
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
            revision: project_revision(&directory)
                .map_err(|error| McpError::internal_error(error, None))?,
            source_path: source_document_path(&directory)
                .to_string_lossy()
                .to_string(),
            narration_path: narration_document_path(&directory)
                .to_string_lossy()
                .to_string(),
            precheck_overridden: !validation.acknowledged_findings.is_empty(),
            acknowledged_findings: validation.acknowledged_findings,
            acknowledgement_reason: validation.acknowledgement_reason,
            verified_checks: validation.verified_checks,
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
            storage_stats,
            get_project_location,
            precheck_sections,
            get_document,
            select_project,
            save_document,
            restore_previous,
            reload_shared_document,
            has_previous,
            delete_project,
            delete_projects,
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
    fn pronunciation_glossary_normalizes_and_persists_entries() {
        let base = test_directory();
        let document_dir = base.join("documents");
        let path = pronunciation_file_path(&document_dir).expect("resolve pronunciation path");
        assert!(read_pronunciations(&path)
            .expect("read missing glossary")
            .is_empty());

        let entries = normalize_pronunciation_entries(BTreeMap::from([
            (" RNowForge ".to_string(), "right now forge".to_string()),
            ("DVLAN".to_string(), "D V LAN".to_string()),
        ]))
        .expect("normalize glossary entries");
        write_pronunciations(&path, &entries).expect("write glossary");
        assert_eq!(
            read_pronunciations(&path).expect("read glossary"),
            BTreeMap::from([
                ("dvlan".to_string(), "D V LAN".to_string()),
                ("rnowforge".to_string(), "right now forge".to_string()),
            ])
        );
        assert!(normalize_pronunciation_entries(BTreeMap::from([(
            "".to_string(),
            "spoken".to_string(),
        )]))
        .is_err());

        fs::write(&path, "[]").expect("write malformed glossary");
        assert!(read_pronunciations(&path).is_err());
        fs::remove_dir_all(base).expect("remove glossary fixture");
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
            created_at: Some(1),
            read_at: Some(2),
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
                created_at: None,
                read_at: None,
                codex_url: Some("https://example.com".to_string()),
            })
            .expect("serialize invalid URL metadata"),
        )
        .expect("write invalid URL metadata");
        assert_eq!(
            read_project_metadata(&project).expect("read sanitized metadata"),
            ProjectMetadata {
                read: true,
                created_at: None,
                read_at: None,
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
                created_at: Some(1),
                read_at: Some(2),
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
            created_at: Some(1),
            read_at: Some(2),
            codex_url: Some("codex://threads/old-thread".to_string()),
        };
        let incoming = project_metadata_for_transfer("codex://threads/new-thread".to_string());
        write_project_metadata(&project, &previous).expect("write previous metadata");
        write_project_metadata(&project, &incoming).expect("write incoming metadata");
        assert_eq!(
            read_project_metadata(&project).expect("read incoming metadata"),
            ProjectMetadata {
                read: false,
                created_at: incoming.created_at,
                read_at: None,
                codex_url: Some("codex://threads/new-thread".to_string()),
            }
        );
        fs::remove_dir_all(project).expect("remove transfer metadata fixture");
    }

    #[test]
    fn transfer_metadata_records_creation_time_and_clears_read_time() {
        let metadata = project_metadata_for_transfer("codex://threads/thread-123".to_string());
        assert!(!metadata.read);
        assert!(metadata.created_at.is_some());
        assert_eq!(metadata.read_at, None);
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
        assert_eq!(
            cached_audio_duration_ms(&truncated).expect("inspect truncated duration"),
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
        assert_eq!(
            cached_audio_duration_ms(&complete).expect("inspect complete duration"),
            Some(0)
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
    fn storage_stats_reports_physical_totals_and_project_cache_usage() {
        let base = test_directory();
        let paths = StoragePaths {
            data_dir: base.join("app"),
            document_dir: base.join("documents"),
            model_dir: base.join("app/models"),
            cache_dir: base.join("cache/audio"),
        };
        fs::create_dir_all(&paths.model_dir).expect("create models");
        fs::create_dir_all(&paths.cache_dir).expect("create cache");
        fs::write(paths.model_dir.join("model.onnx"), [1_u8; 7]).expect("write model");
        let project_id = create_project_dir(&paths.document_dir).expect("create project");
        let project = project_path(&paths.document_dir, &project_id).expect("resolve project");
        replace_current_document(&project, &default_document()).expect("write project");
        let key = cache_key("cached article", "af_bella");
        fs::write(paths.cache_dir.join(format!("{key}.wav")), vec![0_u8; 45]).expect("write cache");
        record_cache_reference(&project, &key).expect("record cache reference");

        let stats = storage_stats_for_paths(&paths).expect("read storage stats");
        assert_eq!(stats.models.bytes, 7);
        assert_eq!(stats.audio_cache.bytes, 45);
        assert_eq!(stats.projects[0].project_id, project_id);
        assert_eq!(stats.projects[0].bytes, 45);
        assert_eq!(stats.projects[0].cached_clips, 1);
        fs::remove_dir_all(base).expect("remove storage fixture");
    }

    #[test]
    fn deleting_multiple_projects_prunes_audio_once() {
        let directory = test_directory();
        let cache_directory = directory.join("audio");
        fs::create_dir_all(&cache_directory).expect("create cache directory");
        let first = create_project_dir(&directory).expect("create first project");
        let second = create_project_dir(&directory).expect("create second project");
        for project_id in [&first, &second] {
            let project = project_path(&directory, project_id).expect("resolve project");
            replace_current_document(&project, &default_document()).expect("write project");
            let key = cache_key(project_id, "af_bella");
            record_cache_reference(&project, &key).expect("record reference");
            fs::write(cache_directory.join(format!("{key}.wav")), vec![0_u8; 45])
                .expect("write audio");
        }
        assert!(
            delete_stored_projects(&directory, &cache_directory, &[first, second])
                .expect("delete projects")
                .is_none()
        );
        assert!(fs::read_dir(&cache_directory)
            .expect("read cache")
            .next()
            .is_none());
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
            "<!-- kokoro-reader-section -->\n# First concept\nThe first concept explains a useful example.\n<!-- kokoro-reader-section -->\n# Second concept\nThe second concept explains another useful example.";
        let narration = "First concept explains the useful example.\n---\nSecond concept explains another useful example.";
        let result = precheck_document_contents(source, narration).expect("precheck should pass");
        assert_eq!(result.source_sections, 2);
        assert_eq!(result.narration_sections, 2);
        assert!(result.explicit_source_markers);
        assert!(result.warnings.is_empty());
        assert!(result.ready_for_send);
        assert!(!result.override_available);
        assert!(result.override_token.is_none());
    }

    #[test]
    fn precheck_reports_low_alignment_without_rejecting_document_shape() {
        let source = "<!-- kokoro-reader-section -->\n# Agent security\nSentinel checks credentials before sending data.";
        let narration = "A mathematician studies fluid equations.";
        let result = precheck_document_contents(source, narration)
            .expect("precheck should inspect alignment");
        assert!(!result.ready_for_send);
        assert!(!result.alignment.ready);
        assert!(!result.alignment.sections[0].warnings.is_empty());
        assert!(!result.alignment.sections[0].missing_visual_terms.is_empty());
    }

    #[test]
    fn precheck_rejects_low_anchor_coverage_even_when_some_terms_match() {
        let source = "<!-- kokoro-reader-section -->\n# Cache queue\n\n```ts\nsummarizeCacheProjects(tasks); // <-- groups clips into article rows and keeps the next incomplete clip for navigation.\n```";
        let narration = "The cache queue changes.";
        let result = precheck_document_contents(source, narration)
            .expect("precheck should inspect coverage");
        assert!(result.alignment.sections[0].shared_terms >= 2);
        assert!(!result.ready_for_send);
        assert!(result.alignment.sections[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("visual coverage")));
    }

    #[test]
    fn precheck_accepts_natural_narration_for_a_code_heavy_card() {
        let fence = char::from(96).to_string().repeat(3);
        let source = format!(
            "<!-- kokoro-reader-section -->\n# Cache queue\n\nThe cache queue groups clips into article rows and keeps the next unfinished clip for navigation.\n\n{fence}ts\nfunction summarizeCacheProjects(tasks: CacheTask[]) {{}}\n{fence}"
        );
        let narration = "The cache queue groups clips into article rows. It keeps the next unfinished clip so opening the row continues useful work.";
        let result = precheck_document_contents(&source, narration).expect("precheck should pass");
        assert!(result.ready_for_send);
        assert!(result.narration_quality.ready);
        assert!(result.narration_quality.sections[0].issues.is_empty());
    }

    #[test]
    fn alignment_uses_visible_code_annotations_without_reading_code_syntax() {
        let fence = char::from(96).to_string().repeat(3);
        let source = format!(
            "<!-- kokoro-reader-section -->\n# Cache queue\n\n{fence}ts\n47 + | summarize cache projects // <-- AFTER: groups clips into article rows.\n{fence}"
        );
        let narration = "After the grouping step, clips become article rows.";
        let result = precheck_document_contents(&source, narration).expect("precheck should pass");
        assert!(result.alignment.sections[0].shared_terms >= 3);
    }

    #[test]
    fn precheck_accepts_semantic_code_line_ranges() {
        let source = "<!-- kokoro-reader-section -->\n# Request flow\n\n```rust\n17 | if bypass { // <-- bypasses validation.\n18 | return; // <-- stops the filter.\n19 | }\n40 | queue_metrics(); // <-- queues metrics.\n43 | return_response(); // <-- returns the response.\n```";
        let narration = "Within this request flow, lines seventeen through nineteen bypass validation and stop the filter. Lines 40–43 queue the metrics, then return the response.";
        let result = precheck_document_contents(source, narration).expect("precheck should pass");
        assert!(result.ready_for_send);
        assert!(result.alignment.sections[0]
            .missing_line_references
            .is_empty());
        assert!(
            narration_line_references("Lines 53 through 55 and lines 60–62").is_superset(
                &BTreeSet::from([
                    "53".to_string(),
                    "55".to_string(),
                    "60".to_string(),
                    "62".to_string()
                ])
            )
        );
    }

    #[test]
    fn precheck_ignores_structural_lines_but_requires_behavioral_ones() {
        let source = "<!-- kokoro-reader-section -->\n# Request flow\n\n```java\n17 | if (bypass) { // <-- bypasses validation.\n18 |   return; // <-- stops the filter.\n19 | }\n20 | import example.metrics.Queue;\n21 | public Response validate(String token) {\n40 | queue_metrics(); // <-- queues metrics.\n```";
        let narration = "Line seventeen routes the request.";
        let result =
            precheck_document_contents(source, narration).expect("precheck should inspect");
        assert!(!result.ready_for_send);
        assert_eq!(
            result.alignment.sections[0].missing_line_references,
            vec!["18".to_string(), "40".to_string()]
        );
        assert!(result.alignment.sections[0]
            .warnings
            .iter()
            .any(|warning| warning == "missing narration line references: 18, 40"));
        assert!(
            narration_line_references("Line one thousand two hundred eighty-eight")
                .contains("1288")
        );
    }

    #[test]
    fn alignment_uses_headings_and_annotations_not_raw_code_syntax() {
        let source = "# Identity flow\n\n```java\nimport example.security.JwtValidator;\npublic Response validate(String token) {\n  if (!signature.verify(token)) return null; // <-- rejects an invalid signature.\n}\n```";
        let terms = source_alignment_terms(source);
        assert_eq!(
            terms,
            BTreeSet::from([
                "flow".to_string(),
                "identity".to_string(),
                "invalid".to_string(),
                "rejects".to_string(),
                "signature".to_string(),
            ])
        );

        let narration =
            "The identity flow rejects an invalid signature before it accepts the request.";
        let shared = terms
            .intersection(&narration_alignment_terms(narration))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(shared.len(), terms.len());
    }

    #[test]
    fn alignment_extracts_slash_and_hash_code_annotations() {
        let source = "```java\nreturn null; // <-- The resolver cannot find an Identity Domain public key.\n# <-- Signature validation fails.\n```";
        let narration = "The resolver cannot find an Identity Domain public key, so signature validation fails when it returns null.";
        let visual_terms = source_alignment_terms(source);
        let narration_terms = narration_alignment_terms(narration);
        assert!(visual_terms.is_subset(&narration_terms));
    }

    #[test]
    fn precheck_rejects_copied_markdown_even_with_anchor_overlap() {
        let fence = char::from(96).to_string().repeat(3);
        let source = format!(
            "<!-- kokoro-reader-section -->\n# Cache queue\n\n| Before | After |\n| --- | --- |\n| Clip rows | Article row |\n\n{fence}ts\n47 + | function summarizeCacheProjects(tasks) {{}}\n{fence}"
        );
        let narration = source.replace("<!-- kokoro-reader-section -->\n", "");
        let result =
            precheck_document_contents(&source, &narration).expect("precheck should inspect");
        assert!(result.alignment.sections[0].shared_terms >= 2);
        assert!(!result.ready_for_send);
        assert!(!result.narration_quality.ready);
        assert!(result.narration_quality.sections[0]
            .issues
            .iter()
            .any(|issue| issue == "Markdown heading syntax"));
        assert!(result.narration_quality.sections[0]
            .issues
            .iter()
            .any(|issue| issue == "code fence syntax"));
    }

    #[test]
    fn narration_quality_rejects_raw_syntax_and_paths() {
        let quote = char::from(96);
        let narration = format!(
            "Use summarizeCacheProjects, read_at, src/main.ts, and {quote}code{quote}.\n47 + | changes."
        );
        let issues = narration_quality_issues("The cache queue explains the transfer.", &narration);
        assert!(issues
            .iter()
            .any(|issue| issue == "raw identifier syntax (`summarizeCacheProjects`)"));
        assert!(issues.iter().any(|issue| issue == "raw file path"));
        assert!(issues.iter().any(|issue| issue == "inline code syntax"));
        assert!(issues.iter().any(|issue| issue == "diff-line syntax"));
    }

    #[test]
    fn transfer_validation_rejects_file_narration_that_precheck_rejects() {
        let fence = char::from(96).to_string().repeat(3);
        let source = format!(
            "<!-- kokoro-reader-section -->\n# Cache queue\n\n{fence}ts\n47 + | function summarizeCacheProjects(tasks) {{}}\n{fence}"
        );
        let narration = source.replace("<!-- kokoro-reader-section -->\n", "");
        let document = document_from_mcp_files("Test".to_string(), &source, &narration)
            .expect("document shape should be valid before quality validation");
        let error = validate_document_narration_quality(&document)
            .expect_err("transfer validation must reject copied narration");
        assert!(error.contains("narration must use natural speech"));
    }

    #[test]
    fn narration_quality_reports_the_copied_phrase() {
        let source = "The memory helper stores durable facts before deciding whether the action worker needs a reminder.";
        let narration = "The memory helper stores durable facts before deciding whether the action worker needs a reminder.";
        let issues = narration_quality_issues(source, narration);
        assert!(issues.iter().any(|issue| issue
            == "long copied source passage (\"the memory helper stores durable facts before deciding whether the action worker\")"));
    }

    #[test]
    fn section_precheck_reports_annotation_coverage_without_blocking_warnings() {
        let source =
            "# Cache queue\n\n```ts\n47 + | queued += 1 // <-- AFTER: adds one pending clip.\n```";
        let narration = "After line forty-seven, the cache queue adds one pending clip.";
        let result = analyze_section(1, source, narration);
        assert!(result.ready);
        assert!(result
            .coverage_blocks
            .iter()
            .any(|block| block.kind == "annotation" && block.covered));
    }

    #[test]
    fn section_precheck_uses_utf16_ranges_for_unicode_diagrams() {
        let source = "# Flow\n\n```text\nA → B → C\n```";
        let narration = "The flow moves from A to B to C.";
        let result = analyze_section(1, source, narration);
        let diagram = result
            .coverage_blocks
            .iter()
            .find(|block| block.kind == "diagram")
            .expect("diagram block");
        assert!(diagram.source_range.end_utf16 > diagram.source_range.start_utf16);
    }

    #[test]
    fn transfer_validation_rejects_low_alignment() {
        let document = Document {
            title: "Test".to_string(),
            sections: vec![DocumentSection {
                markdown: "The cache queue groups article clips and keeps pending navigation."
                    .to_string(),
                speech_text: "A totally unrelated natural explanation discusses weather forecasts."
                    .to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        assert!(validate_document_for_transfer(&document, None).is_err());
    }

    #[test]
    fn precheck_exposes_a_token_for_advisory_findings() {
        let source = "<!-- kokoro-reader-section -->\n# Agent security\nSentinel checks credentials before sending data.";
        let narration = "A mathematician studies fluid equations.";
        let result =
            precheck_document_contents(source, narration).expect("precheck should inspect");
        assert!(!result.ready_for_send);
        assert!(result.override_available);
        assert!(result.override_token.is_some());
    }

    #[test]
    fn section_and_file_precheck_share_the_same_override_token() {
        let markdown = "# Agent security\nSentinel checks credentials before sending data.";
        let narration = "A mathematician studies fluid equations.";
        let section_result = precheck_sections(PrecheckSectionsParams {
            sections: vec![PrecheckSectionInput {
                section_index: 1,
                markdown: markdown.to_string(),
                speech_text: narration.to_string(),
            }],
        })
        .expect("section precheck should inspect");
        let file_result = precheck_document_contents(
            &format!("<!-- kokoro-reader-section -->\n{markdown}"),
            narration,
        )
        .expect("file precheck should inspect");
        assert!(section_result.override_available);
        assert_eq!(section_result.override_token, file_result.override_token);
    }

    #[test]
    fn transfer_validation_accepts_a_current_acknowledgement() {
        let document = Document {
            title: "Test".to_string(),
            sections: vec![DocumentSection {
                markdown: "The cache queue groups article clips and keeps pending navigation."
                    .to_string(),
                speech_text: "A totally unrelated natural explanation discusses weather forecasts."
                    .to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        let token = transfer_precheck(&document)
            .override_token
            .expect("advisory findings produce a token");
        let result = validate_document_for_transfer(
            &document,
            Some(PrecheckAcknowledgement {
                token,
                reason: "The lesson is intentionally contrastive.".to_string(),
                verified_checks: vec![
                    "Reviewed the visible claim and spoken explanation.".to_string()
                ],
            }),
        )
        .expect("current acknowledgement should allow transfer");
        assert!(!result.acknowledged_findings.is_empty());
        assert_eq!(result.verified_checks.len(), 1);
    }

    #[test]
    fn transfer_validation_rejects_stale_or_empty_acknowledgements() {
        let mut document = Document {
            title: "Test".to_string(),
            sections: vec![DocumentSection {
                markdown: "The cache queue groups article clips and keeps pending navigation."
                    .to_string(),
                speech_text: "A totally unrelated natural explanation discusses weather forecasts."
                    .to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        let token = transfer_precheck(&document)
            .override_token
            .expect("advisory findings produce a token");
        document.sections[0].speech_text.push_str(" Today.");
        let stale = validate_document_for_transfer(
            &document,
            Some(PrecheckAcknowledgement {
                token: token.clone(),
                reason: "Reviewed.".to_string(),
                verified_checks: vec!["Checked.".to_string()],
            }),
        )
        .expect_err("changed content invalidates the token");
        assert!(stale.contains("stale"));
        let empty = validate_document_for_transfer(
            &document,
            Some(PrecheckAcknowledgement {
                token: transfer_precheck(&document).override_token.unwrap(),
                reason: " ".to_string(),
                verified_checks: vec![" ".to_string()],
            }),
        )
        .expect_err("empty acknowledgement details fail");
        assert!(empty.contains("reason"));
    }

    #[test]
    fn acknowledgement_does_not_bypass_structural_validation() {
        let document = Document {
            title: "Test".to_string(),
            sections: vec![DocumentSection {
                markdown: "# Empty narration".to_string(),
                speech_text: "".to_string(),
                speech_mode: SpeechMode::Custom,
            }],
        };
        assert!(validate_document_for_transfer(
            &document,
            Some(PrecheckAcknowledgement {
                token: "not-a-real-token".to_string(),
                reason: "Reviewed.".to_string(),
                verified_checks: vec!["Checked.".to_string()],
            }),
        )
        .is_err());
    }

    #[test]
    fn toon_packet_renders_ordered_custom_sections() {
        let packet = serde_json::json!({
            "title": "TOON lesson",
            "cards": [{
                "id": "first",
                "heading": "First card",
                "claim": "The first card explains the shared concept.",
                "connection": "It connects the shared concept to practice.",
                "diagram": "Input -> shared concept -> output",
                "pause_question": "Which concept is shared?",
                "answer": "The shared concept is the bridge.",
                "narration": "First card explains the shared concept and connects it to practice. Pause and predict which concept is shared. The answer is the shared concept is the bridge."
            }, {
                "id": "second",
                "heading": "Second card",
                "claim": "The second card keeps the same shared concept.",
                "connection": null,
                "diagram": null,
                "pause_question": null,
                "answer": null,
                "narration": "Second card keeps the same shared concept."
            }]
        });
        let packet_toon = toon_format::encode_default(&packet).expect("encode TOON fixture");
        let document = render_toon_packet(&packet_toon, None).expect("render TOON packet");
        assert_eq!(document.title, "TOON lesson");
        assert_eq!(document.sections.len(), 2);
        assert!(document.sections[0].markdown.contains("```text"));
        assert!(document
            .sections
            .iter()
            .all(|section| section.speech_mode == SpeechMode::Custom));
    }

    #[test]
    fn toon_packet_rejects_duplicate_ids_and_incomplete_pause() {
        let duplicate = serde_json::json!({"cards":[
            {"id":"same","heading":"One","claim":"One shared claim.","narration":"One shared claim."},
            {"id":"same","heading":"Two","claim":"Two shared claim.","narration":"Two shared claim."}
        ]});
        let duplicate_toon =
            toon_format::encode_default(&duplicate).expect("encode duplicate fixture");
        assert!(render_toon_packet(&duplicate_toon, None)
            .expect_err("duplicate IDs fail")
            .contains("Duplicate TOON card id"));
        let incomplete = serde_json::json!({"cards":[{
            "id":"only","heading":"Only","claim":"Only shared claim.","pause_question":"Question?","narration":"Only shared claim question."
        }]});
        let incomplete_toon =
            toon_format::encode_default(&incomplete).expect("encode incomplete fixture");
        assert!(render_toon_packet(&incomplete_toon, None)
            .expect_err("incomplete pause fails")
            .contains("pause_question and answer"));
    }

    #[test]
    fn gmail_batch_ids_are_strict_and_scoped() {
        assert!(gmail_batch_path("gmail-20260920T064421Z-c33583cf").is_ok());
        assert!(gmail_batch_path("../../secrets").is_err());
        assert!(gmail_batch_path("gmail-batch/escape").is_err());
    }

    #[test]
    fn toon_fixture_reports_compact_uniform_gmail_rows() {
        let fixture = serde_json::json!({
            "clusters": [{"id":"records","verdict":"ignore","message_ids":["a1","a2","a3"]}]
        });
        let json = serde_json::to_string(&fixture).expect("encode JSON fixture");
        let toon = toon_format::encode_default(&fixture).expect("encode TOON fixture");
        eprintln!(
            "gmail fixture bytes: json={} toon={}",
            json.len(),
            toon.len()
        );
        assert!(
            toon.len() < json.len(),
            "TOON should compact this uniform fixture"
        );
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
