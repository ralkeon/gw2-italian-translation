//! The project's translations database (crates/server on ifalna): every couple of minutes the
//! translations made there by the agents come here. Read only: nothing is sent but where the last
//! sync stopped.
//!
//! This thread has its own connection to strings.db. What it writes is seen by the store thread
//! like any other outside change (store::Watch), which keeps it in memory and sends it to the game
//! and to map_it.db if there. Received rows have source 'cloud:<author>'.
//!
//! Merge rules: a received hand correction replaces anything, a received machine translation only a
//! machine translation (the hand corrections an earlier version's editor made here stay).
//!
//! When the server does not answer, the game keeps what strings.db already has and the sync tries
//! again later (there is no spare server any more).

use std::{
    path::{Path, PathBuf},
    sync::{
        LazyLock, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use ureq::Agent;

use crate::{
    config, markup,
    state::SHARED,
    store::{self, now_ms},
};

/// Requests in a row when there is a lot to exchange, e.g. the first sync of an installation.
const ROUNDS_MAX: usize = 20;
/// Between two syncs, unless the server asks for another interval.
const INTERVAL: Duration = Duration::from_secs(120);
/// First sync after loading.
const FIRST: Duration = Duration::from_secs(15);
/// Longest wait after errors.
const RETRY_MAX: Duration = Duration::from_secs(1800);

static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into()
});

static WORKER: Mutex<Option<(Sender<()>, JoinHandle<()>)>> = Mutex::new(None);

pub fn start(db: PathBuf) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("gw2it-sync".into())
        .spawn(move || run(&db, &rx))
        .expect("spawn sync thread");
    *WORKER.lock().unwrap() = Some((tx, handle));
}

pub fn stop() {
    let worker = WORKER.lock().ok().and_then(|mut w| w.take());
    if let Some((tx, handle)) = worker {
        drop(tx); // wakes the thread; at worst a request in flight, bounded by the HTTP timeout
        let _ = handle.join();
    }
}

#[derive(Debug)]
enum Error {
    /// The server does not know our token (new server, or it was revoked): get a new one.
    Unauthorized,
    /// No answer, or the server is down (5xx).
    Unreachable(String),
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("chiave non riconosciuta, ne chiedo una nuova"),
            Self::Unreachable(e) | Self::Other(e) => f.write_str(e),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Other(format!("strings.db: {e}"))
    }
}

fn run(db: &Path, stop: &Receiver<()>) {
    let mut con = match open(db) {
        Ok(con) => con,
        Err(e) => {
            log::error!("sync: cannot open strings.db: {e}");
            set_status("errore: strings.db non accessibile");
            return;
        }
    };
    let mut wait = FIRST;
    let mut failures = 0u32;
    loop {
        match stop.recv_timeout(wait) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => return,
        }
        let sync = config::get().sync;
        if !sync.enabled || sync.server().is_empty() {
            set_status("spenta");
            wait = INTERVAL;
            continue;
        }
        match cycle(&mut con, &sync, stop) {
            Ok(next) => {
                failures = 0;
                wait = next;
            }
            Err(Error::Unauthorized) => {
                log::warn!("sync: token refused, registering again");
                let mut c = config::get();
                c.sync.token.clear();
                config::set(c);
                failures += 1;
                wait = FIRST;
            }
            Err(e) => {
                failures += 1;
                wait = (Duration::from_secs(60) * 2u32.pow(failures.min(5))).min(RETRY_MAX);
                log::warn!("sync: {e}; next try in {}s", wait.as_secs());
                set_status(&format!("errore: {e} (riprovo tra {} min)", wait.as_secs().div_ceil(60)));
            }
        }
    }
}

fn open(db: &Path) -> rusqlite::Result<Connection> {
    let con = Connection::open(db)?;
    con.busy_timeout(Duration::from_secs(10))?;
    // The store thread created the tables; this one is ours.
    con.execute_batch("CREATE TABLE IF NOT EXISTS sync_state (k TEXT PRIMARY KEY, v INTEGER NOT NULL);")?;
    // Before cloud_strings the English of what was received was not kept: everything once more.
    con.execute_batch(
        "DELETE FROM sync_state WHERE k = 'seq'
             AND NOT EXISTS (SELECT 1 FROM sync_state WHERE k = 'cloud_en');
         INSERT OR IGNORE INTO sync_state (k, v) VALUES ('cloud_en', 1);
         DELETE FROM sync_state WHERE k IN ('seq_spare', 'gseq_spare');",
    )?;
    Ok(con)
}

