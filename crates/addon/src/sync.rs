//! Global translations database (server/ in the repository): every couple of minutes the
//! translations made here (by DeepL or by hand) go to the server, and those made by the other
//! players come back. One request does both.
//!
//! This thread has its own connection to strings.db. What it writes is seen by the store thread
//! like any other outside change (store::Watch), which keeps it in memory and sends it to the game
//! and to map_it.db if there. Received rows have source 'cloud:<author>' and are never sent back.
//!
//! Merge rules, the same as on the server: a hand correction replaces anything, a machine
//! translation only replaces another machine translation. A hand correction made here and not
//! sent yet is never replaced.
//!
//! When the main server does not answer, the same sync goes to its spare (config.rs), which holds
//! a copy of it as of last night with the same seq; the main server takes back what was written
//! there (crates/server/src/replicate.rs). Every sync tries the main server first.

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

use gw2it_text::glossary::{self, Term};

use crate::{
    config, markup,
    state::SHARED,
    store::{self, now_ms},
};

/// Rows sent in one request (the server accepts up to 200).
const PUSH_MAX: usize = 200;
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
    /// True for the spare's token.
    Unauthorized(bool),
    /// No answer, or the server is down (5xx): worth trying the spare.
    Unreachable(String),
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized(_) => f.write_str("chiave non riconosciuta, ne chiedo una nuova"),
            Self::Unreachable(e) | Self::Other(e) => f.write_str(e),
        }
    }
}

/// The server a sync goes to.
#[derive(Clone, Copy)]
struct Target<'a> {
    url: &'a str,
    token: &'a str,
    spare: bool,
}

impl<'a> Target<'a> {
    fn main(sync: &'a config::Sync) -> Self {
        Self { url: sync.server(), token: &sync.token, spare: false }
    }

    fn spare(sync: &'a config::Sync) -> Option<Self> {
        Some(Self { url: sync.fallback()?, token: &sync.fallback_token, spare: true })
    }

