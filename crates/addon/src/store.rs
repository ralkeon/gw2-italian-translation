//! strings.db: every source text the game asked for, and its Italian translations.
//!
//! One background thread owns the connection: it does all the writes, in batches (requests come
//! in bursts of hundreds), and notices the rows the sync thread (or another program) wrote. Translations are also kept in memory, so the game gets an answer without
//! touching the disk.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gw2it_text::names::Names;
use rusqlite::{Connection, named_params, params};

use crate::{cache, config, markup, pipe, plural};

pub struct Seen {
    pub id: u32,
    pub hash: u64,
    pub text: String,
    /// Unix time in ms when it arrived: texts arriving together usually belong together.
    pub at_ms: i64,
}

enum Msg {
    Seen(Seen),
    /// The variant changed (set_full_mode): the texts seen here that differ are sent again.
    Resend,
}

static WORKER: Mutex<Option<(Sender<Msg>, JoinHandle<()>)>> = Mutex::new(None);

/// (id, hash of the English) -> Italian.
static TRANSLATIONS: Mutex<Option<HashMap<(u32, u64), String>>> = Mutex::new(None);

/// (id, hash of the English) -> the all-Italian variant's Italian, where it differs from the hybrid
/// one (the table `full`, from the global database).
static FULL: Mutex<Option<HashMap<(u32, u64), String>>> = Mutex::new(None);

/// The option "Traduzione completa" (config.full_italian): the game is answered from FULL first.
static FULL_MODE: AtomicBool = AtomicBool::new(false);

/// The names that stay in English (gw2it_text::names), from the global database: a text that is
/// one is not translated.
static NAMES: Mutex<Option<Names>> = Mutex::new(None);