/// Where the last sync stopped: newest seq received, and the last row sent, by (at, id, hash) (from
/// earlier versions, which sent hand corrections). The same for the glossary: newest seq received, and the time of the newest term learned here that was sent.
#[derive(Clone, Copy, Default)]
struct Mark {
    seq: i64,
    at: i64,
    id: i64,
    hash: i64,
    gseq: i64,
    gsent: i64,
    /// The time of the newest name that stays in English received (from any server).
    nseq: i64,
    /// The same for the names taken out of them.
    useq: i64,
    /// The newest row of the all-Italian variant received.
    fseq: i64,
}

impl Mark {
    fn load(con: &Connection) -> rusqlite::Result<Self> {
        let get = |k: &str| -> rusqlite::Result<i64> {
            Ok(con.query_row("SELECT v FROM sync_state WHERE k = ?1", [k], |r| r.get(0)).optional()?.unwrap_or(0))
        };
        Ok(Self { seq: get("seq")?, at: get("sent_at")?, id: get("sent_id")?, hash: get("sent_hash")?, gseq: get("gseq")?, gsent: get("gsent_at")?, nseq: get("nseq")?, useq: get("useq")?, fseq: get("fseq")? })
    }

    fn save(&self, tx: &Transaction) -> rusqlite::Result<()> {
        let mut stmt = tx.prepare_cached("INSERT OR REPLACE INTO sync_state (k, v) VALUES (?1, ?2)")?;
        for (k, v) in [
            ("seq", self.seq),
            ("sent_at", self.at),
            ("sent_id", self.id),
            ("sent_hash", self.hash),
            ("gseq", self.gseq),
            ("gsent_at", self.gsent),
            ("nseq", self.nseq),
            ("useq", self.useq),
            ("fseq", self.fseq),
        ] {
            stmt.execute(params![k, v])?;
        }
        Ok(())
    }
}

