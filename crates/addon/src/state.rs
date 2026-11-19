//! State shared between the pipe thread, the sync thread and the options page.

use std::{
    sync::{
        Mutex,
        atomic::AtomicBool,
    },
    time::Instant,
};

pub struct Shared {
    pub shim_connected: AtomicBool,
    pub sync: Mutex<Sync>,
}

/// The global database, in this session.
pub struct Sync {
    pub status: String,
    /// Last sync that worked.
    pub at: Option<Instant>,
}

pub static SHARED: Shared = Shared {
    shim_connected: AtomicBool::new(false),
    sync: Mutex::new(Sync { status: String::new(), at: None }),
};