    /// The newest seq received from it.
    fn seq_key(&self) -> &'static str {
        if self.spare { "seq_spare" } else { "seq" }
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
        match sync_once(&mut con, &sync, stop) {
            Ok((next, _)) => {
                failures = 0;
                wait = next;
            }
            Err(Error::Unauthorized(spare)) => {
                log::warn!("sync: token refused, registering again");
                let mut c = config::get();
                if spare { c.sync.fallback_token.clear() } else { c.sync.token.clear() }
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

/// One sync with the main server, or with the spare if the main one does not answer. Returns
/// the wait before the next one and whether it went to the spare.
fn sync_once(con: &mut Connection, sync: &config::Sync, stop: &Receiver<()>) -> Result<(Duration, bool), Error> {
    let main = Target::main(sync);
    match cycle(con, main, stop) {
        Ok(next) => {
            // On the main server: a later fallback starts again from its seq.
            con.execute("DELETE FROM sync_state WHERE k IN ('seq_spare', 'gseq_spare')", [])?;
            Ok((next, false))
        }
        Err(Error::Unreachable(e)) => {
            let stopping = matches!(stop.try_recv(), Err(TryRecvError::Disconnected));
            let Some(spare) = Target::spare(sync).filter(|_| !stopping) else {
                return Err(Error::Unreachable(e));
            };
            log::warn!("sync: {e}; trying the spare {}", spare.url);
            cycle(con, spare, stop).map(|next| (next, true))
        }
        Err(e) => Err(e),
    }
}

fn open(db: &Path) -> rusqlite::Result<Connection> {
    let con = Connection::open(db)?;
    con.busy_timeout(Duration::from_secs(10))?;
    // The store thread created the tables; this one is ours.
    con.execute_batch("CREATE TABLE IF NOT EXISTS sync_state (k TEXT PRIMARY KEY, v INTEGER NOT NULL);")?;
    // Before cloud_strings the English of what was received was not kept: everything once more.
    con.execute_batch(
        "DELETE FROM sync_state WHERE k IN ('seq', 'seq_spare')
             AND NOT EXISTS (SELECT 1 FROM sync_state WHERE k = 'cloud_en');
         INSERT OR IGNORE INTO sync_state (k, v) VALUES ('cloud_en', 1);",
    )?;
    Ok(con)
}

/// Where the last sync stopped: newest seq received (from the server in use), and the last row
/// sent, by (at, id, hash). The rows sent to the spare reach the main server through it. The same
/// for the glossary: newest seq received, and the time of the newest term learned here that was sent.
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
}

/// The sync_state key of the glossary's seq that goes with `seq_key`.
fn glossary_key(seq_key: &str) -> String {
    format!("g{seq_key}")
}

impl Mark {
    fn load(con: &Connection, seq_key: &str) -> rusqlite::Result<Self> {
        let get = |k: &str| -> rusqlite::Result<i64> {
            Ok(con.query_row("SELECT v FROM sync_state WHERE k = ?1", [k], |r| r.get(0)).optional()?.unwrap_or(0))
        };
        // The spare is a copy of the main server with its seq, plus what was written there (seq
        // from 10^12): what we had from the main server we need not get again.
        let seq = get(seq_key)?.max(get("seq")?);
        let gseq = get(&glossary_key(seq_key))?.max(get("gseq")?);
        Ok(Self { seq, at: get("sent_at")?, id: get("sent_id")?, hash: get("sent_hash")?, gseq, gsent: get("gsent_at")?, nseq: get("nseq")? })
    }

    fn save(&self, tx: &Transaction, seq_key: &str) -> rusqlite::Result<()> {
        let mut stmt = tx.prepare_cached("INSERT OR REPLACE INTO sync_state (k, v) VALUES (?1, ?2)")?;
        let gkey = glossary_key(seq_key);
        for (k, v) in [
            (seq_key, self.seq),
            ("sent_at", self.at),
            ("sent_id", self.id),
            ("sent_hash", self.hash),
            (gkey.as_str(), self.gseq),
            ("gsent_at", self.gsent),
            ("nseq", self.nseq),
        ] {
            stmt.execute(params![k, v])?;
        }
        Ok(())
    }
}

/// A row to send: [id, hash, en, it, status] (+ the server's proof for a text it translated: the
/// signature and the XML it answered), and its place in the order of sending.
struct Outgoing {
    row: Value,
    at: i64,
    id: i64,
    hash: i64,
}

/// A row received: [id, hash, en, it, status, author, seq].
#[derive(Deserialize)]
struct Remote(u32, String, String, String, String, String, #[allow(dead_code)] i64);

/// A term received: [en, it, [wrong...], author, seq].
#[derive(Deserialize)]
struct RemoteTerm(String, String, Vec<String>, #[allow(dead_code)] String, #[allow(dead_code)] i64);

#[derive(Deserialize)]
struct Reply {
    rows: Vec<Remote>,
    seq: i64,
    more: bool,
    next_s: u64,
    // Missing from a server without the glossary.
    #[serde(default)]
    glossary: Vec<RemoteTerm>,
    #[serde(default)]
    gseq: i64,
    #[serde(default)]
    gmore: bool,
    // Missing from a server without the names that stay in English.
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    nseq: i64,
}

/// Terms sent in one request (the server accepts up to 50).
const TERMS_MAX: usize = 50;

/// One sync: a few requests while there is more to send or to receive. Returns the wait before
/// the next one.
fn cycle(con: &mut Connection, target: Target, stop: &Receiver<()>) -> Result<Duration, Error> {
    let refused = |e| match e {
        Error::Unauthorized(_) => Error::Unauthorized(target.spare),
        e => e,
    };
    let server = target.url;
    let token = if target.token.is_empty() { register(target).map_err(refused)? } else { target.token.to_owned() };
    let mut next = INTERVAL;
    let (mut sent, mut received) = (0, 0);
    for _ in 0..ROUNDS_MAX {
        let mut mark = Mark::load(con, target.seq_key())?;
        let outgoing = pending(con, mark)?;
        let rows: Vec<&Value> = outgoing.iter().map(|o| &o.row).collect();
        let (terms, terms_at) = pending_terms(con, mark.gsent)?;
        let reply: Reply = post(
            &format!("{server}/sync"),
            Some(&token),
            &json!({ "since": mark.seq, "rows": rows, "gsince": mark.gseq, "glossary": terms, "nsince": mark.nseq }),
        )
        .map_err(refused)?;
        if let Some(last) = outgoing.last() {
            (mark.at, mark.id, mark.hash) = (last.at, last.id, last.hash);
        }
        mark.seq = mark.seq.max(reply.seq);
        mark.gsent = mark.gsent.max(terms_at);
        mark.gseq = mark.gseq.max(reply.gseq);
        mark.nseq = mark.nseq.max(reply.nseq);
        // The glossary first: the translations that come with it are repaired with it.
        received_terms(&reply.glossary);
        received_names(con, &reply.names)?;
        received += apply(con, &reply.rows, mark, target.seq_key())?;
        sent += outgoing.len();
        next = Duration::from_secs(reply.next_s.clamp(30, 3600));
        let stopping = matches!(stop.try_recv(), Err(TryRecvError::Disconnected));
        if stopping || (outgoing.len() < PUSH_MAX && !reply.more && terms.len() < TERMS_MAX && !reply.gmore) {
            break;
        }
    }
    if sent > 0 || received > 0 {
        log::info!("sync: {sent} sent, {received} received");
    }
    if let Ok(mut s) = SHARED.sync.lock() {
        s.at = Some(Instant::now());
        s.sent += sent as u64;
        s.received += received as u64;
        s.status = if target.spare { "ok (server di riserva)".into() } else { "ok".into() };
    }
    Ok(next)
}

fn register(target: Target) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Registered {
        token: String,
    }
    let name = config::contributor().unwrap_or_default();
    let Registered { token } = post(&format!("{}/register", target.url), None, &json!({ "name": name }))?;
    let mut c = config::get();
    if target.spare { c.sync.fallback_token = token.clone() } else { c.sync.token = token.clone() }
    config::set(c);
    log::info!("sync: registered on {}", target.url);
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
        401 => Err(Error::Unauthorized(false)),
        429 => Err(Error::Other("server occupato".into())),
        s @ 500.. => Err(Error::Unreachable(format!("HTTP {s}"))),
        s => {
            let text = response.body_mut().read_to_string().unwrap_or_default();
            Err(Error::Other(format!("HTTP {s} {}", text.chars().take(200).collect::<String>())))
        }
    }
}

/// Translations made here after `mark`, oldest first. Only texts whose English is known here (seen
/// in the game, or received from the global database and corrected in the editor): the English
/// goes with them, for whoever reads the global database.
fn pending(con: &Connection, mark: Mark) -> rusqlite::Result<Vec<Outgoing>> {
    let mut stmt = con.prepare_cached(
        "SELECT t.id, t.hash, s.en, t.it, t.status, t.at, p.sig, p.raw FROM translations t
         JOIN (SELECT id, hash, en FROM strings
               UNION ALL
               SELECT c.id, c.hash, c.en FROM cloud_strings c
               WHERE NOT EXISTS (SELECT 1 FROM strings x WHERE x.id = c.id AND x.hash = c.hash)) s
           ON s.id = t.id AND s.hash = t.hash
         LEFT JOIN proofs p ON p.id = t.id AND p.hash = t.hash AND t.status = 'auto'
         WHERE t.source NOT LIKE 'cloud:%' AND (t.at, t.id, t.hash) > (?1, ?2, ?3)
         ORDER BY t.at, t.id, t.hash LIMIT ?4",
    )?;
    stmt.query_map(params![mark.at, mark.id, mark.hash, PUSH_MAX as i64], |r| {
        let (id, hash): (i64, i64) = (r.get(0)?, r.get(1)?);
        let (en, it, status): (String, String, String) = (r.get(2)?, r.get(3)?, r.get(4)?);
        let hash_hex = format!("{:016x}", hash as u64);
        let row = match (r.get::<_, Option<String>>(6)?, r.get::<_, Option<String>>(7)?) {
            (Some(sig), Some(raw)) => json!([id, hash_hex, en, it, status, sig, raw]),
            _ => json!([id, hash_hex, en, it, status]),
        };
        Ok(Outgoing { row, at: r.get(5)?, id, hash })
    })?
    .collect()
}

/// The terms this installation learned that the server has not got (up to `TERMS_MAX`, oldest
/// first): [en, it, [wrong...]], and the time of the newest. Terms received from the server are
/// not sent back.
fn pending_terms(con: &Connection, after: i64) -> rusqlite::Result<(Vec<Value>, i64)> {
    let mut stmt = con.prepare_cached(
        "SELECT en, it, wrong, at FROM glossary
         WHERE en IN (SELECT en FROM glossary WHERE local = 1 AND at > ?1) ORDER BY en, at",
    )?;
    // One term per English word: its rows are together.
    let mut terms: Vec<(String, String, Vec<String>, i64)> = Vec::new();
    for row in stmt.query_map([after], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?)))? {
        let (en, it, wrong, at) = row?;
        match terms.last_mut().filter(|t| t.0 == en) {
            Some(term) => {
                term.2.push(wrong);
                term.3 = term.3.max(at);
            }
            None => terms.push((en, it, vec![wrong], at)),
        }
    }
    terms.sort_by_key(|t| t.3);
    terms.truncate(TERMS_MAX);
    let newest = terms.iter().map(|t| t.3).max().unwrap_or(after);
    Ok((terms.into_iter().map(|(en, it, wrong, _)| json!([en, it, wrong])).collect(), newest))
}