/// A row received: [id, hash, en, it, status, author, seq].
#[derive(Deserialize)]
struct Remote(u32, String, String, String, String, String, #[allow(dead_code)] i64);

#[derive(Deserialize)]
struct Reply {
    rows: Vec<Remote>,
    seq: i64,
    more: bool,
    next_s: u64,
    // The glossary's seq (its terms are not used here); missing from a server without it.
    #[serde(default)]
    gseq: i64,
    #[serde(default)]
    gmore: bool,
    // Missing from a server without the names that stay in English.
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    nseq: i64,
    // Missing from a server that does not take names out.
    #[serde(default)]
    unnames: Vec<String>,
    #[serde(default)]
    useq: i64,
    // Missing from a server without the all-Italian variant.
    #[serde(default)]
    full: Vec<RemoteFull>,
    #[serde(default)]
    fseq: i64,
    #[serde(default)]
    fmore: bool,
}

/// A row of the all-Italian variant received: [id, hash, it, seq]; an empty `it` is a row taken out.
#[derive(Deserialize)]
struct RemoteFull(u32, String, String, #[allow(dead_code)] i64);

/// One sync: a few requests while there is more to receive. Returns the wait before the next one.
/// Nothing is sent but the marks: the translations are made on the server only.
fn cycle(con: &mut Connection, sync: &config::Sync, stop: &Receiver<()>) -> Result<Duration, Error> {
    let server = sync.server();
    let token = if sync.token.is_empty() { register(server)? } else { sync.token.clone() };
    let mut next = INTERVAL;
    let mut received = 0;
    for _ in 0..ROUNDS_MAX {
        let mut mark = Mark::load(con)?;
        // The glossary is no longer used here (the rows come made by the agents): its seq still
        // moves on, so that a server that sends it does not send it again.
        let reply: Reply = post(
            &format!("{server}/sync"),
            Some(&token),
            &json!({ "since": mark.seq, "rows": [], "gsince": mark.gseq, "glossary": [], "nsince": mark.nseq, "usince": mark.useq, "fsince": mark.fseq }),
        )?;
        mark.seq = mark.seq.max(reply.seq);
        mark.gseq = mark.gseq.max(reply.gseq);
        mark.nseq = mark.nseq.max(reply.nseq);
        mark.useq = mark.useq.max(reply.useq);
        received_names(con, &reply.names, &reply.unnames)?;
        received_full(con, &reply.full)?;
        mark.fseq = mark.fseq.max(reply.fseq);
        received += apply(con, &reply.rows, mark)?;
        next = Duration::from_secs(reply.next_s.clamp(30, 3600));
        let stopping = matches!(stop.try_recv(), Err(TryRecvError::Disconnected));
        if stopping || (!reply.more && !reply.gmore && !reply.fmore) {
            break;
        }
    }
    if received > 0 {
        log::info!("sync: {received} received");
    }
    if let Ok(mut s) = SHARED.sync.lock() {
        s.at = Some(Instant::now());
        s.status = "ok".into();
    }
    Ok(next)
}

fn register(server: &str) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Registered {
        token: String,
    }
    let name = config::contributor().unwrap_or_default();
    let Registered { token } = post(&format!("{server}/register"), None, &json!({ "name": name }))?;
    let mut c = config::get();
    c.sync.token = token.clone();
    config::set(c);
    log::info!("sync: registered on {server}");
    Ok(token)
}

fn post<T: DeserializeOwned>(url: &str, token: Option<&str>, body: &Value) -> Result<T, Error> {
    let mut request = AGENT.post(url);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let mut response = request.send_json(body).map_err(|e| Error::Unreachable(e.to_string()))?;
    match response.status().as_u16() {
        200 => response.body_mut().read_json().map_err(|e| Error::Other(e.to_string())),
        401 => Err(Error::Unauthorized),
        429 => Err(Error::Other("server occupato".into())),
        s @ 500.. => Err(Error::Unreachable(format!("HTTP {s}"))),
        s => {
            let text = response.body_mut().read_to_string().unwrap_or_default();
            Err(Error::Other(format!("HTTP {s} {}", text.chars().take(200).collect::<String>())))
        }
    }
}

/// The all-Italian variant's rows received: kept in the table `full` (store.rs reads them from there
/// and, with the option on, sends them to the game). An empty one is a row taken out: kept empty, so
/// that the store sees it go. A row whose markup does not match our English is not taken.
fn received_full(con: &mut Connection, rows: &[RemoteFull]) -> rusqlite::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = con.transaction()?;
    {
        let mut en_of = tx.prepare_cached(
            "SELECT en FROM strings WHERE id = ?1 AND hash = ?2 UNION ALL SELECT en FROM cloud_strings WHERE id = ?1 AND hash = ?2 LIMIT 1",
        )?;
        let mut put = tx.prepare_cached("INSERT OR REPLACE INTO full (id, hash, it, at) VALUES (?1, ?2, ?3, ?4)")?;
        // `at` grows with every row: the store picks up what is newer than what it has seen.
        let mut at = now_ms();
        for RemoteFull(id, hash, it, _) in rows {
            let Ok(hash) = u64::from_str_radix(hash, 16) else {
                continue;
            };
            if !it.is_empty()
                && let Some(en) = en_of.query_row(params![id, hash as i64], |r| r.get::<_, String>(0)).optional()?
                && !markup::token_mismatch(&en, it).is_empty()
            {
                log::warn!("sync: all-Italian text of {id} breaks the markup: skipped");
                continue;
            }
            put.execute(params![id, hash as i64, it, at])?;
            at += 1;
        }
    }
    tx.commit()
}

