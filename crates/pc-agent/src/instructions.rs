use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use pc_errors::{internal, not_found, unprocessable, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const ENTRY_FILE_DEFAULT: &str = "AGENTS.md";
const LEGACY_PROMPT_PATH: &str = "promptTemplate.legacy.md";
const MODE_KEY: &str = "instructionsBundleMode";
const ROOT_KEY: &str = "instructionsRootPath";
const ENTRY_KEY: &str = "instructionsEntryFile";
const FILE_KEY: &str = "instructionsFilePath";
const PROMPT_KEY: &str = "promptTemplate";
const BOOTSTRAP_PROMPT_KEY: &str = "bootstrapPromptTemplate";

#[derive(Debug, Clone)]
pub struct InstructionAgent {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub adapter_config: Value,
}

impl From<&pc_repos::agent::AgentRow> for InstructionAgent {
    fn from(agent: &pc_repos::agent::AgentRow) -> Self {
        Self {
            id: agent.id.to_string(),
            company_id: agent.company_id.to_string(),
            name: agent.name.clone(),
            adapter_config: agent.adapter_config.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstructionsFileSummary {
    pub path: String,
    pub size: u64,
    pub language: String,
    pub markdown: bool,
    pub is_entry_file: bool,
    pub editable: bool,
    pub deprecated: bool,
    #[serde(rename = "virtual")]
    pub virtual_file: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstructionsFileDetail {
    #[serde(flatten)]
    pub summary: AgentInstructionsFileSummary,
    pub content: String,
}

impl std::ops::Deref for AgentInstructionsFileDetail {
    type Target = AgentInstructionsFileSummary;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInstructionsBundle {
    pub agent_id: String,
    pub company_id: String,
    pub mode: Option<String>,
    pub root_path: Option<String>,
    pub managed_root_path: String,
    pub entry_file: String,
    pub resolved_entry_path: Option<String>,
    pub editable: bool,
    pub warnings: Vec<String>,
    pub legacy_prompt_template_active: bool,
    pub legacy_bootstrap_prompt_template_active: bool,
    pub files: Vec<AgentInstructionsFileSummary>,
}

#[derive(Debug, Clone, Default)]
pub struct InstructionsBundleUpdate {
    pub mode: Option<String>,
    pub root_path: Option<Option<String>>,
    pub entry_file: Option<String>,
    pub clear_legacy_prompt_template: bool,
}

#[derive(Debug, Clone)]
pub struct InstructionsUpdateResult {
    pub bundle: AgentInstructionsBundle,
    pub adapter_config: Value,
}

#[derive(Debug, Clone)]
pub struct WriteInstructionsFileResult {
    pub bundle: AgentInstructionsBundle,
    pub file: AgentInstructionsFileDetail,
    pub adapter_config: Value,
}

#[derive(Debug, Clone)]
pub struct DeleteInstructionsFileResult {
    pub bundle: AgentInstructionsBundle,
    pub adapter_config: Value,
}

#[derive(Debug, Clone)]
struct BundleState {
    config: Map<String, Value>,
    mode: Option<String>,
    root_path: Option<PathBuf>,
    entry_file: String,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AgentInstructionsService {
    instance_root: PathBuf,
}

impl AgentInstructionsService {
    #[must_use]
    pub fn new(instance_root: PathBuf) -> Self {
        Self { instance_root }
    }

    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var_os("PAPERCLIP_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|path| path.join(".paperclip")))
            .unwrap_or_else(|| PathBuf::from(".paperclip"));
        let instance = std::env::var("PAPERCLIP_INSTANCE_ID").unwrap_or_else(|_| "default".into());
        Self::new(home.join("instances").join(instance))
    }

    #[must_use]
    pub fn managed_root(&self, agent: &InstructionAgent) -> PathBuf {
        self.instance_root
            .join("companies")
            .join(&agent.company_id)
            .join("agents")
            .join(&agent.id)
            .join("instructions")
    }

    pub fn sync_bundle_config_from_path(
        &self,
        agent: &InstructionAgent,
        instructions_file_path: Option<&str>,
    ) -> Result<Map<String, Value>> {
        let config = config_object(&agent.adapter_config);
        match instructions_file_path {
            None | Some("") => {
                let mut config = config;
                config.remove(MODE_KEY);
                config.remove(ROOT_KEY);
                config.remove(ENTRY_KEY);
                config.remove(FILE_KEY);
                Ok(config)
            }
            Some(value) => {
                let resolved = resolve_legacy_path(value, &config)?;
                let root = resolved
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("/"));
                let entry = resolved
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        unprocessable("Instructions file path must include a file name")
                    })?
                    .to_owned();
                let mode = if root.starts_with(self.managed_root(agent)) {
                    "managed"
                } else {
                    "external"
                };
                Ok(apply_bundle_config(config, mode, &root, &entry, false))
            }
        }
    }

    pub async fn get_bundle(&self, agent: &InstructionAgent) -> Result<AgentInstructionsBundle> {
        let state = self.derive_state(agent).await?;
        let mut files = if let Some(root) = state.root_path.as_deref() {
            if is_directory(root).await {
                let paths = list_files_recursive(root).await?;
                let mut summaries = Vec::with_capacity(paths.len());
                for path in paths {
                    summaries.push(file_summary(root, &path, &state.entry_file).await?);
                }
                summaries
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        if let Some(prompt) = string_value(state.config.get(PROMPT_KEY)) {
            files.push(virtual_summary(LEGACY_PROMPT_PATH, prompt.len() as u64));
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(self.to_bundle(agent, state, files))
    }

    pub async fn read_file(
        &self,
        agent: &InstructionAgent,
        relative_path: &str,
    ) -> Result<AgentInstructionsFileDetail> {
        let state = self.derive_state(agent).await?;
        if relative_path == LEGACY_PROMPT_PATH {
            let content = string_value(state.config.get(PROMPT_KEY))
                .ok_or_else(|| not_found("Instructions file"))?
                .to_owned();
            return Ok(AgentInstructionsFileDetail {
                summary: virtual_summary(LEGACY_PROMPT_PATH, content.len() as u64),
                content,
            });
        }
        let root = state
            .root_path
            .as_deref()
            .ok_or_else(|| not_found("Agent instructions bundle"))?;
        let normalized = normalize_relative_path(relative_path)?;
        let absolute = resolve_within_root(root, &normalized)?;
        let content = tokio::fs::read_to_string(&absolute)
            .await
            .map_err(|error| map_read_error(error, "Instructions file"))?;
        let summary = file_summary(root, &normalized, &state.entry_file).await?;
        Ok(AgentInstructionsFileDetail { summary, content })
    }

    pub async fn update_bundle(
        &self,
        agent: &InstructionAgent,
        input: InstructionsBundleUpdate,
    ) -> Result<InstructionsUpdateResult> {
        let current = self.derive_state(agent).await?;
        let mode = input
            .mode
            .as_deref()
            .or(current.mode.as_deref())
            .unwrap_or("managed");
        if !matches!(mode, "managed" | "external") {
            return Err(unprocessable("Instructions bundle mode must be managed or external"));
        }
        let entry = normalize_relative_path(
            input.entry_file.as_deref().unwrap_or(&current.entry_file),
        )?;
        let root = if mode == "managed" {
            self.managed_root(agent)
        } else {
            let configured = input
                .root_path
                .flatten()
                .map(PathBuf::from)
                .or(current.root_path)
                .ok_or_else(|| {
                    unprocessable("External instructions bundles require an absolute rootPath")
                })?;
            let expanded = expand_home(configured);
            if !expanded.is_absolute() {
                return Err(unprocessable(
                    "External instructions bundles require an absolute rootPath",
                ));
            }
            expanded
        };
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(map_write_error)?;
        if list_files_recursive(&root).await?.is_empty() {
            let exported = self.export_files(agent).await?;
            for (path, content) in exported {
                write_path(&root, &path, &content).await?;
            }
        }
        let entry_path = resolve_within_root(&root, &entry)?;
        if tokio::fs::metadata(&entry_path).await.is_err() {
            write_path(&root, &entry, "").await?;
        }
        let config = apply_bundle_config(
            current.config,
            mode,
            &root,
            &entry,
            input.clear_legacy_prompt_template,
        );
        let configured = InstructionAgent {
            adapter_config: Value::Object(config.clone()),
            ..agent.clone()
        };
        Ok(InstructionsUpdateResult {
            bundle: self.get_bundle(&configured).await?,
            adapter_config: Value::Object(config),
        })
    }

    pub async fn write_file(
        &self,
        agent: &InstructionAgent,
        relative_path: &str,
        content: &str,
        clear_legacy_prompt_template: bool,
    ) -> Result<WriteInstructionsFileResult> {
        if relative_path == LEGACY_PROMPT_PATH {
            let mut config = config_object(&agent.adapter_config);
            config.insert(PROMPT_KEY.into(), Value::String(content.into()));
            let configured = InstructionAgent {
                adapter_config: Value::Object(config.clone()),
                ..agent.clone()
            };
            return Ok(WriteInstructionsFileResult {
                bundle: self.get_bundle(&configured).await?,
                file: self.read_file(&configured, LEGACY_PROMPT_PATH).await?,
                adapter_config: Value::Object(config),
            });
        }
        let (state, config) = self
            .ensure_writable(agent, clear_legacy_prompt_template)
            .await?;
        let root = state.root_path.as_deref().expect("writable bundle has root");
        let normalized = normalize_relative_path(relative_path)?;
        write_path(root, &normalized, content).await?;
        let configured = InstructionAgent {
            adapter_config: Value::Object(config.clone()),
            ..agent.clone()
        };
        Ok(WriteInstructionsFileResult {
            bundle: self.get_bundle(&configured).await?,
            file: self.read_file(&configured, &normalized).await?,
            adapter_config: Value::Object(config),
        })
    }

    pub async fn delete_file(
        &self,
        agent: &InstructionAgent,
        relative_path: &str,
    ) -> Result<DeleteInstructionsFileResult> {
        if relative_path == LEGACY_PROMPT_PATH {
            return Err(unprocessable(
                "Cannot delete the legacy promptTemplate pseudo-file",
            ));
        }
        let state = self.derive_state(agent).await?;
        let normalized = normalize_relative_path(relative_path)?;
        if normalized == state.entry_file {
            return Err(unprocessable("Cannot delete the bundle entry file"));
        }
        let root = state
            .root_path
            .as_deref()
            .ok_or_else(|| not_found("Agent instructions bundle"))?;
        let absolute = resolve_within_root(root, &normalized)?;
        match tokio::fs::remove_file(absolute).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(map_write_error(error)),
        }
        let config = apply_bundle_config(
            state.config,
            state.mode.as_deref().unwrap_or("managed"),
            root,
            &state.entry_file,
            false,
        );
        let configured = InstructionAgent {
            adapter_config: Value::Object(config.clone()),
            ..agent.clone()
        };
        Ok(DeleteInstructionsFileResult {
            bundle: self.get_bundle(&configured).await?,
            adapter_config: Value::Object(config),
        })
    }

    async fn ensure_writable(
        &self,
        agent: &InstructionAgent,
        clear_legacy_prompt_template: bool,
    ) -> Result<(BundleState, Map<String, Value>)> {
        let current = self.derive_state(agent).await?;
        let mode = current.mode.as_deref().unwrap_or("managed");
        let root = current
            .root_path
            .clone()
            .unwrap_or_else(|| self.managed_root(agent));
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(map_write_error)?;
        let entry = current.entry_file.clone();
        let entry_path = resolve_within_root(&root, &entry)?;
        if tokio::fs::metadata(&entry_path).await.is_err() {
            let legacy = string_value(current.config.get(PROMPT_KEY))
                .or_else(|| string_value(current.config.get(BOOTSTRAP_PROMPT_KEY)))
                .unwrap_or("");
            write_path(&root, &entry, legacy).await?;
        }
        let config = apply_bundle_config(
            current.config,
            mode,
            &root,
            &entry,
            clear_legacy_prompt_template,
        );
        let state = BundleState {
            config: config.clone(),
            mode: Some(mode.into()),
            root_path: Some(root),
            entry_file: entry,
            warnings: current.warnings,
        };
        Ok((state, config))
    }

    async fn derive_state(&self, agent: &InstructionAgent) -> Result<BundleState> {
        let config = config_object(&agent.adapter_config);
        let managed_root = self.managed_root(agent);
        let configured_mode = string_value(config.get(MODE_KEY))
            .filter(|mode| matches!(*mode, "managed" | "external"))
            .map(str::to_owned);
        let configured_root = string_value(config.get(ROOT_KEY)).map(PathBuf::from);
        let mut warnings = Vec::new();
        let mut mode = configured_mode;
        let mut root = configured_root;
        let mut entry = string_value(config.get(ENTRY_KEY))
            .unwrap_or(ENTRY_FILE_DEFAULT)
            .to_owned();
        entry = normalize_relative_path(&entry)?;

        if mode.as_deref() == Some("managed") {
            if root.as_ref().is_some_and(|path| path != &managed_root) && is_directory(&managed_root).await {
                warnings.push(format!(
                    "Recovered managed instructions from disk at {}; ignoring stale configured root {}.",
                    managed_root.display(),
                    root.as_ref().expect("checked root").display()
                ));
            }
            root = Some(managed_root.clone());
        } else if mode.is_none() && is_directory(&managed_root).await {
            if let Some(stale) = root.as_ref().filter(|path| *path != &managed_root) {
                warnings.push(format!(
                    "Recovered managed instructions from disk at {}; ignoring stale configured root {}.",
                    managed_root.display(),
                    stale.display()
                ));
            }
            mode = Some("managed".into());
            root = Some(managed_root.clone());
        } else if root.is_none() {
            if let Some(file_path) = string_value(config.get(FILE_KEY)) {
                let resolved = resolve_legacy_path(file_path, &config)?;
                entry = resolved
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(ENTRY_FILE_DEFAULT)
                    .to_owned();
                root = resolved.parent().map(Path::to_path_buf);
                mode = Some(if resolved.starts_with(&managed_root) {
                    "managed"
                } else {
                    "external"
                }
                .into());
            }
        }
        if let Some(root_path) = root.as_deref() {
            if !root_path.is_absolute() {
                return Err(unprocessable("Instructions rootPath must be absolute"));
            }
            let entry_path = resolve_within_root(root_path, &entry)?;
            if mode.as_deref() == Some("managed")
                && tokio::fs::metadata(&entry_path).await.is_err()
                && tokio::fs::metadata(root_path.join(ENTRY_FILE_DEFAULT)).await.is_ok()
            {
                warnings.push(format!(
                    "Recovered managed instructions entry file from disk as AGENTS.md; previous entry {entry} was missing."
                ));
                entry = ENTRY_FILE_DEFAULT.into();
            }
        }
        Ok(BundleState {
            config,
            mode,
            root_path: root,
            entry_file: entry,
            warnings,
        })
    }

    async fn export_files(&self, agent: &InstructionAgent) -> Result<BTreeMap<String, String>> {
        let state = self.derive_state(agent).await?;
        let mut files = BTreeMap::new();
        if let Some(root) = state.root_path.as_deref().filter(|root| root.is_dir()) {
            for relative in list_files_recursive(root).await? {
                let absolute = resolve_within_root(root, &relative)?;
                let content = tokio::fs::read_to_string(absolute)
                    .await
                    .map_err(map_write_error)?;
                files.insert(relative, content);
            }
        }
        if files.is_empty() {
            let legacy = string_value(state.config.get(PROMPT_KEY))
                .or_else(|| string_value(state.config.get(BOOTSTRAP_PROMPT_KEY)))
                .unwrap_or("_No AGENTS instructions were resolved from current agent config._");
            files.insert(state.entry_file, legacy.into());
        }
        Ok(files)
    }

    fn to_bundle(
        &self,
        agent: &InstructionAgent,
        state: BundleState,
        files: Vec<AgentInstructionsFileSummary>,
    ) -> AgentInstructionsBundle {
        let resolved_entry_path = state
            .root_path
            .as_ref()
            .map(|root| root.join(&state.entry_file).to_string_lossy().into_owned());
        AgentInstructionsBundle {
            agent_id: agent.id.clone(),
            company_id: agent.company_id.clone(),
            mode: state.mode,
            root_path: state
                .root_path
                .map(|path| path.to_string_lossy().into_owned()),
            managed_root_path: self.managed_root(agent).to_string_lossy().into_owned(),
            entry_file: state.entry_file,
            resolved_entry_path,
            editable: true,
            warnings: state.warnings,
            legacy_prompt_template_active: string_value(state.config.get(PROMPT_KEY)).is_some(),
            legacy_bootstrap_prompt_template_active: string_value(
                state.config.get(BOOTSTRAP_PROMPT_KEY),
            )
            .is_some(),
            files,
        }
    }
}

fn config_object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn string_value(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_relative_path(path: &str) -> Result<String> {
    let replaced = path.replace('\\', "/");
    let candidate = Path::new(&replaced);
    if replaced.trim().is_empty() || candidate.is_absolute() {
        return Err(unprocessable(
            "Instructions file path must stay within the bundle root",
        ));
    }
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(unprocessable(
                    "Instructions file path must stay within the bundle root",
                ));
            }
        }
    }
    let normalized = normalized.to_string_lossy().replace('\\', "/");
    if normalized.is_empty() {
        return Err(unprocessable(
            "Instructions file path must stay within the bundle root",
        ));
    }
    Ok(normalized)
}

fn resolve_within_root(root: &Path, relative: &str) -> Result<PathBuf> {
    let relative = normalize_relative_path(relative)?;
    Ok(root.join(relative))
}

fn resolve_legacy_path(candidate: &str, config: &Map<String, Value>) -> Result<PathBuf> {
    let path = PathBuf::from(candidate);
    if path.is_absolute() {
        return Ok(path);
    }
    let cwd = string_value(config.get("cwd"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            unprocessable(
                "Legacy relative instructionsFilePath requires adapterConfig.cwd to be set to an absolute path",
            )
        })?;
    Ok(cwd.join(path))
}

fn expand_home(path: PathBuf) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path;
    };
    if value == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(suffix) = value.strip_prefix("~/") {
        return dirs::home_dir().map_or(path.clone(), |home| home.join(suffix));
    }
    path
}

fn apply_bundle_config(
    mut config: Map<String, Value>,
    mode: &str,
    root: &Path,
    entry: &str,
    clear_legacy: bool,
) -> Map<String, Value> {
    config.insert(MODE_KEY.into(), Value::String(mode.into()));
    config.insert(
        ROOT_KEY.into(),
        Value::String(root.to_string_lossy().into_owned()),
    );
    config.insert(ENTRY_KEY.into(), Value::String(entry.into()));
    config.insert(
        FILE_KEY.into(),
        Value::String(root.join(entry).to_string_lossy().into_owned()),
    );
    if clear_legacy {
        config.remove(PROMPT_KEY);
        config.remove(BOOTSTRAP_PROMPT_KEY);
    }
    config
}

async fn write_path(root: &Path, relative: &str, content: &str) -> Result<()> {
    let absolute = resolve_within_root(root, relative)?;
    if let Some(parent) = absolute.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(map_write_error)?;
    }
    tokio::fs::write(absolute, content)
        .await
        .map_err(map_write_error)
}

async fn list_files_recursive(root: &Path) -> Result<Vec<String>> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&directory)
            .await
            .map_err(map_write_error)?;
        while let Some(entry) = entries.next_entry().await.map_err(map_write_error)? {
            let file_type = entry.file_type().await.map_err(map_write_error)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if should_ignore(&name, file_type.is_dir()) || file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| internal(error.to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn should_ignore(name: &str, directory: bool) -> bool {
    if directory {
        matches!(
            name,
            ".git"
                | ".nox"
                | ".pytest_cache"
                | ".ruff_cache"
                | ".tox"
                | ".venv"
                | "__pycache__"
                | "node_modules"
                | "venv"
        )
    } else {
        matches!(name, ".DS_Store" | "Thumbs.db" | "Desktop.ini")
    }
}

async fn file_summary(root: &Path, relative: &str, entry: &str) -> Result<AgentInstructionsFileSummary> {
    let absolute = resolve_within_root(root, relative)?;
    let metadata = tokio::fs::metadata(absolute).await.map_err(map_write_error)?;
    Ok(AgentInstructionsFileSummary {
        path: relative.into(),
        size: metadata.len(),
        language: infer_language(relative).into(),
        markdown: relative.to_ascii_lowercase().ends_with(".md"),
        is_entry_file: relative == entry,
        editable: true,
        deprecated: false,
        virtual_file: false,
    })
}

fn virtual_summary(path: &str, size: u64) -> AgentInstructionsFileSummary {
    AgentInstructionsFileSummary {
        path: path.into(),
        size,
        language: "markdown".into(),
        markdown: true,
        is_entry_file: false,
        editable: true,
        deprecated: true,
        virtual_file: true,
    }
}

fn infer_language(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".md") {
        "markdown"
    } else if lower.ends_with(".json") {
        "json"
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "yaml"
    } else if lower.ends_with(".ts") || lower.ends_with(".tsx") {
        "typescript"
    } else if lower.ends_with(".js")
        || lower.ends_with(".jsx")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjs")
    {
        "javascript"
    } else if lower.ends_with(".sh") {
        "bash"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".toml") {
        "toml"
    } else {
        "text"
    }
}

async fn is_directory(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
}

fn map_read_error(error: std::io::Error, resource: &str) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        not_found(resource)
    } else {
        internal(format!("instructions read failed: {error}"))
    }
}

fn map_write_error(error: std::io::Error) -> Error {
    internal(format!("instructions filesystem operation failed: {error}"))
}
