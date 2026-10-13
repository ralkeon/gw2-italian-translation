//! strings.db: every source text the game asked for, and its Italian translations.
//!
//! One background thread owns the connection: it does all the writes, in batches (requests come
//! in bursts of hundreds), answers the editor's searches, and notices edits made to the database
//! by other programs. Translations are also kept in memory, so the game gets an answer without
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

use rusqlite::{Connection, named_params, params};

use crate::{config, markup, pipe, plural, state::SHARED, translate::Job};

pub struct Seen {
    pub id: u32,
    pub hash: u64,
    pub text: String,
    /// Unix time in ms when it arrived: texts arriving together usually belong together.
    pub at_ms: i64,
}

pub struct Translated {
    pub id: u32,
    pub hash: u64,
    pub it: String,
    /// Who made it, e.g. "deepl".
    pub source: &'static str,
}

/// A text as the editor shows it.
#[derive(Clone)]
pub struct Row {
    pub id: u32,
    pub hash: u64,
    pub en: String,
    pub it: Option<String>,
    /// 'auto' or 'manual'; None without a translation.
    pub status: Option<String>,
}

enum Msg {
    Seen(Seen),
    Translated(Translated),
    Manual { id: u32, hash: u64, it: String, author: String },
    Forget { id: u32, hash: u64 },
    Search { query: String, reply: Sender<Vec<Row>> },
}

static WORKER: Mutex<Option<(Sender<Msg>, JoinHandle<()>)>> = Mutex::new(None);

/// (id, hash of the English) -> Italian.
static TRANSLATIONS: Mutex<Option<HashMap<(u32, u64), String>>> = Mutex::new(None);

/// Translations repaired at start-up: Text Translator still has the old ones in its cache, so they
/// are sent to the game when the shim connects.
static REPAIRED: Mutex<Vec<(u32, u64, String)>> = Mutex::new(Vec::new());

const SEARCH_MAX: usize = 200;

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
        source TEXT    NOT NULL,      -- 'deepl', 'editor:<contributor>', 'external', 'cloud:<author>'
        at     INTEGER NOT NULL,      -- unix ms of the last change
        PRIMARY KEY (id, hash)
    ) WITHOUT ROWID;
    CREATE INDEX IF NOT EXISTS translations_at ON translations (at);
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

