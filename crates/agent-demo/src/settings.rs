//! LLM settings, stored as JSON in the data directory (`settings.json`).
//!
//! The API key is write-only over the API: a stored key wins, otherwise the environment variable
//! named by `api_key_env` (`MELIOUS_API_KEY` by default, also read from `./.env` at startup).

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use agent_demo_proto::agentdemo::v1 as pb;
use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://api.melious.ai";
pub const DEFAULT_MODELS: [&str; 2] = ["deepseek-v4-flash-0731", "qwen3.5-397b-a17b"];

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// What the file holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub models: Vec<String>,
    pub api_key_env: String,
    /// Stored key; empty means "use the environment variable".
    pub api_key: String,
    pub temperature: f64,
    pub max_tokens: u32,
    pub system_prompt: String,
    pub max_tool_rounds: u32,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODELS[0].to_owned(),
            models: DEFAULT_MODELS.map(str::to_owned).to_vec(),
            api_key_env: "MELIOUS_API_KEY".to_owned(),
            api_key: String::new(),
            temperature: 0.3,
            max_tokens: 16_384,
            system_prompt: String::new(),
            max_tool_rounds: 16,
        }
    }
}

impl LlmConfig {
    /// The key to send: the stored one, else the environment variable.
    pub fn resolved_key(&self) -> Option<String> {
        if !self.api_key.is_empty() {
            return Some(self.api_key.clone());
        }
        std::env::var(&self.api_key_env)
            .ok()
            .filter(|k| !k.trim().is_empty())
    }

    /// The settings as the UI sees them (no key).
    pub fn to_proto(&self) -> pb::LlmSettings {
        pb::LlmSettings {
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            models: self.models.clone(),
            api_key_env: self.api_key_env.clone(),
            api_key_set: self.resolved_key().is_some(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            system_prompt: self.system_prompt.clone(),
            max_tool_rounds: self.max_tool_rounds,
            ..Default::default()
        }
    }

    /// Applies what the UI saved; the key only changes when one is given (or cleared).
    fn merge(&mut self, s: &pb::LlmSettings) -> Result<(), SettingsError> {
        let base_url = s.base_url.trim().trim_end_matches('/');
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(SettingsError::Invalid(format!(
                "base URL must be http(s): {base_url:?}"
            )));
        }
        let mut models: Vec<String> = s
            .models
            .iter()
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty())
            .collect();
        let model = s.model.trim();
        if model.is_empty() {
            return Err(SettingsError::Invalid("a model is required".into()));
        }
        if !models.iter().any(|m| m == model) {
            models.insert(0, model.to_owned());
        }
        if !(0.0..=2.0).contains(&s.temperature) {
            return Err(SettingsError::Invalid(
                "temperature must be between 0 and 2".into(),
            ));
        }
        base_url.clone_into(&mut self.base_url);
        model.clone_into(&mut self.model);
        self.models = models;
        if !s.api_key_env.trim().is_empty() {
            s.api_key_env.trim().clone_into(&mut self.api_key_env);
        }
        if s.clear_api_key {
            self.api_key.clear();
        } else if !s.api_key.trim().is_empty() {
            s.api_key.trim().clone_into(&mut self.api_key);
        }
        self.temperature = s.temperature;
        self.max_tokens = s.max_tokens.clamp(256, 65_536);
        self.system_prompt.clone_from(&s.system_prompt);
        self.max_tool_rounds = s.max_tool_rounds.clamp(1, 64);
        Ok(())
    }
}

#[derive(Debug)]
pub struct Settings {
    path: PathBuf,
    llm: Mutex<LlmConfig>,
}

impl Settings {
    /// Loads `dir/settings.json`; a missing or unreadable file gives the defaults (logged).
    pub fn load(dir: &std::path::Path) -> Self {
        let path = dir.join("settings.json");
        let llm = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "bad settings file; using defaults");
                LlmConfig::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LlmConfig::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot read settings; using defaults");
                LlmConfig::default()
            }
        };
        Self {
            path,
            llm: Mutex::new(llm),
        }
    }

    pub fn llm(&self) -> LlmConfig {
        self.llm
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn save(&self, update: &pb::LlmSettings) -> Result<LlmConfig, SettingsError> {
        let mut llm = self.llm.lock().unwrap_or_else(PoisonError::into_inner);
        let mut next = llm.clone();
        next.merge(update)?;
        let text = serde_json::to_string_pretty(&next).expect("settings serialize");
        let write = |path: &std::path::Path| -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Write then rename, so a crash never leaves half a file.
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, path)
        };
        write(&self.path).map_err(|source| SettingsError::Write {
            path: self.path.clone(),
            source,
        })?;
        *llm = next.clone();
        Ok(next)
    }
}

/// Sets variables from `path` (`KEY=value` lines, `#` comments, optional quotes) that are not
/// already set, like a minimal dotenv. A missing file is fine.
pub fn load_dotenv(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.starts_with('#') || std::env::var_os(key).is_some() {
            continue;
        }
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        set_env(key, value);
    }
}

#[allow(unsafe_code)]
fn set_env(key: &str, value: &str) {
    // SAFETY: called from `main` before the tokio runtime or any other thread starts.
    unsafe { std::env::set_var(key, value) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_keeps_the_key_unless_replaced_and_never_returns_it() {
        let dir = std::env::temp_dir().join(format!("agent-demo-settings-{}", std::process::id()));
        let settings = Settings::load(&dir);
        let mut update = settings.llm().to_proto();
        update.api_key = "sk-test".into();
        settings.save(&update).unwrap();
        assert_eq!(settings.llm().api_key, "sk-test");
        let shown = settings.llm().to_proto();
        assert!(shown.api_key.is_empty() && shown.api_key_set);
        // Saving again without a key keeps it; the file survives a reload.
        settings.save(&shown).unwrap();
        assert_eq!(Settings::load(&dir).llm().api_key, "sk-test");
        let mut clear = shown.clone();
        clear.clear_api_key = true;
        assert_eq!(settings.save(&clear).unwrap().api_key, "");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bad_values_are_refused() {
        let settings = Settings::load(std::path::Path::new("/nonexistent-agent-demo"));
        let mut update = settings.llm().to_proto();
        update.base_url = "ftp://x".into();
        assert!(matches!(
            settings.save(&update),
            Err(SettingsError::Invalid(_))
        ));
    }
}
