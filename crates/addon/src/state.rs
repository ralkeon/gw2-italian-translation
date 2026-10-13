//! State shared between the pipe thread, the store thread, the translator and the UI.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64},
};

pub struct Shared {
    pub shim_connected: AtomicBool,
    /// Requests received since the addon loaded.
    pub received: AtomicU64,
    /// Distinct (id, hash) rows in strings.db.
    pub stored: AtomicU64,
    /// Saved translations.
    pub translated: AtomicU64,
    /// Texts waiting for the translator.
    pub queued: AtomicU64,
    /// Translations thrown away in this session because they broke the markup.
    pub rejected: AtomicU64,
    /// What the translator is doing.
    pub status: Mutex<String>,
    /// DeepL characters used and allowed this month.
    pub usage: Mutex<Option<(u64, u64)>>,
}

pub static SHARED: Shared = Shared {
    shim_connected: AtomicBool::new(false),
    received: AtomicU64::new(0),
    stored: AtomicU64::new(0),
    translated: AtomicU64::new(0),
    queued: AtomicU64::new(0),
    rejected: AtomicU64::new(0),
    status: Mutex::new(String::new()),
    usage: Mutex::new(None),
};
