//! config.toml in the addon folder. Edited from the addon's options in Nexus or by hand (read at
//! load).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // The translators' keys of earlier versions ([deepl], [gemini], [azure], [[llm]]) are not read
    // any more, and are left out when the file is written back.
    pub sync: Sync,
    pub install: Install,
    /// "Tutto in italiano": the names the hybrid translation keeps in English (skills, traits,
    /// places, boons...) shown in Italian too, where the global database has the all-Italian row.
    pub full_italian: bool,
}

/// The project's translations database (crates/server on ifalna).
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sync {
    pub enabled: bool,
    /// Empty: the project's server.
    pub url: String,
    /// This installation's key, given by the server the first time.
    pub token: String,
    // fallback_url and fallback_token (the spare on Cloudflare, removed 2026-11-19) are not read
    // any more, and are left out when the file is written back.
}

impl Default for Sync {
    fn default() -> Self {
        Self {
            enabled: true,
            url: String::new(),
            token: String::new(),
        }
    }
}

impl Sync {
    pub fn server(&self) -> &str {
        let url = self.url.trim().trim_end_matches('/');
        if url.is_empty() { SYNC_URL } else { url }
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

static CONFIG: Mutex<Option<(PathBuf, Config)>> = Mutex::new(None);

/// contributor.json in the addon folder (the project's own installations): `user` is the name given
/// to the server at registration and the author of map_it.db's rows; `"save_map": true` keeps
/// map_it.db up to date (store.rs), without having to create it. There is no editor any more.
#[derive(Deserialize)]
struct Contributor {
    user: String,
    #[serde(default)]
    save_map: bool,
}

static CONTRIBUTOR: Mutex<Option<String>> = Mutex::new(None);
static SAVE_MAP: AtomicBool = AtomicBool::new(false);

/// Reads contributor.json (at load), if there.
pub fn load_contributor(path: &Path) {
    let mut save_map = false;
    let user = match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Contributor>(&text) {
            Ok(c) if !c.user.trim().is_empty() => {
                save_map = c.save_map;
                Some(c.user.trim().to_owned())
            }
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
        log::info!("contributor {user}{}", if save_map { ": map_it.db on" } else { "" });
    }
    SAVE_MAP.store(save_map, Ordering::Relaxed);
    *CONTRIBUTOR.lock().unwrap() = user;
}

/// True if contributor.json asks for map_it.db (`"save_map": true`).
pub fn save_map() -> bool {
    SAVE_MAP.load(Ordering::Relaxed)
}

/// The contributor's name, if contributor.json gives one.
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