pub(crate) const SCHEMA: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    CREATE TABLE IF NOT EXISTS strings (
        id         INTEGER NOT NULL,
        hash       INTEGER NOT NULL,  -- XXH3-64 of en, stored as i64
        en         TEXT    NOT NULL,
        first_seen INTEGER NOT NULL,  -- unix ms
        last_seen  INTEGER NOT NULL,
        seen       INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (id, hash)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS strings_first_seen ON strings (first_seen);
    CREATE INDEX IF NOT EXISTS strings_last_seen ON strings (last_seen);
    CREATE TABLE IF NOT EXISTS translations (
        id     INTEGER NOT NULL,
        hash   INTEGER NOT NULL,      -- hash of the English it translates: a new English is a new row
        it     TEXT    NOT NULL,
        status TEXT    NOT NULL,      -- 'auto' (machine), 'manual' (never overwritten)
        source TEXT    NOT NULL,      -- 'deepl', 'gemini', 'server:<translator>', 'editor:<contributor>', 'external', 'cloud:<author>', 'copy' (same English, other ID)
        at     INTEGER NOT NULL,      -- unix ms of the last change
        PRIMARY KEY (id, hash)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS translations_at ON translations (at);
    -- The server's proof of a translation it made (store::Translated), sent with it by sync.rs.
    CREATE TABLE IF NOT EXISTS proofs (
        id   INTEGER NOT NULL,
        hash INTEGER NOT NULL,
        sig  TEXT    NOT NULL,
        raw  TEXT    NOT NULL,
        PRIMARY KEY (id, hash)
    ) WITHOUT ROWID;
    -- Terms a hand correction fixed (gw2it_text::glossary): one row per wrong form of a word.
    CREATE TABLE IF NOT EXISTS glossary (
        en    TEXT    NOT NULL,       -- English word, lowercase
        wrong TEXT    NOT NULL,       -- what the translators wrote instead
        it    TEXT    NOT NULL,       -- what it must be
        at    INTEGER NOT NULL,
        local INTEGER NOT NULL,       -- 1 learned here (sync.rs sends it), 0 received from the global database
        PRIMARY KEY (en, wrong)
    ) WITHOUT ROWID;
    -- The English of the translations received from the global database (sync.rs), for the texts
    -- the game has not shown here (they are not in `strings`): the editor finds them too.
    CREATE TABLE IF NOT EXISTS cloud_strings (
        id   INTEGER NOT NULL,
        hash INTEGER NOT NULL,
        en   TEXT    NOT NULL,
        PRIMARY KEY (id, hash)
    ) WITHOUT ROWID;
    -- The names that stay in English, from the global database (sync.rs).
    CREATE TABLE IF NOT EXISTS english_names (
        name TEXT PRIMARY KEY
    ) WITHOUT ROWID;
    -- The all-Italian variant: the rows whose text differs from the hybrid one, from the global
    -- database (sync.rs); shown instead of `translations` when the option is on. An empty `it` is a
    -- row taken out there.
    CREATE TABLE IF NOT EXISTS full (
        id   INTEGER NOT NULL,
        hash INTEGER NOT NULL,
        it   TEXT    NOT NULL,
        at   INTEGER NOT NULL,
        PRIMARY KEY (id, hash)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS full_at ON full (at);
    -- A text changed by another program (it without at) is a hand correction: keep it, and give it
    -- a new `at` so that the addon picks it up and sends it to the game.
    CREATE TRIGGER IF NOT EXISTS translations_edited AFTER UPDATE OF it ON translations
    WHEN NEW.at = OLD.at AND NEW.it IS NOT OLD.it
    BEGIN
        UPDATE translations
        SET status = 'manual', source = 'external',
            at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
        WHERE id = NEW.id AND hash = NEW.hash;
    END;
";

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Opens the database. The translations are kept as they came from the project's database: never
/// changed here (no glossary, no repair), only the hand corrections of a contributor's editor.
pub fn start(path: &Path) -> rusqlite::Result<()> {
    let con = Connection::open(path)?;
    con.execute_batch(SCHEMA)?;
    load_names(&con)?;
    // Where it is already there, or a contributor asked for it ("save_map" in contributor.json): a
    // local tool, not something the addon hands out.
    let map = path.with_file_name("map_it.db");
    if map.exists() || config::save_map() {
        if let Err(e) = open_map(&con, &map) {
            log::error!("map_it.db: {e}");
        }
    }

    let map: HashMap<(u32, u64), String> = {
        let mut stmt = con.prepare("SELECT id, hash, it FROM translations")?;
        stmt.query_map([], |r| Ok(((r.get::<_, u32>(0)?, r.get::<_, i64>(1)? as u64), r.get::<_, String>(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    *TRANSLATIONS.lock().unwrap() = Some(map);
    let full: HashMap<(u32, u64), String> = {
        let mut stmt = con.prepare("SELECT id, hash, it FROM full WHERE it <> ''")?;
        stmt.query_map([], |r| Ok(((r.get::<_, u32>(0)?, r.get::<_, i64>(1)? as u64), r.get::<_, String>(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    *FULL.lock().unwrap() = Some(full);
    FULL_MODE.store(config::get().full_italian, Ordering::Relaxed);

    let watch = Watch::new(&con)?;

    let (tx, rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("gw2it-store".into())
        .spawn(move || run(con, watch, &rx))
        .expect("spawn store thread");
    *WORKER.lock().unwrap() = Some((tx, handle));
    Ok(())
}

/// Reads the names that stay in English again (sync.rs saves the new ones in the table).
pub fn load_names(con: &Connection) -> rusqlite::Result<()> {
    let mut stmt = con.prepare("SELECT name FROM english_names")?;
    let list: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    *NAMES.lock().unwrap() = Some(Names::new(list));
    Ok(())
}

/// map_it.db: the same texts in the format of the Local Translator module
/// (lt_server.KeyMap), kept next to strings.db so it can be sent to its author and merged with
/// its tools/unisci_contributi.py:
/// - texts: one row per string ID with its newest English, our translation (NULL if none) and
///   how = 'api-<addon version>' (seen in game, by the version that wrote the file);
/// - suggestions: our hand corrections, with the contributor as author (version 0: we have no
///   patch version).
/// Kept only if the file is already in the addon folder (create it empty to have it) or
/// contributor.json has `"save_map": true`, which creates it.
/// Rebuilt at start, then updated after every write (map_refresh). Rollback-journal mode, so it is
/// always a single complete file. Its own transactions: a problem with it never stops strings.db.
const MAP_SCHEMA: &str = "
    PRAGMA map.journal_mode = DELETE;
    PRAGMA map.synchronous = NORMAL;
    CREATE TABLE IF NOT EXISTS map.texts (
        key INTEGER PRIMARY KEY NOT NULL,
        en TEXT NOT NULL,
        it TEXT,
        how TEXT,          -- 'api-0.3.2' (seen while playing, this addon's version), 'contrib...' (merged from a contributor)
        seen INTEGER);
    CREATE TABLE IF NOT EXISTS map.suggestions (
        id         INTEGER PRIMARY KEY,   -- order of the saves: the last of a key is the one in use
        key        INTEGER NOT NULL,      -- string ID
        en_source  TEXT NOT NULL,         -- English text
        suggestion_source TEXT,           -- Italian text before the correction, NULL if unknown
        suggestion TEXT NOT NULL,
        author     TEXT NOT NULL,
        version    INTEGER NOT NULL,      -- patch version (Local Translator's): always 0 here
        src_hash   INTEGER                -- XXH3-64 of the English, as in Text Translator's cache.db
    );
    CREATE INDEX IF NOT EXISTS map.suggestions_key ON suggestions(key, id);
    -- What the checks (map_check) found: grave = 1 the text is left out (it is NULL in texts), 0 only
    -- a name that stays in English is missing. For whoever merges the file: nothing here is a text to use.
    CREATE TABLE IF NOT EXISTS map.problems (
        key   INTEGER PRIMARY KEY NOT NULL,
        en    TEXT NOT NULL,
        it    TEXT,
        why   TEXT NOT NULL,
        grave INTEGER NOT NULL);
";

/// Rows of map.texts / map.suggestions from strings + translations; `{where}` picks the strings.
/// how = 'api-<version>': where the texts come from (this addon, which reads them from the game) and
/// which version of it wrote them, for whoever merges the file.
const MAP_TEXTS: &str = concat!(
    "INSERT OR REPLACE INTO map.texts (key, en, it, how, seen)
    SELECT s.id, s.en, t.it, 'api-",
    env!("CARGO_PKG_VERSION"),
    "', s.last_seen / 1000
    FROM main.strings s LEFT JOIN main.translations t ON t.id = s.id AND t.hash = s.hash
    WHERE {where}"
);
const MAP_SUGGESTIONS: &str = "INSERT INTO map.suggestions
        (key, en_source, suggestion_source, suggestion, author, version, src_hash)
    SELECT t.id, s.en, NULL, t.it,
           CASE WHEN t.source LIKE 'editor:%' THEN substr(t.source, 8) ELSE :author END, 0, t.hash
    FROM main.translations t JOIN main.strings s ON s.id = t.id AND s.hash = t.hash
    WHERE t.status = 'manual' AND {where} ORDER BY t.at";

static MAP_ON: AtomicBool = AtomicBool::new(false);

/// Author of the hand corrections that do not name one ('editor', 'external').
fn map_author() -> String {
    config::contributor().unwrap_or_else(|| "italian_translation".into())
}

/// Why `it` cannot be handed over as the translation of `en` (the game would show it broken), if it
/// cannot: markup that is not the English's, a wrapper (`%str1%%str2%`, `<c>`) in the wrong place, a
/// `%` the game reads as a format, plural marks that are broken.
fn broken(en: &str, it: &str) -> Option<&'static str> {
    if !markup::token_mismatch(en, it).is_empty() {
        return Some("markup differs from the English");
    }
    if markup::wrap_broken(en, it) {
        return Some("tags in the wrong place");
    }
    if markup::stray_percent(it) {
        return Some("stray %");
    }
    if !plural::marks_ok(it) {
        return Some("plural marks broken");
    }
    None
}

/// What map_it.db hands over is clean: a text is repaired in the file where it can be (markup.rs
/// `repair`: tags in place, % signs, straight quotes), left out (`it` NULL, listed in `problems`)
/// where it cannot, and listed with its text kept where only a name that stays in English is
/// missing. The same for the hand corrections (`suggestions`). `only`: one ID, else all.
fn map_check(con: &Connection, only: Option<u32>) -> rusqlite::Result<usize> {
    let filter = if only.is_some() { " AND key = ?1" } else { "" };
    let key_param: Vec<i64> = only.map(|k| vec![i64::from(k)]).unwrap_or_default();
    let guard = NAMES.lock().ok();
    let names = guard.as_ref().and_then(|g| g.as_ref());
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = con.prepare(&format!("SELECT key, en, it FROM map.texts WHERE it IS NOT NULL{filter}"))?;
        stmt.query_map(rusqlite::params_from_iter(&key_param), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    con.execute(&format!("DELETE FROM map.problems WHERE 1 = 1{filter}"), rusqlite::params_from_iter(&key_param))?;
    let mut left_out = 0;
    for (key, en, it) in rows {
        let new = markup::repair(&en, &it);
        if new != it {
            con.execute("UPDATE map.texts SET it = ?2 WHERE key = ?1", params![key, new])?;
        }
        let (why, grave) = match broken(&en, &new) {
            Some(why) => (Some(why), true),
            None if names.is_some_and(|n| !n.missing(&en, &new).is_empty()) => (Some("a name that stays in English is missing"), false),
            None => (None, false),
        };
        if let Some(why) = why {
            if grave {
                con.execute("UPDATE map.texts SET it = NULL WHERE key = ?1", [key])?;
                left_out += 1;
            }
            con.execute("INSERT OR REPLACE INTO map.problems (key, en, it, why, grave) VALUES (?1, ?2, ?3, ?4, ?5)", params![key, en, it, why, grave])?;
        }
    }
    // The hand corrections: repaired, and dropped if they stay broken.
    let sugg: Vec<(i64, String, String)> = {
        let mut stmt = con.prepare(&format!("SELECT id, en_source, suggestion FROM map.suggestions WHERE 1 = 1{filter}"))?;
        stmt.query_map(rusqlite::params_from_iter(&key_param), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for (id, en, it) in sugg {
        let new = markup::repair(&en, &it);
        if broken(&en, &new).is_some() {
            con.execute("DELETE FROM map.suggestions WHERE id = ?1", [id])?;
            left_out += 1;
        } else if new != it {
            con.execute("UPDATE map.suggestions SET suggestion = ?2 WHERE id = ?1", params![id, new])?;
        }
    }
    Ok(left_out)
}

fn open_map(con: &Connection, path: &Path) -> rusqlite::Result<()> {
    con.execute("ATTACH ?1 AS map", [path.to_string_lossy()])?;
    con.execute_batch(MAP_SCHEMA)?;
    let newest = "s.last_seen = (SELECT max(last_seen) FROM main.strings x WHERE x.id = s.id)";
    con.execute_batch("BEGIN; DELETE FROM map.texts; DELETE FROM map.suggestions;")?;
    let filled = con
        .execute(&MAP_TEXTS.replace("{where}", newest), [])
        .and_then(|_| con.execute(&MAP_SUGGESTIONS.replace("{where}", "1"), named_params! {":author": map_author()}))
        .and_then(|_| map_check(con, None))
        .and_then(|left_out| {
            if left_out > 0 {
                log::warn!("map_it.db: {left_out} texts left out (broken markup): see the table problems");
            }
            con.execute_batch("COMMIT;")
        });
    if filled.is_err() {
        let _ = con.execute_batch("ROLLBACK;");
        return filled;
    }
    MAP_ON.store(true, Ordering::Relaxed);
    let n: i64 = con.query_row("SELECT count(*) FROM map.texts", [], |r| r.get(0))?;
    log::info!("map_it.db: {n} texts");
    Ok(())
}

/// Brings one text (id, hash of the English) up to date in map_it.db, if it is the newest
/// English of its ID.
fn map_refresh(con: &Connection, id: u32, hash: u64) -> rusqlite::Result<()> {
    if !MAP_ON.load(Ordering::Relaxed) {
        return Ok(());
    }
    let newest: Option<i64> = con
        .prepare_cached("SELECT hash FROM main.strings WHERE id = ?1 ORDER BY last_seen DESC LIMIT 1")?
        .query_row([id], |r| r.get(0))
        .ok();
    if newest != Some(hash as i64) {
        return Ok(());
    }
    let this = "s.id = :id AND s.hash = :hash";
    con.prepare_cached(&MAP_TEXTS.replace("{where}", this))?.execute(named_params! {":id": id, ":hash": hash as i64})?;
    con.prepare_cached("DELETE FROM map.suggestions WHERE key = ?1")?.execute([id])?;
    con.prepare_cached(&MAP_SUGGESTIONS.replace("{where}", this))?
        .execute(named_params! {":id": id, ":hash": hash as i64, ":author": map_author()})?;
    map_check(con, Some(id))?;
    Ok(())
}

/// map_refresh for many texts, in one transaction.
fn map_refresh_all(con: &mut Connection, keys: &[(u32, u64)]) {
    if keys.is_empty() || !MAP_ON.load(Ordering::Relaxed) {
        return;
    }
    let done = con.transaction().and_then(|tx| {
        for &(id, hash) in keys {
            map_refresh(&tx, id, hash)?;
        }
        tx.commit()
    });
    if let Err(e) = done {
        log::error!("map_it.db: {e}");
    }
}

/// The saved translation, if any.
pub fn lookup(id: u32, hash: u64) -> Option<String> {
    TRANSLATIONS.lock().ok()?.as_ref()?.get(&(id, hash)).cloned()
}

/// What the game shows for a text: with the option "Traduzione completa" the all-Italian variant's
/// row where there is one, else the hybrid translation.
pub fn shown(id: u32, hash: u64) -> Option<String> {
    if FULL_MODE.load(Ordering::Relaxed)
        && let Some(it) = FULL.lock().ok().and_then(|f| f.as_ref().and_then(|f| f.get(&(id, hash)).cloned()))
    {
        return Some(it);
    }
    lookup(id, hash)
}

/// Turns the all-Italian variant on or off: the texts the game has shown here that differ between
/// the two are sent to it again.
pub fn set_full_mode(on: bool) {
    if FULL_MODE.swap(on, Ordering::Relaxed) != on {
        send(Msg::Resend);
    }
}

/// The rows of the all-Italian variant kept here.
pub fn full_count() -> usize {
    FULL.lock().ok().and_then(|f| f.as_ref().map(HashMap::len)).unwrap_or(0)
}

/// Queues a text for saving; dropped silently if the store is not running.
pub fn record(seen: Seen) {
    send(Msg::Seen(seen));
}

fn remember(id: u32, hash: u64, it: &str) {
    if let Ok(mut map) = TRANSLATIONS.lock() {
        if let Some(map) = map.as_mut() {
            map.insert((id, hash), it.to_owned());
        }
    }
}

fn send(m: Msg) {
    if let Ok(worker) = WORKER.lock() {
        if let Some((tx, _)) = worker.as_ref() {
            let _ = tx.send(m);
        }
    }
}

pub fn stop() {
    let worker = WORKER.lock().ok().and_then(|mut w| w.take());
    if let Some((tx, handle)) = worker {
        drop(tx); // the thread drains what is left, commits and exits
        let _ = handle.join();
    }
    *TRANSLATIONS.lock().unwrap() = None;
}

fn run(mut con: Connection, mut watch: Watch, rx: &Receiver<Msg>) {
    loop {
        let first = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => {
                if let Err(e) = watch.check(&con) {
                    log::error!("strings.db: {e}");
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // Collect the rest of the burst, then commit once.
        let mut batch = vec![first];
        let mut last = false;
        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(m) => batch.push(m),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    last = true;
                    break;
                }
            }
            if batch.len() >= 5000 {
                break;
            }
        }
        if let Err(e) = save(&mut con, batch) {
            log::error!("strings.db: {e}");
        }
        if let Err(e) = watch.check(&con) {
            log::error!("strings.db: {e}");
        }
        if last {
            return;
        }
    }
}

/// Writes a batch.
fn save(con: &mut Connection, batch: Vec<Msg>) -> rusqlite::Result<()> {
    let tx = con.transaction()?;
    // Texts to bring up to date in map_it.db once committed.
    let mut touched = Vec::new();
    let mut resend: Vec<(u32, u64)> = Vec::new();
    {
        let mut seen_stmt = tx.prepare_cached(
            "INSERT INTO strings (id, hash, en, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (id, hash) DO UPDATE SET last_seen = excluded.last_seen, seen = seen + 1
             RETURNING seen",
        )?;
        for m in batch {
            match m {
                Msg::Seen(s) => {
                    let seen: i64 = seen_stmt.query_row(params![s.id, s.hash as i64, s.text, s.at_ms], |r| r.get(0))?;
                    if seen == 1 {
                        touched.push((s.id, s.hash));
                    }
                }
                Msg::Resend => {
                    // The texts seen here, and those Text Translator's cache holds (cache.rs).
                    let rows: Vec<(u32, i64, bool)> = tx
                        .prepare_cached(
                            "SELECT f.id, f.hash, EXISTS (SELECT 1 FROM strings s WHERE s.id = f.id AND s.hash = f.hash)
                             FROM full f WHERE f.it <> ''",
                        )?
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                        .collect::<rusqlite::Result<_>>()?;
                    resend.extend(rows.into_iter().filter(|(id, hash, seen)| *seen || cache::wanted(*id, *hash as u64)).map(|(id, hash, _)| (id, hash as u64)));
                }
            }
        }
    }
    tx.commit()?;
    for (id, hash) in resend {
        if let Some(it) = shown(id, hash) {
            pipe::send_text(id, hash, &it);
        }
    }
    map_refresh_all(con, &touched);
    Ok(())
}

/// Notices translations changed by other programs (a database editor, a script) and sends them
/// to the game. `PRAGMA data_version` only changes when another connection commits.
struct Watch {
    version: i64,
    /// Newest `at` already handled.
    last_at: i64,
    /// The same for the all-Italian variant's rows (sync.rs writes them).
    last_full_at: i64,
}

impl Watch {
    fn new(con: &Connection) -> rusqlite::Result<Self> {
        let version = con.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        let last_at = con.query_row("SELECT max(at) FROM translations", [], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0);
        let last_full_at = con.query_row("SELECT max(at) FROM full", [], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0);
        Ok(Self { version, last_at, last_full_at })
    }

    /// The all-Italian rows received since the last check: kept in memory (an empty one taken out)
    /// and, with the option on, sent to the game for the texts it has shown here.
    fn check_full(&mut self, con: &Connection) -> rusqlite::Result<()> {
        let mut stmt = con.prepare_cached(
            "SELECT f.id, f.hash, f.it, f.at, EXISTS (SELECT 1 FROM strings s WHERE s.id = f.id AND s.hash = f.hash)
             FROM full f WHERE f.at > ?1 ORDER BY f.at",
        )?;
        let rows: Vec<(u32, u64, String, i64, bool)> = stmt
            .query_map([self.last_full_at], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mode = FULL_MODE.load(Ordering::Relaxed);
        for (id, hash, it, at, seen) in rows {
            self.last_full_at = self.last_full_at.max(at);
            if let Ok(mut full) = FULL.lock()
                && let Some(full) = full.as_mut()
            {
                if it.is_empty() {
                    full.remove(&(id, hash));
                } else {
                    full.insert((id, hash), it);
                }
            }
            if mode
                && (seen || cache::wanted(id, hash))
                && let Some(text) = shown(id, hash)
            {
                pipe::send_text(id, hash, &text);
            }
        }
        Ok(())
    }

    fn check(&mut self, con: &Connection) -> rusqlite::Result<()> {
        let version: i64 = con.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        if version == self.version {
            return Ok(());
        }
        self.version = version;
        // `seen`: the game has shown this text here. Text Translator may hold an older translation
        // of it, so the new one is sent; the others are answered from memory when asked for.
        let mut stmt = con.prepare_cached(
            "SELECT t.id, t.hash, t.it, t.at, t.source,
                    EXISTS (SELECT 1 FROM strings s WHERE s.id = t.id AND s.hash = t.hash)
             FROM translations t WHERE t.at > ?1 ORDER BY t.at",
        )?;
        let rows = stmt.query_map([self.last_at], |r| {
            Ok((
                r.get::<_, u32>(0)?,
                r.get::<_, i64>(1)? as u64,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?;
        for row in rows {
            let (id, hash, it, at, source, seen) = row?;
            self.last_at = self.last_at.max(at);
            if lookup(id, hash).as_deref() != Some(it.as_str()) {
                // From the global database (sync.rs): thousands at the first sync, not logged.
                if !source.starts_with("cloud:") {
                    log::info!("translation {id} changed outside the addon: {it:?}");
                }
                if let Err(e) = map_refresh(con, id, hash) {
                    log::error!("map_it.db: {e}");
                }
                remember(id, hash, &it);
                // Also a text not seen here: Text Translator's cache holds it or lacks it (cache.rs).
                if seen || cache::wanted(id, hash) {
                    pipe::send_text(id, hash, &shown(id, hash).unwrap_or(it));
                }
            }
        }
        drop(stmt);
        self.check_full(con)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_follows_strings() {
        let dir = std::env::temp_dir().join(format!("gw2it-map-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut con = Connection::open(dir.join("strings.db")).unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO strings VALUES (1, 10, 'Old', 1000, 1000, 1), (1, 11, 'New', 2000, 2000, 1),
                                        (2, 20, 'Two', 1000, 1000, 1);
             INSERT INTO translations VALUES (1, 11, 'Nuovo', 'manual', 'editor:Tizio', 5),
                                             (1, 10, 'Vecchio', 'auto', 'deepl', 1);",
        )
        .unwrap();
        open_map(&con, &dir.join("map_it.db")).unwrap();
        let texts = |con: &Connection| -> Vec<(u32, String, Option<String>)> {
            let mut stmt = con.prepare("SELECT key, en, it FROM map.texts ORDER BY key").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
        };
        let suggestions = |con: &Connection| -> Vec<(u32, String, String)> {
            let mut stmt = con.prepare("SELECT key, suggestion, author FROM map.suggestions ORDER BY id").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(texts(&con), [(1, "New".into(), Some("Nuovo".into())), (2, "Two".into(), None)]);
        assert_eq!(suggestions(&con), [(1, "Nuovo".into(), "Tizio".into())]);

        // A new text, a machine translation, a forgotten correction, an older English.
        con.execute_batch(
            "INSERT INTO strings VALUES (3, 30, 'Three', 3000, 3000, 1);
             INSERT INTO translations VALUES (2, 20, 'Due', 'auto', 'deepl', 6);
             DELETE FROM translations WHERE id = 1 AND hash = 11;",
        )
        .unwrap();
        map_refresh_all(&mut con, &[(3, 30), (2, 20), (1, 11), (1, 10)]);
        assert_eq!(
            texts(&con),
            [(1, "New".into(), None), (2, "Two".into(), Some("Due".into())), (3, "Three".into(), None)]
        );
        assert!(suggestions(&con).is_empty());

        drop(con);
        assert!(!dir.join("map_it.db-wal").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn map_hands_over_clean_texts_only() {
        let dir = std::env::temp_dir().join(format!("gw2it-map-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let con = Connection::open(dir.join("strings.db")).unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO strings VALUES
               (1, 1, '%str1%%str2%Seer Pants%str3%%str4%', 1000, 1000, 1),
               (2, 2, 'Dawn''s Bow', 1000, 1000, 1),
               (3, 3, '<c=@abilitytype>Dual Attack</c> skills weaken enemies.', 1000, 1000, 1),
               (4, 4, '66%% Chance', 1000, 1000, 1),
               (5, 5, 'Mushroom[s]', 1000, 1000, 1);
             INSERT INTO translations VALUES
               (1, 1, 'Pantaloni%str3%%str4% %str1%%str2%del Veggente', 'auto', 'deepl', 1),
               (2, 2, 'Arco dell’Alba', 'auto', 'deepl', 1),
               (3, 3, 'Le abilità di \"Attacco</c> <c=@abilitytype>doppio\" indeboliscono.', 'auto', 'deepl', 1),
               (4, 4, '66%% % di probabilità', 'auto', 'deepl', 1),
               (5, 5, 'Fungo[pl:\"\"]', 'auto', 'deepl', 1);",
        )
        .unwrap();
        open_map(&con, &dir.join("map_it.db")).unwrap();
        let it = |key: i64| -> Option<String> { con.query_row("SELECT it FROM map.texts WHERE key = ?1", [key], |r| r.get(0)).unwrap() };
        // Repaired in the file: tags in place, straight quote, % sign.
        assert_eq!(it(1).as_deref(), Some("%str1%%str2%Pantaloni del Veggente%str3%%str4%"));
        assert_eq!(it(2).as_deref(), Some("Arco dell'Alba"));
        assert_eq!(it(4).as_deref(), Some("66%% di probabilità"));
        // Left out, with the reason: tags that cannot be put right, broken plural marks.
        assert_eq!(it(3), None);
        assert_eq!(it(5), None);
        let why: Vec<(i64, String, i64)> = con
            .prepare("SELECT key, why, grave FROM map.problems ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(why, [(3, "tags in the wrong place".into(), 1), (5, "plural marks broken".into(), 1)]);
        drop(con);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
