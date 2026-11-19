//! State shared between the pipe thread, the store thread, the sync thread and the UI.

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
    pub sync: Mutex<Sync>,
}

/// The global database, in this session.
pub struct Sync {
    pub status: String,
    /// Last sync that worked.
    pub at: Option<Instant>,
    pub received: u64,
}

pub static SHARED: Shared = Shared {
    shim_connected: AtomicBool::new(false),
    received: AtomicU64::new(0),
    stored: AtomicU64::new(0),
    translated: AtomicU64::new(0),
    sync: Mutex::new(Sync { status: String::new(), at: None, received: 0 }),
};