/// The terms the server sent: checked, then kept and used (the server is not trusted: they go into
/// the translators' prompts and change texts).
fn received_terms(terms: &[RemoteTerm]) {
    let valid: Vec<Term> = terms
        .iter()
        .map(|RemoteTerm(en, it, wrong, ..)| Term { en: en.clone(), it: it.clone(), wrong: wrong.clone() })
        .filter(|t| {
            let ok = t.is_valid();
            if !ok {
                log::warn!("sync: glossary term {:?} refused", t.en);
            }
            ok
        })
        .collect();
    store::receive_terms(valid);
}

/// The names that stay in English the server sent: kept, and used from now on (translate.rs).
fn received_names(con: &mut Connection, names: &[String]) -> rusqlite::Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    let tx = con.transaction()?;
    {
        let mut insert = tx.prepare_cached("INSERT OR IGNORE INTO english_names (name) VALUES (?1)")?;
        for name in names.iter().filter(|n| n.chars().count() <= 100) {
            insert.execute([name])?;
        }
    }
    tx.commit()?;
    log::info!("sync: {} names that stay in English", names.len());
    store::load_names(con)
}

/// Saves the received rows and the new mark, in one transaction. Returns how many changed here.
fn apply(con: &mut Connection, rows: &[Remote], mark: Mark, seq_key: &str) -> Result<usize, Error> {
    let tx = con.transaction()?;
    let mut changed = 0;
    let terms = store::glossary();
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
            // A machine translation from outside gets our glossary too (never a hand correction).
            let fixed;
            let it = if status == "auto" {
                fixed = glossary::fix(&en, it, &terms);
                &fixed
            } else {
                it
            };
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
    mark.save(&tx, seq_key)?;
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
    fn server_proof_goes_with_the_machine_translation_only() {
        let con = db();
        // A proof for the machine translation (1) and a stale one for the hand correction (2).
        con.execute_batch("INSERT INTO proofs VALUES (1, 1, 'abc123', '<x id=\"0\"/> Spada'), (2, 2, 'def456', 'Scudo');").unwrap();
        let out = pending(&con, Mark::default()).unwrap();
        assert_eq!(out.iter().map(|o| o.row.to_string()).collect::<Vec<_>>(), [
            r#"[1,"0000000000000001","Sword","Spada","auto","abc123","<x id=\"0\"/> Spada"]"#,
            r#"[2,"0000000000000002","Shield","Scudo","manual"]"#,
        ]);
    }

    #[test]
    fn merge() {
        let mut con = db();
        let out = pending(&con, Mark::default()).unwrap();
        assert_eq!(out.iter().map(|o| o.row.to_string()).collect::<Vec<_>>(), [
            r#"[1,"0000000000000001","Sword","Spada","auto"]"#,
            r#"[2,"0000000000000002","Shield","Scudo","manual"]"#,
        ]);
        // Sent: the mark is past both rows.
        let sent = Mark { seq: 0, at: 20, id: 2, hash: 2, ..Mark::default() };
        assert!(pending(&con, sent).unwrap().is_empty());

        // A hand correction made after sending: not replaced by the server's.
        con.execute("UPDATE translations SET it = 'Scudo nuovo', at = 30 WHERE id = 2", []).unwrap();
        let rows = [
            remote(1, "Sword", "Lama", "auto"),       // machine over machine: taken
            remote(2, "Shield", "Egida", "manual"),   // over our unsent correction: refused
            remote(3, "%str1% Bow", "Arco", "auto"),  // markup lost: refused
            remote(4, "Axe", "Ascia", "auto"),        // never seen here: kept for later
        ];
        assert_eq!(apply(&mut con, &rows, Mark { seq: 9, ..sent }, "seq").unwrap(), 2);
        assert_eq!(texts(&con), [
            (1, "Lama".into(), "cloud:Bea".into()),
            (2, "Scudo nuovo".into(), "editor:Ana".into()),
            (4, "Ascia".into(), "cloud:Bea".into()),
        ]);
        assert_eq!(Mark::load(&con, "seq").unwrap().seq, 9);
        // Received rows never go back; our correction does.
        let out = pending(&con, Mark::load(&con, "seq").unwrap()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].row[3], "Scudo nuovo");

        // Once sent, a newer correction from someone else replaces it; a machine one never does.
        let sent = Mark { seq: 9, at: out[0].at, id: 2, hash: 2, ..Mark::default() };
        apply(&mut con, &[remote(2, "Shield", "Egida", "manual"), remote(4, "Axe", "Scure", "manual")], sent, "seq").unwrap();
        apply(&mut con, &[remote(2, "Shield", "Scudetto", "auto")], sent, "seq").unwrap();
        assert_eq!(texts(&con)[1..], [(2, "Egida".into(), "cloud:Bea".into()), (4, "Scure".into(), "cloud:Bea".into())]);
    }

    #[test]
    fn what_is_received_can_be_found_and_corrected() {
        let mut con = db();
        apply(&mut con, &[remote(4, "Axe", "Ascia", "auto"), remote(1, "Sword", "Lama", "auto")], Mark::default(), "seq").unwrap();
        // The English of a text the game has not shown here is kept; one it has shown is in `strings`.
        let kept: Vec<(u32, String)> = {
            let mut stmt = con.prepare("SELECT id, en FROM cloud_strings").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(kept, [(4, "Axe".to_owned())]);
        // Corrected in the editor: it goes to the global database, with its English.
        con.execute("UPDATE translations SET it = 'Scure', status = 'manual', source = 'editor:Ana', at = 50 WHERE id = 4", []).unwrap();
        let out = pending(&con, Mark { at: 20, id: 2, hash: 2, ..Mark::default() }).unwrap();
        assert_eq!(out.iter().map(|o| o.row.to_string()).collect::<Vec<_>>(), [r#"[4,"0000000000000004","Axe","Scure","manual"]"#]);
    }

    #[test]
    fn names_that_stay_in_english_are_kept_and_used() {
        let mut con = db();
        received_names(&mut con, &["Signet of Rage".to_owned(), "Firebrand".to_owned()]).unwrap();
        received_names(&mut con, &["Firebrand".to_owned()]).unwrap();
        let count: i64 = con.query_row("SELECT count(*) FROM english_names", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        assert_eq!(store::english_name("Signet of Rage").as_deref(), Some("Signet of Rage"));
        assert_eq!(store::english_name("Signet of Rage Recharge"), None);
    }

    /// Against a running server: cd server && npx wrangler dev, then
    /// GW2IT_SYNC_URL=http://localhost:8787 cargo test -p gw2it-addon -- --ignored
    #[test]
    #[ignore]
    fn against_server() {
        let url = std::env::var("GW2IT_SYNC_URL").expect("GW2IT_SYNC_URL");
        let (_stop_tx, stop) = mpsc::channel();
        let (mut a, mut b) = (db(), Connection::open_in_memory().unwrap());
        b.execute_batch(crate::store::SCHEMA).unwrap();
        b.execute_batch("CREATE TABLE sync_state (k TEXT PRIMARY KEY, v INTEGER NOT NULL);").unwrap();
        let main = Target { url: &url, token: "", spare: false };
        cycle(&mut a, main, &stop).unwrap();
        cycle(&mut b, main, &stop).unwrap();
        let got = texts(&b);
        assert!(got.contains(&(1, "Spada".into(), "cloud:anonimo".into())), "{got:?}");
        assert!(got.contains(&(2, "Scudo".into(), "cloud:anonimo".into())), "{got:?}");
        assert!(pending(&b, Mark::load(&b, "seq").unwrap()).unwrap().is_empty());
    }

    #[test]
    fn terms_to_send_are_the_ones_learned_here() {
        let con = db();
        con.execute_batch(
            "INSERT INTO glossary VALUES ('seraph', 'Serafi', 'Serafini', 10, 1), ('seraph', 'Serafino', 'Serafini', 11, 1),
                                         ('vigil', 'Veglia', 'Vigilia', 12, 0), ('pact', 'Accordo', 'Patto', 30, 1);",
        )
        .unwrap();
        // One term with all its wrong forms, even those older than the mark; from the server: never.
        let (terms, newest) = pending_terms(&con, 0).unwrap();
        assert_eq!(terms, [json!(["seraph", "Serafini", ["Serafi", "Serafino"]]), json!(["pact", "Patto", ["Accordo"]])]);
        assert_eq!(newest, 30);
        let (terms, newest) = pending_terms(&con, 11).unwrap();
        assert_eq!(terms, [json!(["pact", "Patto", ["Accordo"]])]);
        assert_eq!(newest, 30);
        assert!(pending_terms(&con, 30).unwrap().0.is_empty());
    }

    /// The glossary through a real server, from a contributor to another installation:
    /// cargo build -p gw2it-server && GW2IT_SERVER_BIN=target/debug/gw2it-server cargo test -p gw2it-addon glossary_through -- --ignored
    #[test]
    #[ignore]
    fn glossary_through_the_server() {
        use std::process::Command;
        let bin = std::env::var("GW2IT_SERVER_BIN").expect("GW2IT_SERVER_BIN");
        let dir = std::env::temp_dir().join(format!("gw2it-glossary-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let server_db = dir.join("main.db");
        let mut child = Command::new(&bin).args(["--db", server_db.to_str().unwrap(), "--listen", "127.0.0.1:8796"]).spawn().unwrap();
        thread::sleep(Duration::from_millis(500));
        let url = "http://127.0.0.1:8796";
        let (_stop_tx, stop) = mpsc::channel();

        // A is a contributor, B is not (the server makes contributors by hand).
        let token_a = register(Target { url, token: "", spare: false }).unwrap();
        Connection::open(&server_db).unwrap().execute("UPDATE users SET contributor = 1", []).unwrap();
        let token_b = register(Target { url, token: "", spare: false }).unwrap();
        let (mut a, mut b) = (db(), db());
        a.execute_batch("INSERT INTO glossary VALUES ('seraph', 'Serafi', 'Serafini', 10, 1);").unwrap();
        b.execute_batch("INSERT INTO glossary VALUES ('vigil', 'Veglia', 'Vigilia', 20, 1);").unwrap();

        cycle(&mut a, Target { url, token: &token_a, spare: false }, &stop).unwrap();
        cycle(&mut b, Target { url, token: &token_b, spare: false }, &stop).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();

        // The contributor's term is on the server and reached B; B's own was refused.
        let on_server: Vec<(String, String, String, i64)> = {
            let check = Connection::open(&server_db).unwrap();
            let mut stmt = check.prepare("SELECT en, it, wrong, trust FROM glossary").unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(on_server, [("seraph".into(), "Serafini".into(), "[\"Serafi\"]".into(), 2)]);
        let got = store::glossary();
        assert!(got.iter().any(|t| t.en == "seraph" && t.it == "Serafini" && t.wrong == ["Serafi"]), "{got:?}");
        assert!(!got.iter().any(|t| t.en == "vigil"), "{got:?}");
        assert_eq!(Mark::load(&b, "seq").unwrap().gseq, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fallback and nightly copy, end to end. The spare: cd server && echo REPLICA_KEY=test > .dev.vars
    /// && npx wrangler d1 execute gw2it --local --file schema.sql && npx wrangler dev --port 8788.
    /// Then: cargo build -p gw2it-server && GW2IT_SPARE_URL=http://localhost:8788
    /// GW2IT_SERVER_BIN=target/debug/gw2it-server cargo test -p gw2it-addon failover -- --ignored
    #[test]
    #[ignore]
    fn failover() {
        use std::process::{Child, Command};
        let spare = std::env::var("GW2IT_SPARE_URL").expect("GW2IT_SPARE_URL");
        let bin = std::env::var("GW2IT_SERVER_BIN").expect("GW2IT_SERVER_BIN");
        let dir = std::env::temp_dir().join(format!("gw2it-failover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (main_db, key) = (dir.join("main.db"), dir.join("replica.key"));
        std::fs::write(&key, "test").unwrap();
        let start_main = || -> Child {
            let child = Command::new(&bin).args(["--db", main_db.to_str().unwrap(), "--listen", "127.0.0.1:8795"]).spawn().unwrap();
            thread::sleep(Duration::from_millis(500));
            child
        };
        let replicate = || {
            let out = Command::new(&bin)
                .args(["replicate", "--db", main_db.to_str().unwrap(), "--to", &spare, "--key-file", key.to_str().unwrap()])
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        let sync = config::Sync {
            enabled: true,
            url: "http://127.0.0.1:8795".into(),
            token: String::new(),
            fallback_url: spare.clone(),
            fallback_token: String::new(),
        };
        let (_stop_tx, stop) = mpsc::channel();
        let mut a = db();
        let mut b = Connection::open_in_memory().unwrap();
        b.execute_batch(crate::store::SCHEMA).unwrap();
        b.execute_batch(
            "CREATE TABLE sync_state (k TEXT PRIMARY KEY, v INTEGER NOT NULL);
             INSERT INTO strings VALUES (9, 9, 'Nine', 0, 0, 1);
             INSERT INTO translations VALUES (9, 9, 'Nove', 'manual', 'editor:Bea', 50);",
        )
        .unwrap();

        // Main server up: A sends its texts there; at night the spare gets a copy, with the glossary.
        let mut main = start_main();
        assert!(!sync_once(&mut a, &sync, &stop).unwrap().1);
        Connection::open(&main_db)
            .unwrap()
            .execute("INSERT INTO glossary VALUES ('failover', 'Passaggio', '[\"Cambio\"]', 'Ana', 1, 1, 2)", [])
            .unwrap();
        println!("{}", replicate());

        // Main server down: B falls back to the spare, gets A's texts from the copy, sends its own;
        // A falls back too and gets B's.
        main.kill().unwrap();
        main.wait().unwrap();
        assert!(sync_once(&mut b, &sync, &stop).unwrap().1);
        let terms = store::glossary();
        assert!(terms.iter().any(|t| t.en == "failover" && t.wrong == ["Cambio"]), "{terms:?}");
        let got = texts(&b);
        assert!(got.contains(&(1, "Spada".into(), "cloud:anonimo".into())), "{got:?}");
        assert!(sync_once(&mut a, &sync, &stop).unwrap().1);
        assert!(texts(&a).contains(&(9, "Nove".into(), "cloud:anonimo".into())), "{:?}", texts(&a));

        // Main server back: the night copy takes B's text back; everyone is on the main server.
        let mut main = start_main();
        let report = replicate();
        println!("{report}");
        assert!(report.contains("back 1 translations"), "{report}");
        assert!(!sync_once(&mut b, &sync, &stop).unwrap().1);
        let check = Connection::open(&main_db).unwrap();
        let nine: (String, String) =
            check.query_row("SELECT it, status FROM translations WHERE id = 9", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        // B is no contributor: its hand correction counts as a machine translation (trust levels).
        assert_eq!(nine, ("Nove".into(), "auto".into()));
        // The spare no longer holds rows of its own.
        assert!(replicate().contains("back 0 translations"));
        main.kill().unwrap();
        main.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
