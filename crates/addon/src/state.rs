//! State shared between the pipe thread, the store thread, the translator and the UI.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64},
    },
    time::Instant,
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
    pub sync: Mutex<Sync>,
}

/// The global database, in this session.
pub struct Sync {
    pub status: String,
    /// Last sync that worked.
    pub at: Option<Instant>,
    pub sent: u64,
    pub received: u64,
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
    sync: Mutex::new(Sync { status: String::new(), at: None, sent: 0, received: 0 }),
};