/// The names that stay in English the server sent (map_it.db's checks use them), and the ones it
/// took out of them.
fn received_names(con: &mut Connection, names: &[String], unnames: &[String]) -> rusqlite::Result<()> {
    if names.is_empty() && unnames.is_empty() {
        return Ok(());
    }
    let tx = con.transaction()?;
    {
        let mut insert = tx.prepare_cached("INSERT OR IGNORE INTO english_names (name) VALUES (?1)")?;
        for name in names.iter().filter(|n| n.chars().count() <= 100) {
            insert.execute([name])?;
        }
        let mut delete = tx.prepare_cached("DELETE FROM english_names WHERE name = ?1")?;
        for name in unnames {
            delete.execute([name])?;
        }
    }
    tx.commit()?;
    log::info!("sync: {} names that stay in English, {} taken out", names.len(), unnames.len());
    store::load_names(con)
}

/// Saves the received rows and the new mark, in one transaction. Returns how many changed here.
fn apply(con: &mut Connection, rows: &[Remote], mark: Mark) -> Result<usize, Error> {
    let tx = con.transaction()?;
    let mut changed = 0;
    {
        let mut local_en = tx.prepare_cached("SELECT en FROM strings WHERE id = ?1 AND hash = ?2")?;
        let mut cloud_en = tx.prepare_cached("INSERT OR REPLACE INTO cloud_strings (id, hash, en) VALUES (?1, ?2, ?3)")?;
        // ?7..?9: the last row sent. A hand correction made after it is still ours alone.
        // `at` always moves forward: a new text with the same `at` would look like an outside
        // edit to the translations_edited trigger, and become a hand correction.
        let mut upsert = tx.prepare_cached(
            "INSERT INTO translations (id, hash, it, status, source, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id, hash) DO UPDATE SET it = excluded.it, status = excluded.status,
                 source = excluded.source, at = max(excluded.at, translations.at + 1)
             WHERE translations.it IS NOT excluded.it
               AND (excluded.status = 'manual' OR translations.status = 'auto')
               AND NOT (translations.status = 'manual' AND translations.source NOT LIKE 'cloud:%'
                        AND (translations.at, translations.id, translations.hash) > (?7, ?8, ?9))",
        )?;
        for Remote(id, hash, en, it, status, author, _) in rows {
            let Ok(hash) = u64::from_str_radix(hash, 16) else {
                continue;
            };
            if status != "auto" && status != "manual" || it.trim().is_empty() {
                continue;
            }
            // Never trust the markup of a text from outside: it must match our English.
            let en = match local_en.query_row(params![id, hash as i64], |r| r.get::<_, String>(0)).optional()? {
                Some(en) => en,
                None => {
                    cloud_en.execute(params![id, hash as i64, en])?;
                    en.clone()
                }
            };
            let mismatch = markup::token_mismatch(&en, it);
            if !mismatch.is_empty() {
                log::warn!("sync: translation of {id} from {author} breaks the markup ({}): skipped", mismatch.join(" "));
                continue;
            }
            // The wrapper and the % signs a translator moved, put right; a machine translation that
            // stays broken is not taken (the English is better than a text the game shows wrong).
            let repaired = markup::repair(&en, it);
            let it = repaired.as_str();
            if status == "auto" && (markup::wrap_broken(&en, it) || markup::stray_percent(it)) {
                log::warn!("sync: translation of {id} from {author} has the markup in the wrong place: skipped");
                continue;
            }
            changed += upsert.execute(params![
                id,
                hash as i64,
                it,
                status,
                format!("cloud:{author}"),
                now_ms(),
                mark.at,
                mark.id,
                mark.hash
            ])?;
        }
    }
    mark.save(&tx)?;
    tx.commit()?;
    Ok(changed)
}

