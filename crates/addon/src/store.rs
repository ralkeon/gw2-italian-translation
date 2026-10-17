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

use gw2it_text::{
    glossary::{self, Learned, Term},
    names::Names,
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
    /// Who made it: "deepl", "gemini", "server:deepl"...
    pub source: String,
    /// Made by the project's server: its signature and the XML it answered, which the global
    /// database checks to tell the server's translations from the ones an addon makes up.
    pub proof: Option<(String, String)>,
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
    /// A term of the glossary, new or changed: saved whole, and the machine translations that
    /// have it wrong are repaired.
    Term(Term, Origin),
    Search { query: String, exact: bool, reply: Sender<Vec<Row>> },
}

static WORKER: Mutex<Option<(Sender<Msg>, JoinHandle<()>)>> = Mutex::new(None);

/// (id, hash of the English) -> Italian.
static TRANSLATIONS: Mutex<Option<HashMap<(u32, u64), String>>> = Mutex::new(None);

/// Hash of the English -> Italian, and whether it is a hand correction: the same English under
/// another ID (ten "Torch" items) takes the translation it already has instead of asking for a new
/// one. The general one: a hand correction if there is one, the text most of them have (a hand
/// correction goes to all the machine translations: one that differs later is for its own context),
/// else the newest machine translation; as server/sql/same.sql chooses.
static SAME: Mutex<Option<HashMap<u64, (String, bool)>>> = Mutex::new(None);

/// Every translation, the general one of each English (SAME) after the others.
const LOAD: &str = "SELECT id, hash, it, status = 'manual' AS manual FROM translations
    ORDER BY manual, CASE WHEN manual THEN count(*) OVER (PARTITION BY hash, it, status) END, at";

/// Translations repaired at start-up: Text Translator still has the old ones in its cache, so they
/// are sent to the game when the shim connects.
static REPAIRED: Mutex<Vec<(u32, u64, String)>> = Mutex::new(Vec::new());

/// What the corrections taught (gw2it_text::glossary), also in the glossary table.
static GLOSSARY: Mutex<Vec<Term>> = Mutex::new(Vec::new());

/// The names that stay in English (gw2it_text::names), from the global database: a text that is
/// one is not translated.
static NAMES: Mutex<Option<Names>> = Mutex::new(None);

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
    *GLOSSARY.lock().unwrap() = load_glossary(&con)?;
    load_names(&con)?;
    repair_auto(&con)?;
    // Where it is already there, or a contributor asked for it ("save_map" in contributor.json): a
    // local tool, not something the addon hands out.
    let map = path.with_file_name("map_it.db");
    if map.exists() || config::save_map() {
        if let Err(e) = open_map(&con, &map) {
            log::error!("map_it.db: {e}");
        }
    }
    let count: i64 = con.query_row("SELECT count(*) FROM strings", [], |r| r.get(0))?;
    SHARED.stored.store(count as u64, Ordering::Relaxed);

    let (mut map, mut same) = (HashMap::new(), HashMap::new());
    {
        let mut stmt = con.prepare(LOAD)?;
        let rows = stmt.query_map([], |r| {
            Ok(((r.get::<_, u32>(0)?, r.get::<_, i64>(1)? as u64), r.get::<_, String>(2)?, r.get::<_, bool>(3)?))
        })?;
        for row in rows {
            let (key, it, manual) = row?;
            same.insert(key.1, (it.clone(), manual));
            map.insert(key, it);
        }
    }
    SHARED.translated.store(map.len() as u64, Ordering::Relaxed);
    *TRANSLATIONS.lock().unwrap() = Some(map);
    *SAME.lock().unwrap() = Some(same);

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

fn load_glossary(con: &Connection) -> rusqlite::Result<Vec<Term>> {
    let mut stmt = con.prepare("SELECT en, wrong, it FROM glossary ORDER BY en, at")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
    let mut terms: Vec<Term> = Vec::new();
    for row in rows {
        let (en, wrong, it) = row?;
        match terms.last_mut().filter(|t| t.en == en) {
            Some(term) => term.wrong.push(wrong),
            None => terms.push(Term { en, it, wrong: vec![wrong] }),
        }
    }
    Ok(terms)
}

/// Reads the names that stay in English again (sync.rs saves the new ones in the table).
pub fn load_names(con: &Connection) -> rusqlite::Result<()> {
    let mut stmt = con.prepare("SELECT name FROM english_names")?;
    let list: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    *NAMES.lock().unwrap() = Some(Names::new(list));
    Ok(())
}