/// Opens the database and returns the texts that still have no translation, oldest first
/// (Text Translator asks for a text once per session: the ones lost to an error or a reload would
/// otherwise stay English until the game restarts).
pub fn start(path: &Path) -> rusqlite::Result<Vec<Job>> {
    let con = Connection::open(path)?;
    con.execute_batch(SCHEMA)?;
    repair_auto(&con)?;
    if let Err(e) = open_map(&con, &path.with_file_name("map_it.db")) {
        log::error!("map_it.db: {e}");
    }
    let count: i64 = con.query_row("SELECT count(*) FROM strings", [], |r| r.get(0))?;
    SHARED.stored.store(count as u64, Ordering::Relaxed);

    let mut map = HashMap::new();
    {
        let mut stmt = con.prepare("SELECT id, hash, it FROM translations")?;
        let rows = stmt.query_map([], |r| Ok(((r.get::<_, u32>(0)?, r.get::<_, i64>(1)? as u64), r.get::<_, String>(2)?)))?;
        for row in rows {
            let (key, it) = row?;
            map.insert(key, it);
        }
    }
    SHARED.translated.store(map.len() as u64, Ordering::Relaxed);
    *TRANSLATIONS.lock().unwrap() = Some(map);

    let backlog = {
        let mut stmt = con.prepare(
            "SELECT s.id, s.hash, s.en FROM strings s
             LEFT JOIN translations t ON t.id = s.id AND t.hash = s.hash
             WHERE t.id IS NULL ORDER BY s.first_seen",
        )?;
        stmt.query_map([], |r| Ok(Job { id: r.get(0)?, hash: r.get::<_, i64>(1)? as u64, en: r.get(2)?, force: false }))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let watch = Watch::new(&con)?;

    let (tx, rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("gw2it-store".into())
        .spawn(move || run(con, watch, &rx))
        .expect("spawn store thread");
    *WORKER.lock().unwrap() = Some((tx, handle));
    Ok(backlog)
}

/// Machine translations saved by earlier versions are repaired from the English they translate:
/// plural marks (see plural.rs) and the spaces a translator dropped next to tokens. Hand
/// corrections are left as they are.
fn repair_auto(con: &Connection) -> rusqlite::Result<()> {
    let rows: Vec<(u32, i64, String, String)> = {
        let mut stmt = con.prepare(
            "SELECT t.id, t.hash, s.en, t.it FROM translations t
             JOIN strings s ON s.id = t.id AND s.hash = t.hash
             WHERE t.status = 'auto'",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut done = 0;
    let mut update = con.prepare("UPDATE translations SET it = ?3, at = ?4 WHERE id = ?1 AND hash = ?2")?;
    for (id, hash, en, it) in rows {
        let fixed = plural::pluralize(&en, &markup::fix_spacing(&en, &it));
        if fixed != it {
            update.execute(params![id, hash, fixed, now_ms()])?;
            REPAIRED.lock().unwrap().push((id, hash as u64, fixed));
            done += 1;
        }
    }
    if done > 0 {
        log::info!("{done} translations repaired (plurals, spacing)");
    }
    Ok(())
}

/// map_it.db: the same texts in the format of the Local Translator module (Nike's
/// lt_server.KeyMap), kept next to strings.db so it can be sent to him and merged with his
/// tools/unisci_contributi.py:
/// - texts: one row per string ID with its newest English, our translation (NULL if none) and
///   how = 'live' (seen in game);
/// - suggestions: our hand corrections, with the contributor as author (version 0: we have no
///   patch version).
/// Rebuilt at start, then updated after every write (map_refresh). Rollback-journal mode, so it is
/// always a single complete file. Its own transactions: a problem with it never stops strings.db.
const MAP_SCHEMA: &str = "
    PRAGMA map.journal_mode = DELETE;
    PRAGMA map.synchronous = NORMAL;
    CREATE TABLE IF NOT EXISTS map.texts (
        key INTEGER PRIMARY KEY NOT NULL,
        en TEXT NOT NULL,
        it TEXT,
        how TEXT,          -- 'live' (seen while playing), 'contrib...' (merged from a contributor)
        seen INTEGER);
    CREATE TABLE IF NOT EXISTS map.suggestions (
        id         INTEGER PRIMARY KEY,   -- order of the saves: the last of a key is the one in use
        key        INTEGER NOT NULL,      -- string ID
        en_source  TEXT NOT NULL,         -- English text
        suggestion_source TEXT,           -- Italian text before the correction, NULL if unknown
        suggestion TEXT NOT NULL,
        author     TEXT NOT NULL,
        version    INTEGER NOT NULL,      -- patch version (Nike's): always 0 here
        src_hash   INTEGER                -- XXH3-64 of the English, as in Text Translator's cache.db
    );
    CREATE INDEX IF NOT EXISTS map.suggestions_key ON suggestions(key, id);
";

/// Rows of map.texts / map.suggestions from strings + translations; `{where}` picks the strings.
const MAP_TEXTS: &str = "INSERT OR REPLACE INTO map.texts (key, en, it, how, seen)
    SELECT s.id, s.en, t.it, 'live', s.last_seen / 1000
    FROM main.strings s LEFT JOIN main.translations t ON t.id = s.id AND t.hash = s.hash
    WHERE {where}";
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

fn open_map(con: &Connection, path: &Path) -> rusqlite::Result<()> {
    con.execute("ATTACH ?1 AS map", [path.to_string_lossy()])?;
    con.execute_batch(MAP_SCHEMA)?;
    let newest = "s.last_seen = (SELECT max(last_seen) FROM main.strings x WHERE x.id = s.id)";
    con.execute_batch("BEGIN; DELETE FROM map.texts; DELETE FROM map.suggestions;")?;
    let filled = con
        .execute(&MAP_TEXTS.replace("{where}", newest), [])
        .and_then(|_| con.execute(&MAP_SUGGESTIONS.replace("{where}", "1"), named_params! {":author": map_author()}))
        .and_then(|_| con.execute_batch("COMMIT;"));
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

/// The translations repaired at start-up, once: to be sent to the game.
pub fn take_repaired() -> Vec<(u32, u64, String)> {
    std::mem::take(&mut *REPAIRED.lock().unwrap())
}

/// The saved translation, if any.
pub fn lookup(id: u32, hash: u64) -> Option<String> {
    TRANSLATIONS.lock().ok()?.as_ref()?.get(&(id, hash)).cloned()
}

/// Queues a text for saving; dropped silently if the store is not running.
pub fn record(seen: Seen) {
    send(Msg::Seen(seen));
}

/// Keeps a new machine translation (in memory now, on disk with the next batch).
pub fn save_translation(t: Translated) {
    remember(t.id, t.hash, &t.it);
    send(Msg::Translated(t));
}

/// Keeps a hand correction and shows it in the game right away. Never overwritten by a machine.
pub fn save_manual(id: u32, hash: u64, it: String, author: &str) {
    remember(id, hash, &it);
    pipe::send_text(id, hash, &it);
    send(Msg::Manual { id, hash, it, author: author.to_owned() });
}

/// Drops a translation (of any kind), to have it made again.
pub fn forget(id: u32, hash: u64) {
    if let Ok(mut map) = TRANSLATIONS.lock() {
        if map.as_mut().and_then(|m| m.remove(&(id, hash))).is_some() {
            SHARED.translated.fetch_sub(1, Ordering::Relaxed);
        }
    }
    send(Msg::Forget { id, hash });
}

/// Texts whose English or Italian contains `query`, or whose ID is `query`; the most recently
/// seen first. Empty query: the most recently seen texts. The answer comes on the receiver.
pub fn search(query: String) -> Receiver<Vec<Row>> {
    let (reply, rx) = mpsc::channel();
    send(Msg::Search { query, reply });
    rx
}

fn remember(id: u32, hash: u64, it: &str) {
    if let Ok(mut map) = TRANSLATIONS.lock() {
        if let Some(map) = map.as_mut() {
            if map.insert((id, hash), it.to_owned()).is_none() {
                SHARED.translated.fetch_add(1, Ordering::Relaxed);
            }
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
        let searches = match save(&mut con, batch) {
            Ok(searches) => searches,
            Err(e) => {
                log::error!("strings.db: {e}");
                Vec::new()
            }
        };
        for (query, reply) in searches {
            match find(&con, &query) {
                Ok(rows) => {
                    let _ = reply.send(rows);
                }
                Err(e) => log::error!("search {query:?}: {e}"),
            }
        }
        if let Err(e) = watch.check(&con) {
            log::error!("strings.db: {e}");
        }
        if last {
            return;
        }
    }
}

/// Writes a batch; returns the searches in it, to run once it is committed.
fn save(con: &mut Connection, batch: Vec<Msg>) -> rusqlite::Result<Vec<(String, Sender<Vec<Row>>)>> {
    let tx = con.transaction()?;
    let mut added = 0u64;
    let mut searches = Vec::new();
    // Texts to bring up to date in map_it.db once committed.
    let mut touched = Vec::new();
    {
        let mut seen_stmt = tx.prepare_cached(
            "INSERT INTO strings (id, hash, en, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (id, hash) DO UPDATE SET last_seen = excluded.last_seen, seen = seen + 1
             RETURNING seen",
        )?;
        let mut auto_stmt = tx.prepare_cached(
            "INSERT INTO translations (id, hash, it, status, source, at) VALUES (?1, ?2, ?3, 'auto', ?4, ?5)
             ON CONFLICT (id, hash) DO UPDATE SET it = excluded.it, status = excluded.status,
                 source = excluded.source, at = excluded.at
             WHERE translations.status <> 'manual'",
        )?;
        let mut manual_stmt = tx.prepare_cached(
            "INSERT INTO translations (id, hash, it, status, source, at) VALUES (?1, ?2, ?3, 'manual', ?4, ?5)
             ON CONFLICT (id, hash) DO UPDATE SET it = excluded.it, status = excluded.status,
                 source = excluded.source, at = excluded.at",
        )?;
        let mut forget_stmt = tx.prepare_cached("DELETE FROM translations WHERE id = ?1 AND hash = ?2")?;
        for m in batch {
            match m {
                Msg::Seen(s) => {
                    let seen: i64 = seen_stmt.query_row(params![s.id, s.hash as i64, s.text, s.at_ms], |r| r.get(0))?;
                    if seen == 1 {
                        added += 1;
                        touched.push((s.id, s.hash));
                    }
                }
                Msg::Translated(t) => {
                    auto_stmt.execute(params![t.id, t.hash as i64, t.it, t.source, now_ms()])?;
                    touched.push((t.id, t.hash));
                }
                Msg::Manual { id, hash, it, author } => {
                    manual_stmt.execute(params![id, hash as i64, it, format!("editor:{author}"), now_ms()])?;
                    touched.push((id, hash));
                }
                Msg::Forget { id, hash } => {
                    forget_stmt.execute(params![id, hash as i64])?;
                    touched.push((id, hash));
                }
                Msg::Search { query, reply } => searches.push((query, reply)),
            }
        }
    }
    tx.commit()?;
    SHARED.stored.fetch_add(added, Ordering::Relaxed);
    map_refresh_all(con, &touched);
    Ok(searches)
}

fn find(con: &Connection, query: &str) -> rusqlite::Result<Vec<Row>> {
    let query = query.trim();
    let id: Option<u32> = query.parse().ok();
    let pattern = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = con.prepare_cached(
        "SELECT s.id, s.hash, s.en, t.it, t.status FROM strings s
         LEFT JOIN translations t ON t.id = s.id AND t.hash = s.hash
         WHERE ?1 = '' OR s.id = ?2 OR s.en LIKE ?3 ESCAPE '\\' OR t.it LIKE ?3 ESCAPE '\\'
         ORDER BY s.last_seen DESC LIMIT ?4",
    )?;
    stmt.query_map(params![query, id, pattern, SEARCH_MAX as i64], |r| {
        Ok(Row { id: r.get(0)?, hash: r.get::<_, i64>(1)? as u64, en: r.get(2)?, it: r.get(3)?, status: r.get(4)? })
    })?
    .collect()
}

/// Notices translations changed by other programs (a database editor, a script) and sends them
/// to the game. `PRAGMA data_version` only changes when another connection commits.
struct Watch {
    version: i64,
    /// Newest `at` already handled.
    last_at: i64,
}

impl Watch {
    fn new(con: &Connection) -> rusqlite::Result<Self> {
        let version = con.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        let last_at = con.query_row("SELECT max(at) FROM translations", [], |r| r.get::<_, Option<i64>>(0))?.unwrap_or(0);
        Ok(Self { version, last_at })
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
                if seen {
                    pipe::send_text(id, hash, &it);
                }
            }
        }
        Ok(())
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
}