fn set_status(s: &str) {
    if let Ok(mut sync) = SHARED.sync.lock() {
        s.clone_into(&mut sync.status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_italian_rows_are_kept_the_broken_ones_not() {
        let mut con = Connection::open_in_memory().unwrap();
        con.execute_batch(crate::store::SCHEMA).unwrap();
        con.execute("INSERT INTO strings (id, hash, en, first_seen, last_seen) VALUES (1, 16, 'Grant %str1% might.', 0, 0)", []).unwrap();
        let rows = vec![
            RemoteFull(1, "10".into(), "Conferisci %str1% Forza.".into(), 1),
            RemoteFull(2, "20".into(), "Il Re Folle".into(), 2),
            RemoteFull(1, "11".into(), String::new(), 3),
        ];
        received_full(&mut con, &rows).unwrap();
        // The markup lost: not taken, the row before stays.
        received_full(&mut con, &[RemoteFull(1, "10".into(), "Conferisci Forza.".into(), 4)]).unwrap();
        let got: Vec<(i64, i64, String)> = con
            .prepare("SELECT id, hash, it FROM full ORDER BY id, hash")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(got, vec![(1, 16, "Conferisci %str1% Forza.".into()), (1, 17, String::new()), (2, 32, "Il Re Folle".into())]);
    }

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(crate::store::SCHEMA).unwrap();
        con.execute_batch("CREATE TABLE sync_state (k TEXT PRIMARY KEY, v INTEGER NOT NULL);").unwrap();
        con.execute_batch(
            "INSERT INTO strings VALUES (1, 1, 'Sword', 0, 0, 1), (2, 2, 'Shield', 0, 0, 1),
                                        (3, 3, '%str1% Bow', 0, 0, 1);
             INSERT INTO translations VALUES (1, 1, 'Spada', 'auto', 'deepl', 10),
                                             (2, 2, 'Scudo', 'manual', 'editor:Ana', 20);",
        )
        .unwrap();
        con
    }

    fn remote(id: u32, en: &str, it: &str, status: &str) -> Remote {
        Remote(id, format!("{id:016x}"), en.into(), it.into(), status.into(), "Bea".into(), 0)
    }

    fn texts(con: &Connection) -> Vec<(u32, String, String)> {
        let mut stmt = con.prepare("SELECT id, it, source FROM translations ORDER BY id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn merge() {
        let mut con = db();
        // Row 2 is a hand correction an earlier version's editor made here (at 20).
        let past = Mark { seq: 0, at: 20, id: 2, hash: 2, ..Mark::default() };
        let rows = [
            remote(1, "Sword", "Lama", "auto"),      // machine over machine: taken
            remote(2, "Shield", "Egida", "auto"),    // machine over a hand correction: refused
            remote(3, "%str1% Bow", "Arco", "auto"), // markup lost: refused
            remote(4, "Axe", "Ascia", "auto"),       // never seen here: kept for later
        ];
        assert_eq!(apply(&mut con, &rows, Mark { seq: 9, ..past }).unwrap(), 2);
        assert_eq!(texts(&con), [
            (1, "Lama".into(), "cloud:Bea".into()),
            (2, "Scudo".into(), "editor:Ana".into()),
            (4, "Ascia".into(), "cloud:Bea".into()),
        ]);
        assert_eq!(Mark::load(&con).unwrap().seq, 9);
        // A hand correction from the server replaces it.
        apply(&mut con, &[remote(2, "Shield", "Egida", "manual")], past).unwrap();
        assert_eq!(texts(&con)[1], (2, "Egida".into(), "cloud:Bea".into()));
    }

    #[test]
    fn the_english_of_what_is_received_is_kept() {
        let mut con = db();
        apply(&mut con, &[remote(4, "Axe", "Ascia", "auto"), remote(1, "Sword", "Lama", "auto")], Mark::default()).unwrap();
        // The English of a text the game has not shown here is kept; one it has shown is in `strings`.
        let kept: Vec<(u32, String)> = {
            let mut stmt = con.prepare("SELECT id, en FROM cloud_strings").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(kept, [(4, "Axe".to_owned())]);
    }

    #[test]
    fn names_that_stay_in_english_are_kept() {
        let mut con = db();
        let count = |con: &Connection| -> i64 { con.query_row("SELECT count(*) FROM english_names", [], |r| r.get(0)).unwrap() };
        received_names(&mut con, &["Signet of Rage".to_owned(), "Firebrand".to_owned(), "Mithril Ore".to_owned()], &[]).unwrap();
        received_names(&mut con, &["Firebrand".to_owned()], &[]).unwrap();
        assert_eq!(count(&con), 3);
        // Taken out by the server: no longer kept.
        received_names(&mut con, &[], &["Mithril Ore".to_owned()]).unwrap();
        assert_eq!(count(&con), 2);
    }
}