/// The name that stays in English the whole of `en` is, if it is one.
pub fn english_name(en: &str) -> Option<String> {
    NAMES.lock().ok()?.as_ref()?.whole(en).map(str::to_owned)
}

/// The glossary as it is now.
pub fn glossary() -> Vec<Term> {
    GLOSSARY.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Adds what a correction taught to the glossary (in memory now, on disk and in the texts already
/// translated with the next batch). An English word keeps one Italian: a later correction of the
/// same word replaces it, and the earlier one becomes a wrong form.
fn learn(l: Learned) {
    let said = Term { en: l.en, it: l.it, wrong: vec![l.wrong] };
    let Ok(mut terms) = GLOSSARY.lock() else {
        return;
    };
    // Here the contributor decides at once; the global database says the last word (sync.rs).
    let i = terms.iter().position(|t| t.en == said.en);
    let Some(term) = glossary::merge(i.map(|i| (&terms[i], 0)), &said, 0) else {
        return;
    };
    match i {
        Some(i) => terms[i] = term.clone(),
        None => terms.push(term.clone()),
    }
    drop(terms);
    send(Msg::Term(term, Origin::Local));
}

/// Where a term of the glossary comes from.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Learned from a correction made here: sync.rs sends it to the global database.
    Local,
    /// Received from it.
    Cloud,
}

/// The terms of the global glossary that are new or changed here: kept, and used at once (the
/// machine translations that have them wrong are repaired). Not sent back.
pub fn receive_terms(received: Vec<Term>) {
    let Ok(mut terms) = GLOSSARY.lock() else {
        return;
    };
    for term in received {
        match terms.iter().position(|t| t.en == term.en) {
            Some(i) if terms[i] == term => continue,
            Some(i) => terms[i] = term.clone(),
            None => terms.push(term.clone()),
        }
        send(Msg::Term(term, Origin::Cloud));
    }
}

/// Machine translations saved by earlier versions are repaired from the English they translate:
/// plural marks (see plural.rs), the spaces a translator dropped next to tokens and the glossary.
/// Hand corrections are left as they are.
fn repair_auto(con: &Connection) -> rusqlite::Result<()> {
    let terms = glossary();
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
        let fixed = plural::pluralize(&en, &glossary::fix(&en, &markup::repair(&en, &markup::fix_spacing(&en, &it)), &terms));
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
        version    INTEGER NOT NULL,      -- patch version (Nike's): always 0 here
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

/// The translations repaired at start-up, once: to be sent to the game.
pub fn take_repaired() -> Vec<(u32, u64, String)> {
    std::mem::take(&mut *REPAIRED.lock().unwrap())
}

/// The saved translation, if any.
pub fn lookup(id: u32, hash: u64) -> Option<String> {
    TRANSLATIONS.lock().ok()?.as_ref()?.get(&(id, hash)).cloned()
}

/// The translation of the same English under any ID (`hash` is the hash of the English).
pub fn lookup_same(hash: u64) -> Option<String> {
    Some(SAME.lock().ok()?.as_ref()?.get(&hash)?.0.clone())
}

/// Queues a text for saving; dropped silently if the store is not running.
pub fn record(seen: Seen) {
    send(Msg::Seen(seen));
}

/// Keeps a new machine translation (in memory now, on disk with the next batch).
pub fn save_translation(t: Translated) {
    remember(t.id, t.hash, &t.it, false);
    send(Msg::Translated(t));
}

/// Keeps a hand correction and shows it in the game right away. Never overwritten by a machine.
/// If it fixed a name (the one that was there had it wrong), the glossary learns it: `en` is the
/// English text.
pub fn save_manual(id: u32, hash: u64, en: &str, it: String, author: &str) {
    if let Some(old) = lookup(id, hash) {
        if let Some(learned) = glossary::learn(en, &old, &it) {
            log::info!("glossary: {} = {} (was {})", learned.en, learned.it, learned.wrong);
            learn(learned);
        }
    }
    remember(id, hash, &it, true);
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
/// seen first, then the ones of the global database the game has not shown here. With `exact`, the whole English or Italian text is `query` (capitals too), not just
/// a part of it. Empty query: the most recently seen texts. The answer comes on the receiver.
pub fn search(query: String, exact: bool) -> Receiver<Vec<Row>> {
    let (reply, rx) = mpsc::channel();
    send(Msg::Search { query, exact, reply });
    rx
}

/// `manual`: a hand correction. It becomes the general translation of its English (SAME) if there
/// is none by hand yet, or if it corrects that one.
fn remember(id: u32, hash: u64, it: &str, manual: bool) {
    let mut old = None;
    if let Ok(mut map) = TRANSLATIONS.lock() {
        if let Some(map) = map.as_mut() {
            old = map.insert((id, hash), it.to_owned());
            if old.is_none() {
                SHARED.translated.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    if let Ok(mut same) = SAME.lock() {
        if let Some(same) = same.as_mut() {
            let general = same.get(&hash);
            if general.is_none_or(|(text, by_hand)| !by_hand || old.as_ref() == Some(text)) {
                same.insert(hash, (it.to_owned(), manual));
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
    *SAME.lock().unwrap() = None;
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
        for (query, exact, reply) in searches {
            match find(&con, &query, exact) {
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
fn save(con: &mut Connection, batch: Vec<Msg>) -> rusqlite::Result<Vec<(String, bool, Sender<Vec<Row>>)>> {
    let tx = con.transaction()?;
    let mut added = 0u64;
    let mut searches = Vec::new();
    // Texts to bring up to date in map_it.db once committed.
    let mut touched = Vec::new();
    let mut repaired = Vec::new();
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
        // The same English under other IDs: their machine translations take the hand correction
        // (the global database does the same for everyone: server/sql/same.sql).
        let mut same_stmt = tx.prepare_cached(
            "UPDATE translations SET it = ?3, status = 'manual', source = ?4, at = ?5
             WHERE hash = ?2 AND id <> ?1 AND status = 'auto' AND it <> ?3 RETURNING id",
        )?;
        let mut forget_stmt = tx.prepare_cached("DELETE FROM translations WHERE id = ?1 AND hash = ?2")?;
        let mut proof_stmt = tx.prepare_cached("INSERT OR REPLACE INTO proofs (id, hash, sig, raw) VALUES (?1, ?2, ?3, ?4)")?;
        let mut forget_proof_stmt = tx.prepare_cached("DELETE FROM proofs WHERE id = ?1 AND hash = ?2")?;
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
                    match &t.proof {
                        Some((sig, raw)) => proof_stmt.execute(params![t.id, t.hash as i64, sig, raw])?,
                        None => forget_proof_stmt.execute(params![t.id, t.hash as i64])?,
                    };
                    touched.push((t.id, t.hash));
                }
                Msg::Manual { id, hash, it, author } => {
                    let (source, now) = (format!("editor:{author}"), now_ms());
                    manual_stmt.execute(params![id, hash as i64, it, source, now])?;
                    touched.push((id, hash));
                    let others: Vec<u32> =
                        same_stmt.query_map(params![id, hash as i64, it, source, now], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                    for other in others {
                        touched.push((other, hash));
                        repaired.push((other, hash, it.clone(), true));
                    }
                }
                Msg::Forget { id, hash } => {
                    forget_stmt.execute(params![id, hash as i64])?;
                    forget_proof_stmt.execute(params![id, hash as i64])?;
                    touched.push((id, hash));
                }
                Msg::Term(term, origin) => {
                    save_term(&tx, &term, origin)?;
                    for (id, hash, it) in repair_with(&tx, &term)? {
                        touched.push((id, hash));
                        repaired.push((id, hash, it, false));
                    }
                }
                Msg::Search { query, exact, reply } => searches.push((query, exact, reply)),
            }
        }
    }
    tx.commit()?;
    SHARED.stored.fetch_add(added, Ordering::Relaxed);
    map_refresh_all(con, &touched);
    // Machine translations the glossary or a hand correction of the same English fixed: the game
    // shows the corrected ones from now on.
    for (id, hash, it, manual) in repaired {
        remember(id, hash, &it, manual);
        pipe::send_text(id, hash, &it);
    }
    Ok(searches)
}

/// A term of the glossary, as one row per wrong form.
fn save_term(tx: &Connection, term: &Term, origin: Origin) -> rusqlite::Result<()> {
    tx.prepare_cached("DELETE FROM glossary WHERE en = ?1")?.execute([&term.en])?;
    let mut insert = tx.prepare_cached("INSERT INTO glossary (en, wrong, it, at, local) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for wrong in &term.wrong {
        insert.execute(params![term.en, wrong, term.it, now_ms(), origin == Origin::Local])?;
    }
    Ok(())
}

/// Fixes the machine translations of texts with the English word of `term` that have it wrong.
/// Returns them. Not the hand corrections; not the server's proof of a text it made, which no
/// longer matches.
fn repair_with(tx: &Connection, term: &Term) -> rusqlite::Result<Vec<(u32, u64, String)>> {
    let pattern = format!("%{}%", term.en.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let rows: Vec<(u32, i64, String, String)> = {
        let mut stmt = tx.prepare_cached(
            "SELECT t.id, t.hash, s.en, t.it FROM translations t
             JOIN strings s ON s.id = t.id AND s.hash = t.hash
             WHERE t.status = 'auto' AND s.en LIKE ?1 ESCAPE '\\'",
        )?;
        stmt.query_map([pattern], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let mut update = tx.prepare_cached("UPDATE translations SET it = ?3, at = ?4 WHERE id = ?1 AND hash = ?2")?;
    let mut forget_proof = tx.prepare_cached("DELETE FROM proofs WHERE id = ?1 AND hash = ?2")?;
    let mut fixed = Vec::new();
    for (id, hash, en, it) in rows {
        let new = glossary::fix(&en, &it, std::slice::from_ref(term));
        if new != it {
            update.execute(params![id, hash, new, now_ms()])?;
            forget_proof.execute(params![id, hash])?;
            fixed.push((id, hash as u64, new));
        }
    }
    if !fixed.is_empty() {
        log::info!("glossary: {} translations fixed for {}", fixed.len(), term.en);
    }
    Ok(fixed)
}

fn find(con: &Connection, query: &str, exact: bool) -> rusqlite::Result<Vec<Row>> {
    let query = query.trim();
    let id: Option<u32> = query.parse().ok();
    let pattern = format!("%{}%", query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = con.prepare_cached(
        "SELECT s.id, s.hash, s.en, t.it, t.status
         FROM (SELECT id, hash, en, last_seen FROM strings
               UNION ALL
               SELECT c.id, c.hash, c.en, 0 FROM cloud_strings c
               WHERE NOT EXISTS (SELECT 1 FROM strings x WHERE x.id = c.id AND x.hash = c.hash)) s
         LEFT JOIN translations t ON t.id = s.id AND t.hash = s.hash
         WHERE ?1 = '' OR s.id = ?2
            OR (?5 = 0 AND (s.en LIKE ?3 ESCAPE '\\' OR t.it LIKE ?3 ESCAPE '\\'))
            OR (?5 = 1 AND (s.en = ?1 OR t.it = ?1))
         ORDER BY s.last_seen DESC LIMIT ?4",
    )?;
    stmt.query_map(params![query, id, pattern, SEARCH_MAX as i64, exact], |r| {
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
                    EXISTS (SELECT 1 FROM strings s WHERE s.id = t.id AND s.hash = t.hash), t.status = 'manual'
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
                r.get::<_, bool>(6)?,
            ))
        })?;
        for row in rows {
            let (id, hash, it, at, source, seen, manual) = row?;
            self.last_at = self.last_at.max(at);
            if lookup(id, hash).as_deref() != Some(it.as_str()) {
                // From the global database (sync.rs): thousands at the first sync, not logged.
                if !source.starts_with("cloud:") {
                    log::info!("translation {id} changed outside the addon: {it:?}");
                }
                if let Err(e) = map_refresh(con, id, hash) {
                    log::error!("map_it.db: {e}");
                }
                remember(id, hash, &it, manual);
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
    fn exact_search_finds_the_whole_text_only() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO strings VALUES (1, 1, 'Seraph', 0, 3, 1), (2, 2, 'Seraph Guard', 0, 2, 1), (3, 3, 'seraph', 0, 1, 1);
             INSERT INTO translations VALUES (1, 1, 'Serafini', 'auto', 'deepl', 1), (2, 2, 'Guardia dei Serafini', 'auto', 'deepl', 1);",
        )
        .unwrap();
        let ids = |query: &str, exact: bool| -> Vec<u32> { find(&con, query, exact).unwrap().iter().map(|r| r.id).collect() };
        // A part of the text, with any capitals, or the whole text with exactly its capitals.
        assert_eq!(ids("Seraph", false), [1, 2, 3]);
        assert_eq!(ids("Seraph", true), [1]);
        assert_eq!(ids("Serafini", true), [1]);
        assert_eq!(ids("Guardia dei Serafini", true), [2]);
        assert_eq!(ids("Guardia", true), Vec::<u32>::new());
        // An ID still finds its text, and an empty search everything.
        assert_eq!(ids("2", true), [2]);
        assert_eq!(ids("", true), [1, 2, 3]);
        // The texts of the global database the game has not shown here: after the others.
        con.execute_batch(
            "INSERT INTO cloud_strings VALUES (4, 4, 'Seraph Archer'), (1, 1, 'Seraph');
             INSERT INTO translations VALUES (4, 4, 'Arciere dei Seraph', 'auto', 'cloud:Bea', 1);",
        )
        .unwrap();
        assert_eq!(ids("Seraph", false), [1, 2, 3, 4]);
        assert_eq!(ids("Arciere", false), [4]);
        assert_eq!(find(&con, "4", true).unwrap()[0].en, "Seraph Archer");
    }

    #[test]
    fn a_hand_correction_goes_to_the_same_english_under_other_ids() {
        let mut con = Connection::open_in_memory().unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO strings VALUES (1, 7, 'Torch', 0, 0, 1), (2, 7, 'Torch', 0, 0, 1), (3, 7, 'Torch', 0, 0, 1),
                                        (4, 8, 'Torches', 0, 0, 1);
             INSERT INTO translations VALUES (1, 7, 'Fiaccola', 'auto', 'deepl', 1), (2, 7, 'Fiaccola', 'auto', 'copy', 1),
                                             (3, 7, 'Torcia di Bea', 'manual', 'cloud:Bea', 1), (4, 8, 'Fiaccole', 'auto', 'deepl', 1);",
        )
        .unwrap();
        save(&mut con, vec![Msg::Manual { id: 1, hash: 7, it: "Torcia".into(), author: "Ana".into() }]).unwrap();
        let rows: Vec<(u32, String, String, String)> = con
            .prepare("SELECT id, it, status, source FROM translations ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        // The machine translation takes it; another hand correction and another English do not.
        assert_eq!(
            rows,
            [
                (1, "Torcia".into(), "manual".into(), "editor:Ana".into()),
                (2, "Torcia".into(), "manual".into(), "editor:Ana".into()),
                (3, "Torcia di Bea".into(), "manual".into(), "cloud:Bea".into()),
                (4, "Fiaccole".into(), "auto".into(), "deepl".into()),
            ]
        );
    }

    #[test]
    fn the_general_translation_of_an_english_comes_last() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO translations VALUES (1, 7, 'Torcia', 'manual', 'editor:Ana', 1), (2, 7, 'Torcia', 'manual', 'editor:Ana', 1),
                                             (3, 7, 'Torcia di Bea', 'manual', 'cloud:Bea', 9), (4, 7, 'Fiaccola', 'auto', 'deepl', 10),
                                             (5, 8, 'Fiaccole', 'auto', 'deepl', 1), (6, 8, 'Fiaccole nuove', 'auto', 'deepl', 2);",
        )
        .unwrap();
        let mut same = HashMap::new();
        let mut stmt = con.prepare(LOAD).unwrap();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(1)?, r.get::<_, String>(2)?))).unwrap() {
            let (hash, it) = row.unwrap();
            same.insert(hash, it);
        }
        // The text most hand corrections have, not the last one; with none, the newest.
        assert_eq!((same[&7].as_str(), same[&8].as_str()), ("Torcia", "Fiaccole nuove"));
    }

    #[test]
    fn a_learned_term_repairs_machine_translations_only() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO strings VALUES (1, 1, 'Seraph Vanguard', 0, 0, 1), (2, 2, 'Seraph Guard', 0, 0, 1),
                                        (3, 3, 'Seraph Axe', 0, 0, 1), (4, 4, 'Sword', 0, 0, 1);
             INSERT INTO translations VALUES (1, 1, 'Avanguardia dei Serafi', 'auto', 'deepl', 1),
                                             (2, 2, 'Guardia dei Serafi', 'manual', 'editor:Ana', 1),
                                             (3, 3, 'Ascia dei Serafi', 'auto', 'server:deepl', 1),
                                             (4, 4, 'Spada dei Serafi', 'auto', 'deepl', 1);
             INSERT INTO proofs VALUES (1, 1, 'sig', 'raw');",
        )
        .unwrap();
        let term = Term { en: "seraph".into(), it: "Serafini".into(), wrong: vec!["Serafi".into()] };
        save_term(&con, &term, Origin::Cloud).unwrap();
        let fixed = repair_with(&con, &term).unwrap();
        assert_eq!(fixed, [(1, 1, "Avanguardia dei Serafini".into()), (3, 3, "Ascia dei Serafini".into())]);
        let it = |id: u32| -> String { con.query_row("SELECT it FROM translations WHERE id = ?1", [id], |r| r.get(0)).unwrap() };
        // The hand correction and the text without the word are untouched.
        assert_eq!((it(2), it(4)), ("Guardia dei Serafi".into(), "Spada dei Serafi".into()));
        let proofs: i64 = con.query_row("SELECT count(*) FROM proofs", [], |r| r.get(0)).unwrap();
        assert_eq!(proofs, 0);
        // Saved and loaded back.
        assert_eq!(load_glossary(&con).unwrap(), [term]);
    }

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
