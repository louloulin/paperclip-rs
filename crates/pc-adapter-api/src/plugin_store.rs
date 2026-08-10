//! JSON 文件存储 + 内存缓存：external adapter plugin 注册表。
//!
//! 原 `pc-adapter-plugin-store` 已下沉为本 crate 的 `plugin_store` 子模块。
//! 对齐 Node `services/adapter-plugin-store.ts`：
//! - 路径：`<paperclip_home>/adapter-plugins.json` 和 `adapter-settings.json`
//! - `AdapterPluginRecord`（npm 包名 / 本地路径 / 版本 / type / installedAt / disabled）
//! - `AdapterSettings { disabledTypes: Vec<String> }`
//! - 第一次读取后内存缓存；写入时同时失效缓存
//! - `ensureDirs`: 创建 `adapter-plugins/` 目录 + 写入 `package.json`（首次）

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use thiserror::Error;
use tokio::fs;

const ADAPTER_PLUGINS_DIRNAME: &str = "adapter-plugins";
const ADAPTER_PLUGINS_STORE_FILENAME: &str = "adapter-plugins.json";
const ADAPTER_SETTINGS_FILENAME: &str = "adapter-settings.json";

/// Adapter plugin 注册记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdapterPluginRecord {
    /// npm 包名（如 `"droid-paperclip-adapter"`）。
    pub package_name: String,
    /// 绝对本地文件系统路径（用于本地 link 的 adapter）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// 已安装版本字符串（用于 npm 包）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Adapter type 标识（与 ServerAdapterModule.type 对应）。
    #[serde(rename = "type")]
    pub kind: String,
    /// ISO 8601 安装时间戳。
    pub installed_at: String,
    /// 是否禁用（不在菜单中显示但仍可调用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

/// Adapter 设置（disabled type 列表）。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdapterSettings {
    #[serde(default)]
    pub disabled_types: Vec<String>,
}

