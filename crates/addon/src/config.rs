//! config.toml in the addon folder. Edited from the addon's options in Nexus or by hand (read at
//! load).

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
    pub azure: Azure,
    pub gemini: Gemini,
    /// Other language models, with OpenAI's API: after Azure, in this order.
    pub llm: Vec<Llm>,
    pub sync: Sync,
    pub install: Install,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Deepl {
    /// DeepL API key (https://www.deepl.com/your-account/keys). Empty: the project's server
    /// translates, with its own key.
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

/// Microsoft Azure AI Translator: after Gemini.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Azure {
    pub api_key: String,
    /// The resource's region, e.g. westeurope; empty for a global resource.
    pub region: String,
}

/// A service with OpenAI's API: Groq, OpenRouter, Mistral, Cerebras...
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Llm {
    pub name: String,
    /// API base, e.g. https://api.groq.com/openai/v1.
    pub url: String,
    pub api_key: String,
    pub model: String,
}

/// Gemini: after DeepL.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Gemini {
    /// Google AI Studio key (https://aistudio.google.com/apikey).
    pub api_key: String,
    /// Models in order, separated by commas: the next one when one is overloaded.
    pub model: String,
}

/// Fast ones: a fallback must not keep the game in English for long.
pub const GEMINI_MODELS: &str = "gemini-3.1-flash-lite,gemini-3.6-flash";

impl Default for Gemini {
    fn default() -> Self {
        Self { api_key: String::new(), model: GEMINI_MODELS.into() }
    }
}

/// Global translations database (server/ in the repository).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sync {
    pub enabled: bool,
    /// Empty: the project's server.
    pub url: String,
    /// This installation's key, given by the server the first time.
    pub token: String,
    /// Used when the main server does not answer. Empty: the project's spare; "off": none.
    pub fallback_url: String,
    /// This installation's key on the spare.
    pub fallback_token: String,
}

impl Default for Sync {
    fn default() -> Self {
        Self {
            enabled: true,
            url: String::new(),
            token: String::new(),
            fallback_url: String::new(),
            fallback_token: String::new(),
        }
    }
}

impl Sync {
    pub fn server(&self) -> &str {
        let url = self.url.trim().trim_end_matches('/');
        if url.is_empty() { SYNC_URL } else { url }
    }

    pub fn fallback(&self) -> Option<&str> {
        let url = self.fallback_url.trim().trim_end_matches('/');
        let url = if url.is_empty() { SYNC_FALLBACK_URL } else { url };
        (url != "off" && !url.is_empty() && url != self.server()).then_some(url)
    }
}

/// What the DLL set up by itself (install.rs).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Install {
    /// The Text Translator module connected at least once: from then on, which module is active
    /// is the player's choice.
    pub module_worked: bool,
}

/// The project's server: crates/server on ifalna.
const SYNC_URL: &str = "https://ifalna.ralkeon.com/gw2it";
/// Its spare, the same API on Cloudflare (server/), copied from it every night.
const SYNC_FALLBACK_URL: &str = "https://gw2it-sync.gw2it-sync.workers.dev";

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
