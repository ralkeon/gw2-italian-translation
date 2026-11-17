//! Text Translator's cache of our module (text_translator/modules/italian_translation/cache.db).
//!
//! Text Translator shows the cache's row of a text the moment the game shows the text, and keeps it
//! until the module sends another; a text it has no row for is asked for, and shown in English until
//! the answer arrives (with Pause Refreshes on, until the text is shown again). So the cache must hold
//! our best translation of every text we have, not only of those seen on this PC, and follow every
//! improvement the global database brings:
//! - at each connection of the shim, the cache is read (read only: Text Translator writes it) and
//!   every text it lacks, or holds with another Italian, is sent to it (pipe::send_text): Text
//!   Translator shows it at once if it is on screen and saves it in the cache;
//! - after that, store.rs's Watch sends each changed translation of a text the cache holds or lacks
//!   (`wanted`), not only of the texts seen here.
//! A row of the cache carries the hash of the English it translates (src_hash): it is replaced only
//! by our translation of that same English; a text it lacks gets ours of the newest English we have
//! (the game asks again if its English is another).

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags};

use crate::{install, pipe, store};

/// What the cache holds: id -> (hash of the English, digest of the Italian). None until the first
/// reading: then nothing is sent beyond what the game asks for and what it has seen.
static HELD: Mutex<Option<HashMap<u32, (u64, u64)>>> = Mutex::new(None);
static PATHS: Mutex<Option<(PathBuf, PathBuf)>> = Mutex::new(None);
/// Shim connections so far: each one gets a full pass.
static CONNECTIONS: AtomicU64 = AtomicU64::new(0);
static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);
/// Texts sent in a row before a short pause, so the game's frame never waits on us.
const BURST: usize = 200;

pub fn digest(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// `tt_dir`: Text Translator's folder; `strings`: our strings.db.
pub fn start(tt_dir: &Path, strings: &Path) {
    *PATHS.lock().unwrap() = Some((install::cache_path(tt_dir), strings.to_path_buf()));
    STOP.store(false, Ordering::SeqCst);
    let handle = thread::Builder::new().name("gw2it-cache".into()).spawn(|| {
        let mut done = 0;
        while !STOP.load(Ordering::SeqCst) {
            let now = CONNECTIONS.load(Ordering::SeqCst);
            if now != done {
                done = now;
                // Let the first answers to the game go out before the pass.
                thread::sleep(Duration::from_secs(5));
                if let Err(e) = pass() {
                    log::warn!("Text Translator's cache: {e}");
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
    match handle {
        Ok(h) => *WORKER.lock().unwrap() = Some(h),
        Err(e) => log::error!("cache thread: {e}"),
    }
}

pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(h) = WORKER.lock().unwrap().take() {
        let _ = h.join();
    }
}

/// The shim connected (pipe.rs).
pub fn connected() {
    CONNECTIONS.fetch_add(1, Ordering::SeqCst);
}

/// A translation went to Text Translator, which keeps it in the cache.
pub fn note(id: u32, hash: u64, text: &str) {
    if let Ok(mut held) = HELD.lock()
        && let Some(held) = held.as_mut()
    {
        held.insert(id, (hash, digest(text)));
    }
}

/// Whether a changed translation of (id, hash) should go to Text Translator although the game has
/// not shown the text here: the cache holds that text for that English, or does not hold it at all.
pub fn wanted(id: u32, hash: u64) -> bool {
    match HELD.lock().ok().as_ref().and_then(|h| h.as_ref()) {
        Some(held) => held.get(&id).is_none_or(|(h, _)| *h == hash),
        None => false,
    }
}

fn pass() -> Result<(), String> {
    let Some((cache, strings)) = PATHS.lock().unwrap().clone() else { return Ok(()) };
    if !cache.exists() {
        return Ok(());
    }
    let ro = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let key = install::cache_key();
    let mut held: HashMap<u32, (u64, u64)> = HashMap::new();
    {
        let con = Connection::open_with_flags(&cache, ro).map_err(|e| format!("{}: {e}", cache.display()))?;
        con.busy_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let mut stmt = con.prepare("SELECT id, src_hash, text FROM translations WHERE cache_key = ?1").map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&key], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, String>(2)?)))
            .map_err(|e| e.to_string())?;
        for (id, hash, text) in rows.flatten() {
            held.insert(id, (hash, digest(&text)));
        }
    }
    // Our newest English of each text (the last row by time wins).
    let mut newest: HashMap<u32, u64> = HashMap::new();
    {
        let con = Connection::open_with_flags(&strings, ro).map_err(|e| format!("{}: {e}", strings.display()))?;
        con.busy_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?;
        let mut stmt = con.prepare("SELECT id, hash FROM translations ORDER BY at").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, i64>(1)? as u64))).map_err(|e| e.to_string())?;
        for (id, hash) in rows.flatten() {
            newest.insert(id, hash);
        }
    }
    let total = held.len();
    *HELD.lock().unwrap() = Some(held.clone());

    let (mut added, mut changed, mut burst) = (0usize, 0usize, 0usize);
    for (id, hash) in newest {
        if STOP.load(Ordering::SeqCst) {
            return Ok(());
        }
        let hash = match held.get(&id) {
            // The cache's English: replaced only with ours of the same English.
            Some((h, d)) => match store::shown(id, *h) {
                Some(it) if digest(&it) != *d => *h,
                _ => continue,
            },
            None => hash,
        };
        let Some(it) = store::shown(id, hash) else { continue };
        if !pipe::send_text(id, hash, &it) {
            log::info!("Text Translator's cache: the shim went away, {added} added and {changed} updated so far");
            return Ok(());
        }
        if held.contains_key(&id) {
            changed += 1;
        } else {
            added += 1;
        }
        burst += 1;
        if burst >= BURST {
            burst = 0;
            thread::sleep(Duration::from_millis(100));
        }
    }
    log::info!("Text Translator's cache: {total} rows held, {added} texts added, {changed} updated to our best translation");
    Ok(())
}