/// Store error。
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("io error while {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// 内存缓存 + 磁盘路径。
#[derive(Debug)]
struct Cache<T> {
    path: PathBuf,
    value: T,
}

/// Adapter plugin store。
#[derive(Debug)]
pub struct AdapterPluginStore {
    paperclip_home: PathBuf,
    store_cache: Mutex<Option<Cache<Vec<AdapterPluginRecord>>>>,
    settings_cache: Mutex<Option<Cache<AdapterSettings>>>,
}

impl AdapterPluginStore {
    pub fn new(paperclip_home: impl Into<PathBuf>) -> Self {
        Self {
            paperclip_home: paperclip_home.into(),
            store_cache: Mutex::new(None),
            settings_cache: Mutex::new(None),
        }
    }

    fn store_path(&self) -> PathBuf {
        self.paperclip_home.join(ADAPTER_PLUGINS_STORE_FILENAME)
    }

    fn settings_path(&self) -> PathBuf {
        self.paperclip_home.join(ADAPTER_SETTINGS_FILENAME)
    }

    fn plugins_dir(&self) -> PathBuf {
        self.paperclip_home.join(ADAPTER_PLUGINS_DIRNAME)
    }

    /// 异步版 `ensureDirs`：创建 `<home>/adapter-plugins/` + 写入 `package.json`。
    pub async fn ensure_dirs(&self) -> Result<PathBuf, StoreError> {
        let dir = self.plugins_dir();
        fs::create_dir_all(&dir).await.map_err(|e| StoreError::Io {
            operation: "mkdir adapter-plugins",
            source: e,
        })?;
        let pkg_json = dir.join("package.json");
        if !fs::try_exists(&pkg_json).await.unwrap_or(false) {
            let body = serde_json::json!({
                "name": "paperclip-adapter-plugins",
                "version": "0.0.0",
                "private": true,
                "description": "Managed directory for Paperclip external adapter plugins. Do not edit manually."
            });
            let mut s = serde_json::to_string_pretty(&body)?;
            s.push('\n');
            fs::write(&pkg_json, s).await.map_err(|e| StoreError::Io {
                operation: "write package.json",
                source: e,
            })?;
        }
        Ok(dir)
    }

    pub async fn list(&self) -> Result<Vec<AdapterPluginRecord>, StoreError> {
        self.read_store().await
    }

    /// 与 Node 1:1：相同 `type` 则替换；否则追加。
    pub async fn add(&self, record: AdapterPluginRecord) -> Result<(), StoreError> {
        let mut store = self.read_store().await?;
        if let Some(idx) = store.iter().position(|r| r.kind == record.kind) {
            store[idx] = record;
        } else {
            store.push(record);
        }
        self.write_store(&store).await
    }

    /// 删除指定 type 的 record；返回是否真的删除了。
    pub async fn remove(&self, kind: &str) -> Result<bool, StoreError> {
        let mut store = self.read_store().await?;
        if let Some(idx) = store.iter().position(|r| r.kind == kind) {
            store.remove(idx);
            self.write_store(&store).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn get_by_type(
        &self,
        kind: &str,
    ) -> Result<Option<AdapterPluginRecord>, StoreError> {
        let store = self.read_store().await?;
        Ok(store.into_iter().find(|r| r.kind == kind))
    }

    /// `getAdapterPluginsDir` — ensure_dirs 后返回路径。
    pub async fn plugins_dir_path(&self) -> Result<PathBuf, StoreError> {
        self.ensure_dirs().await
    }

    // ---- settings ----

    pub async fn get_disabled_types(&self) -> Result<Vec<String>, StoreError> {
        Ok(self.read_settings().await?.disabled_types)
    }

    pub async fn is_disabled(&self, kind: &str) -> Result<bool, StoreError> {
        let s = self.read_settings().await?;
        Ok(s.disabled_types.iter().any(|t| t == kind))
    }

    /// 切换 disabled 状态。返回 `true` 表示写入了文件；状态未变时返回 `false`。
    pub async fn set_disabled(
        &self,
        kind: &str,
        disabled: bool,
    ) -> Result<bool, StoreError> {
        let mut settings = self.read_settings().await?;
        let idx = settings.disabled_types.iter().position(|t| t == kind);
        match (disabled, idx) {
            (true, None) => {
                settings.disabled_types.push(kind.to_string());
                self.write_settings(&settings).await?;
                Ok(true)
            }
            (false, Some(i)) => {
                settings.disabled_types.remove(i);
                self.write_settings(&settings).await?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    // ---- internal read/write with caching ----

    async fn read_store(&self) -> Result<Vec<AdapterPluginRecord>, StoreError> {
        let path = self.store_path();
        if let Some(cache) = self.store_cache.lock().unwrap().as_ref() {
            if cache.path == path {
                return Ok(cache.value.clone());
            }
        }
        let records = match fs::read_to_string(&path).await {
            Ok(raw) => match serde_json::from_str::<Vec<AdapterPluginRecord>>(&raw) {
                Ok(records) => records,
                Err(_) => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        *self.store_cache.lock().unwrap() = Some(Cache {
            path,
            value: records.clone(),
        });
        Ok(records)
    }

    async fn write_store(&self, records: &[AdapterPluginRecord]) -> Result<(), StoreError> {
        self.ensure_dirs().await?;
        let path = self.store_path();
        let mut s = serde_json::to_string_pretty(records)?;
        s.push('\n');
        fs::write(&path, s).await.map_err(|e| StoreError::Io {
            operation: "write adapter-plugins store",
            source: e,
        })?;
        *self.store_cache.lock().unwrap() = Some(Cache {
            path,
            value: records.to_vec(),
        });
        Ok(())
    }

    async fn read_settings(&self) -> Result<AdapterSettings, StoreError> {
        let path = self.settings_path();
        if let Some(cache) = self.settings_cache.lock().unwrap().as_ref() {
            if cache.path == path {
                return Ok(cache.value.clone());
            }
        }
        let settings = match fs::read_to_string(&path).await {
            Ok(raw) => serde_json::from_str::<AdapterSettings>(&raw).unwrap_or_default(),
            Err(_) => AdapterSettings::default(),
        };
        *self.settings_cache.lock().unwrap() = Some(Cache {
            path: path.clone(),
            value: settings.clone(),
        });
        Ok(settings)
    }

    async fn write_settings(&self, settings: &AdapterSettings) -> Result<(), StoreError> {
        self.ensure_dirs().await?;
        let path = self.settings_path();
        let mut s = serde_json::to_string_pretty(settings)?;
        s.push('\n');
        fs::write(&path, s).await.map_err(|e| StoreError::Io {
            operation: "write adapter-settings",
            source: e,
        })?;
        *self.settings_cache.lock().unwrap() = Some(Cache {
            path,
            value: settings.clone(),
        });
        Ok(())
    }

    /// 仅供测试 / 调试：暴露 paperclip_home 路径。
    pub fn paperclip_home(&self) -> &Path {
        &self.paperclip_home
    }
}
