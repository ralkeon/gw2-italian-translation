//! config.toml in the addon folder. Edited from the in-game window or by hand (read at load).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub deepl: Deepl,
    pub gemini: Gemini,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Deepl {
    /// DeepL API key (https://www.deepl.com/your-account/keys). Empty: no translations.
    /// Keys ending in ":fx" are API Free keys and use the api-free endpoint.
    pub api_key: String,
    /// Most texts sent in one request (DeepL accepts up to 50).
    pub batch_max: usize,
    /// How long to keep collecting after the first new text: a window or a dialogue arrives in
    /// one burst and is translated together, as context for each other.
    pub batch_wait_ms: u64,
}

impl Default for Deepl {
    fn default() -> Self {
        Self { api_key: String::new(), batch_max: 50, batch_wait_ms: 400 }
    }
}

/// LLM that will review the machine translations (not used yet).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Gemini {
    /// Google AI Studio key (https://aistudio.google.com/apikey).
    pub api_key: String,
    pub model: String,
}

impl Default for Gemini {
    fn default() -> Self {
        Self { api_key: String::new(), model: "gemini-3.8-flash".into() }
    }
}

static CONFIG: Mutex<Option<(PathBuf, Config)>> = Mutex::new(None);

/// contributor.json in the addon folder: whoever has it gets the translation editor.
#[derive(Deserialize)]
struct Contributor {
    user: String,
}

static CONTRIBUTOR: Mutex<Option<String>> = Mutex::new(None);

/// Reads contributor.json (at load). Missing: no editor.
pub fn load_contributor(path: &Path) {
    let user = match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Contributor>(&text) {
            Ok(c) if !c.user.trim().is_empty() => Some(c.user.trim().to_owned()),
            Ok(_) => {
                log::error!("{}: \"user\" is empty", path.display());
                None
            }
            Err(e) => {
                log::error!("{}: {e}", path.display());
                None
            }
        },
        Err(_) => None,
    };
    if let Some(user) = &user {
        log::info!("contributor {user}: editor on");
    }
    *CONTRIBUTOR.lock().unwrap() = user;
}

/// The contributor's name, if this installation has the editor.
pub fn contributor() -> Option<String> {
    CONTRIBUTOR.lock().ok()?.clone()
}

pub fn load(path: &Path) {
    let config = match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            log::error!("{}: {e}; using defaults", path.display());
            Config::default()
        }),
        Err(_) => Config::default(),
    };
    *CONFIG.lock().unwrap() = Some((path.to_owned(), config.clone()));
    // Write it back so that every option is there to edit.
    save(&config);
}

pub fn get() -> Config {
    CONFIG.lock().ok().and_then(|c| c.as_ref().map(|(_, c)| c.clone())).unwrap_or_default()
}

pub fn set(config: Config) {
    save(&config);
    if let Ok(mut c) = CONFIG.lock() {
        if let Some((_, current)) = c.as_mut() {
            *current = config;
        }
    }
}

fn save(config: &Config) {
    let Some(path) = CONFIG.lock().ok().and_then(|c| c.as_ref().map(|(p, _)| p.clone())) else {
        return;
    };
    match toml::to_string_pretty(config) {
        Ok(text) => {
            if let Err(e) = fs::write(&path, text) {
                log::error!("cannot write {}: {e}", path.display());
            }
        }
        Err(e) => log::error!("config: {e}"),
    }
}
