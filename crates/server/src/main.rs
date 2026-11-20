//! Global translations database of Italian Translation, self-hosted on a local SQLite file
//! (schema.sql and sql/ next to this crate).
//!
//! Plain HTTP on a local address: a reverse proxy (Apache, Caddy...) in front gives it HTTPS.
//!
//! gw2it-server [--db gw2it.db] [--listen 127.0.0.1:8790]
//!              [--frozen]   (the database as it is: /sync gives everything but takes no rows or terms;
//!                            also when the file <db>.frozen exists)
//!              (/translate answers 503 "no translations on this server": the machine translators were
//!              removed on 2026-11-20; `--translators <file>` is still accepted and ignored, for the
//!              service's command line)
//! gw2it-server repair [--db gw2it.db] [--dry-run]   (the `%` a translator added, in every row)
//! gw2it-server bulk [--db gw2it.db] --file texts.jsonl (--ask-to <file> | --answers <file>) [--max-texts N] [--all] [--dry-run]
//!                                                                         (see bulk.rs)
//! gw2it-server lt-sync [--db gw2it.db] --glossary <file or URL> [--ask-to <file> | --answers <file>]
//!              [--max-texts 2000] [--from-last] [--dry-run]               (see lt_sync.rs)
//! gw2it-server gw2dat --dat Gw2.dat --seeds seeds.db --out gw2dat_en.db [--db gw2it.db] [--file map_it.db]
//!                                                                         (see gw2dat.rs)
//! gw2it-server seeds --out seeds.db   (Text Translator's seed pool, read only: see gw2dat.rs)
//! gw2it-server full --names names_it.toml --terms terms.toml [--ask-to <file> | --answers <file>] [--max-texts 200] [--dry-run]
//!                                                                         (see full.rs)

use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gw2it_text::names::Names;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};

mod bulk;
mod full;
mod glossary;
mod gw2dat;
mod names;
mod lt_sync;
mod byhand;
mod refs;
mod sign;

const SCHEMA: &str = include_str!("../schema.sql");
/// The tables of the old jobs, kept in the databases.
const SQL_OLD_JOBS: &str = include_str!("../sql/old_jobs.sql");
const SQL_HISTORY: &str = include_str!("../sql/history.sql");
const SQL_UPSERT: &str = include_str!("../sql/upsert.sql");
const SQL_PULL: &str = include_str!("../sql/pull.sql");
const SQL_SAME_HISTORY: &str = include_str!("../sql/same_history.sql");
const SQL_SAME: &str = include_str!("../sql/same.sql");

/// Rows accepted in one /sync: the addon sends at most this many.
const PUSH_MAX: usize = 200;
/// Rows returned by one /sync: with more, `more` is true and the addon calls again at once.
const PULL_MAX: i64 = 2000;
/// Seconds the addon waits before the next /sync.
const NEXT_S: u64 = 120;
const TEXT_MAX: usize = 4000;
const BODY_MAX: u64 = 1 << 20;
/// Requests per minute, per token (and per address for /register).
const RATE: u32 = 30;
/// Registrations per address in REGISTER_WINDOW.
const REGISTER_PER_DAY: u32 = 5;
const REGISTER_WINDOW: Duration = Duration::from_secs(86_400);
const THREADS: usize = 4;

struct App {
    db: Mutex<Connection>,
    /// Checks the translations the old /translate made and signed (sign.rs).
    secret: Vec<u8>,
    /// key -> (start of the current minute, requests in it)
    rate: Mutex<HashMap<String, (Instant, u32)>>,
    /// The names that stay in English (names.rs), and when they were read.
    names: Mutex<Option<(Instant, Arc<Names>)>>,
    /// --frozen: /sync serves the database as it is and writes nothing the addons send (2026-11-18:
    /// the agents retranslate everything into a new database; the old one stays as it was until then).
    frozen: bool,
}

/// How long the names read from the database are used before reading them again (someone may
/// add new ones).
const NAMES_FRESH: Duration = Duration::from_secs(600);

struct HttpError(u16, &'static str);

impl From<rusqlite::Error> for HttpError {
    fn from(e: rusqlite::Error) -> Self {
        eprintln!("database: {e}");
        Self(500, "internal error")
    }
}

const USAGE: &str = "usage: gw2it-server [--db gw2it.db] [--listen 127.0.0.1:8790] [--frozen]
       gw2it-server repair [--db gw2it.db] [--dry-run]
       gw2it-server bulk [--db gw2it.db] --file texts.jsonl (--ask-to <file> | --answers <file>) [--max-texts N] [--all] [--dry-run]
       gw2it-server lt-sync [--db gw2it.db] --glossary <file or URL> [--ask-to <file> | --answers <file>] [--max-texts 2000] [--from-last] [--dry-run]
       gw2it-server gw2dat --dat Gw2.dat --seeds seeds.db --out gw2dat_en.db [--db gw2it.db] [--file map_it.db]
       gw2it-server seeds --out seeds.db
       gw2it-server full --names names_it.toml --terms terms.toml [--ask-to <file> | --answers <file>] [--max-texts 200] [--all] [--dry-run]";

/// What the commands of the old machine-translation pipeline say now.
const REMOVED: &str = "removed on 2026-11-20 with the machine translators (the agents translate now)";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(old) = args.first().filter(|a| ["consistency", "names", "review", "plurals", "import-glossary"].contains(&a.as_str())) {
        eprintln!("gw2it-server {old}: {REMOVED}");
        std::process::exit(2);
    }
    let repair = args.first().is_some_and(|a| a == "repair");
    let bulk = args.first().is_some_and(|a| a == "bulk");
    let lt_sync = args.first().is_some_and(|a| a == "lt-sync");
    let gw2dat = args.first().is_some_and(|a| a == "gw2dat");
    let seeds = args.first().is_some_and(|a| a == "seeds");
    let full = args.first().is_some_and(|a| a == "full");
    if repair || bulk || lt_sync || gw2dat || seeds || full {
        args.remove(0);
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let from_last = args.iter().any(|a| a == "--from-last");
    let all = args.iter().any(|a| a == "--all");
    let frozen = args.iter().any(|a| a == "--frozen");
    args.retain(|a| a != "--dry-run" && a != "--from-last" && a != "--all" && a != "--frozen");
    let mut options = HashMap::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            (name @ ("--db" | "--listen" | "--to" | "--key-file" | "--translators" | "--max-texts" | "--file" | "--ask-to" | "--answers" | "--glossary" | "--dat" | "--seeds" | "--out" | "--names" | "--terms"), Some(v)) => {
                options.insert(name.to_owned(), v);
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let db = options.remove("--db").unwrap_or_else(|| "gw2it.db".into());

    if full {
        let (Some(names), Some(terms)) = (options.remove("--names"), options.remove("--terms")) else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let by_hand = match (options.remove("--ask-to"), options.remove("--answers")) {
            (Some(f), None) => Some(full::ByHand::AskTo(f)),
            (None, Some(f)) => Some(full::ByHand::Answers(f)),
            _ => None,
        };
        let max_texts = options.remove("--max-texts").and_then(|n| n.parse().ok()).unwrap_or(200);
        let con = open(&db).unwrap_or_else(|e| {
            eprintln!("cannot open {db}: {e}");
            std::process::exit(1);
        });
        match full::run(&con, &names, &terms, &full::Options { max_texts, dry_run, all, by_hand }) {
            Ok(r) => println!(
                "full: {} words, {} rows: {} as in the hybrid, {} made already, {} stale, {} names written, {} to rewrite, {} asked, {} written, {} refused{}",
                r.words, r.rows, r.same, r.kept, r.stale, r.names, r.to_rewrite, r.asked, r.written, r.refused,
                if dry_run { " (dry run: nothing changed)" } else { "" }
            ),
            Err(e) => {
                eprintln!("full: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if seeds {
        let Some(out) = options.remove("--out") else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        match gw2dat::sync_seeds(&out) {
            Ok((new, total)) => println!("seeds: {new} new, {total} in {out}"),
            Err(e) => {
                eprintln!("seeds: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if gw2dat {
        let (Some(dat), Some(seeds), Some(out)) = (options.remove("--dat"), options.remove("--seeds"), options.remove("--out")) else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let db = std::path::Path::new(&db).exists().then_some(db);
        match gw2dat::run(&dat, &seeds, &out, db.as_deref(), options.remove("--file").as_deref()) {
            Ok(r) => {
                println!(
                    "gw2dat: {} entries ({} read again), {} English files, {} placed in a block; texts: {} plain, {} opened with a seed, \
                     {} encrypted without a seed, {} seeds that do not fit, {} empty",
                    r.scanned, r.rescanned, r.english_files, r.with_block, r.plain, r.seeded, r.no_seed, r.bad_seed, r.empty
                );
                println!(
                    "gw2dat: strs files per language code {:?}; other languages: {} files, {} placed, {} ambiguous, {} texts, {} seeds that do not fit",
                    r.langs, r.other_files, r.other_placed, r.other_ambiguous, r.other_texts, r.other_bad_seed
                );
            }
            Err(e) => {
                eprintln!("gw2dat: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if repair {
        let con = open(&db).unwrap_or_else(|e| {
            eprintln!("cannot open {db}: {e}");
            std::process::exit(1);
        });
        match run_repair(&con, dry_run) {
            Ok(n) => println!("repair: {n} rows{}", if dry_run { " (dry run: nothing changed)" } else { "" }),
            Err(e) => {
                eprintln!("repair: database: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    // The translators of translators.json are no more: the commands that took them say so.
    if (bulk || lt_sync) && options.contains_key("--translators") {
        eprintln!("--translators: {REMOVED}; use --ask-to or --answers");
        std::process::exit(2);
    }
    if bulk {
        let by_hand = match (options.remove("--ask-to"), options.remove("--answers")) {
            (Some(f), None) => bulk::ByHand::AskTo(f),
            (None, Some(f)) => bulk::ByHand::Answers(f),
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        };
        let Some(file) = options.remove("--file") else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let max_texts = options.remove("--max-texts").and_then(|n| n.parse().ok()).unwrap_or(usize::MAX);
        run_bulk(&db, &file, &bulk::Options { max_texts, dry_run, all, by_hand });
        return;
    }
    if lt_sync {
        let Some(glossary) = options.remove("--glossary") else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let max_texts = options.remove("--max-texts").and_then(|n| n.parse().ok()).unwrap_or(lt_sync::MAX_TEXTS);
        // The answers by hand; without, only what needs no model.
        let ask = ["--ask-to", "--answers"].iter().any(|o| options.contains_key(*o)).then(|| model(&mut options));
        lt_sync::command(&db, &glossary, ask.as_deref(), &lt_sync::Options { dry_run, max_texts, from_last });
        return;
    }
    let listen = options.remove("--listen").unwrap_or_else(|| "127.0.0.1:8790".into());
    if options.remove("--translators").is_some() {
        eprintln!("gw2it-server: --translators ignored: {REMOVED}");
    }

    let con = open(&db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    let server = Arc::new(Server::http(&listen).unwrap_or_else(|e| {
        eprintln!("cannot listen on {listen}: {e}");
        std::process::exit(1);
    }));
    eprintln!("gw2it-server: {db} on http://{listen}");

    let secret = sign::secret(&con).unwrap_or_else(|e| {
        eprintln!("cannot read the signing secret: {e}");
        std::process::exit(1);
    });
    // Also a file <db>.frozen next to the database: the service's command line needs root to change.
    let frozen = frozen || std::path::Path::new(&format!("{db}.frozen")).exists();
    if frozen {
        eprintln!("gw2it-server: frozen, the rows and terms the addons send are not written, no /translate");
    }
    let app = Arc::new(App { db: Mutex::new(con), secret, rate: Mutex::new(HashMap::new()), names: Mutex::new(None), frozen });
    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let (server, app) = (server.clone(), app.clone());
            thread::spawn(move || {
                for request in server.incoming_requests() {
                    handle(&app, request);
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
}

/// The `%` signs a translator added or moved and the wrapper (`%str1%%str2%`... `<c=...>`) it moved
/// into the sentence (markup::repair), in every row, hand corrections too: the game cuts the text
/// or shows it broken, the curly quotes, and the plural mark of the adjective that ends a name (plural::fix_trailing).
/// Returns the rows repaired.
fn run_repair(con: &Connection, dry_run: bool) -> rusqlite::Result<usize> {
    let rows: Vec<(i64, String, String, String)> = {
        let mut stmt = con.prepare("SELECT id, hash, en, it FROM translations WHERE instr(it, '%') > 0 OR instr(it, '</c>') > 0 OR instr(it, '[pl:') > 0 OR instr(it, '’') > 0 OR instr(it, '‘') > 0 OR instr(it, '“') > 0 OR instr(it, '”') > 0 OR instr(en, '  ' || char(10)) > 0")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let now = now_ms();
    let mut repaired = 0;
    for (id, hash, en, it) in rows {
        // Plural: only the adjective that ends the name of a text already marked (hand corrections too).
        let new = gw2it_text::plural::fix_trailing(&en, &gw2it_text::markup::repair(&en, &it));
        if new == it {
            if gw2it_text::markup::stray_percent(&it) || gw2it_text::markup::wrap_broken(&en, &it) {
                eprintln!("repair: {id} {it:?}: cannot be repaired");
            }
            continue;
        }
        println!("repair: {id} {it:?} -> {new:?}");
        repaired += 1;
        if dry_run {
            continue;
        }
        con.execute(
            "INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
             SELECT id, hash, it, status, author, at, 'repair', ?3 FROM translations WHERE id = ?1 AND hash = ?2",
            params![id, hash, now],
        )?;
        con.execute(
            "UPDATE translations SET it = ?3, at = ?4, seq = (SELECT coalesce(max(seq), 0) FROM translations) + 1
             WHERE id = ?1 AND hash = ?2 AND it = ?5",
            params![id, hash, new, now, it],
        )?;
    }
    Ok(repaired)
}

/// The model of lt-sync answered by hand (byhand.rs), with --ask-to or --answers.
fn model(options: &mut HashMap<String, String>) -> Box<names::Ask<'static>> {
    let made = match (options.remove("--ask-to"), options.remove("--answers")) {
        (Some(file), None) => byhand::ask_to(&file),
        (None, Some(file)) => byhand::answers(&file),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    made.unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    })
}

/// The texts of a file translated ahead by the agents (bulk.rs).
fn run_bulk(db: &str, file: &str, options: &bulk::Options) {
    let con = open(db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    match bulk::run(&con, file, options) {
        Ok(r) => println!(
            "bulk: {} texts, {} known, {} with new English, {} names, {} copied, {} with nothing to translate, {} translated, {} rejected{}",
            r.read,
            r.known,
            r.changed,
            r.names,
            r.copied,
            r.nothing,
            r.translated,
            r.rejected,
            if options.dry_run { " (dry run: nothing changed)" } else { "" },
        ),
        Err(e) => {
            eprintln!("bulk: {e}");
            std::process::exit(1);
        }
    }
}

fn open(path: &str) -> rusqlite::Result<Connection> {
    let con = Connection::open(path)?;
    con.busy_timeout(Duration::from_secs(5))?;
    con.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    con.execute_batch(SCHEMA)?;
    // Databases from before users.contributor.
    let has_contributor: bool =
        con.query_row("SELECT count(*) FROM pragma_table_info('users') WHERE name = 'contributor'", [], |r| r.get(0))?;
    if !has_contributor {
        con.execute_batch("ALTER TABLE users ADD COLUMN contributor INTEGER NOT NULL DEFAULT 0;")?;
    }
    // Databases from before translations.trust: the hand corrections were all alike.
    let has_trust: bool =
        con.query_row("SELECT count(*) FROM pragma_table_info('translations') WHERE name = 'trust'", [], |r| r.get(0))?;
    if !has_trust {
        con.execute_batch(
            "ALTER TABLE translations ADD COLUMN trust INTEGER NOT NULL DEFAULT 0;
             UPDATE translations SET trust = 2 WHERE status = 'manual';",
        )?;
    }
    con.execute_batch(SQL_OLD_JOBS)?;
    con.execute_batch(names::SCHEMA)?;
    Ok(con)
}

fn handle(app: &App, mut request: Request) {
    let started = Instant::now();
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    let result = match (&method, path.as_str()) {
        (Method::Post, "/register") => register(app, &mut request),
        (Method::Post, "/sync") => sync(app, &mut request),
        // The addons before 0.4.3 still ask: the answer they got from a server without translators.
        (Method::Post, "/translate") => Err(HttpError(503, "no translations on this server")),
        (Method::Get, "/") => Ok(json!({ "service": "gw2it-sync" })),
        _ => Err(HttpError(404, "not found")),
    };
    let (status, body) = match result {
        Ok(body) => (200, body),
        Err(HttpError(status, message)) => (status, json!({ "error": message })),
    };
    if status != 200 || started.elapsed() > Duration::from_millis(500) {
        eprintln!("{method} {path} -> {status} in {} ms", started.elapsed().as_millis());
    }
    let header = Header::from_bytes("Content-Type", "application/json; charset=utf-8").unwrap();
    let _ = request.respond(Response::from_string(body.to_string()).with_status_code(status).with_header(header));
}

/// The names that stay in English, read again every `NAMES_FRESH`.
fn english_names(app: &App, db: &Connection) -> rusqlite::Result<Arc<Names>> {
    let mut cached = app.names.lock().unwrap();
    if let Some((_, names)) = cached.as_ref().filter(|(at, _)| at.elapsed() < NAMES_FRESH) {
        return Ok(names.clone());
    }
    let names = Arc::new(names::load(db)?);
    *cached = Some((Instant::now(), names.clone()));
    Ok(names)
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn limit(app: &App, key: String, per_minute: u32) -> Result<(), HttpError> {
    limit_in(app, key, per_minute, Duration::from_secs(60))
}

/// At most `max` requests per `window` for `key` (counted in memory: a restart resets it).
fn limit_in(app: &App, key: String, max: u32, window: Duration) -> Result<(), HttpError> {
    let mut rate = app.rate.lock().unwrap();
    let now = Instant::now();
    // Forget the expired ones now and then.
    if rate.len() > 10_000 {
        rate.retain(|k, (start, _)| now.duration_since(*start) < if k.starts_with("register-day:") { REGISTER_WINDOW } else { Duration::from_secs(60) });
    }
    let (start, count) = rate.entry(key).or_insert((now, 0));
    if now.duration_since(*start) >= window {
        (*start, *count) = (now, 0);
    }
    *count += 1;
    if *count > max { Err(HttpError(429, "too many requests")) } else { Ok(()) }
}

fn body(request: &mut Request) -> Result<Value, HttpError> {
    let mut text = String::new();
    request
        .as_reader()
        .take(BODY_MAX)
        .read_to_string(&mut text)
        .map_err(|_| HttpError(400, "body is not text"))?;
    serde_json::from_str(&text).map_err(|_| HttpError(400, "body is not JSON"))
}

fn header(request: &Request, name: &'static str) -> Option<String> {
    request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_owned())
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The client's address: the last one in X-Forwarded-For when behind a proxy. The proxy appends the
/// address it saw to what the client sent, so only the last value cannot be forged.
pub(crate) fn client(request: &Request) -> String {
    header(request, "X-Forwarded-For")
        .and_then(|h| h.split(',').next_back().map(|a| a.trim().to_owned()))
        .filter(|a| !a.is_empty())
        .or_else(|| request.remote_addr().map(|a| a.ip().to_string()))
        .unwrap_or_else(|| "?".into())
}

/// POST /register {name} -> {token}: one token per installation, kept in its config.toml.
fn register(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    let ip = client(request);
    limit(app, format!("register:{ip}"), RATE)?;
    // One installation registers once: a few per address and day (a household, a shared network).
    limit_in(app, format!("register-day:{ip}"), REGISTER_PER_DAY, REGISTER_WINDOW)?;
    let input = body(request)?;
    let name: String = input["name"].as_str().unwrap_or("").chars().filter(|c| !c.is_control()).collect();
    let name: String = name.trim().chars().take(32).collect();
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| HttpError(500, "internal error"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    app.db.lock().unwrap().execute(
        "INSERT INTO users (token_hash, name, created) VALUES (?1, ?2, ?3)",
        params![sha256(&token), if name.is_empty() { "anonimo" } else { &name }, now_ms()],
    )?;
    Ok(json!({ "token": token }))
}

/// A row sent by the addon: [id, hash, en, it, status], and for a text made by this server's old
/// /translate [.., signature, the XML it answered] (sign.rs). None if malformed. Its proof is not
/// checked here.
fn valid(row: &Value) -> Option<(Value, Option<(String, String)>)> {
    let (id, hash, en, it, status, proof) = match row.as_array()?.as_slice() {
        [id, hash, en, it, status] => (id, hash, en, it, status, None),
        [id, hash, en, it, status, signature, answered] => {
            let proof = (signature.as_str()?.to_owned(), answered.as_str().filter(|a| a.chars().count() <= TEXT_MAX)?.to_owned());
            (id, hash, en, it, status, Some(proof))
        }
        _ => return None,
    };
    let id = id.as_u64().filter(|&id| id <= u32::MAX as u64)?;
    let hash = hash.as_str().filter(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))?;
    let en = en.as_str().filter(|en| en.chars().count() <= TEXT_MAX)?;
    let it = it.as_str().filter(|it| !it.trim().is_empty() && it.chars().count() <= TEXT_MAX)?;
    let status = status.as_str().filter(|s| *s == "auto" || *s == "manual")?;
    Some((json!([id, hash, en, it, status]), proof))
}

/// The installation behind the request's token.
struct User {
    token_hash: String,
    name: String,
    /// 0 nobody special, 1 may send hand corrections, 2 the owner (users.contributor).
    level: i64,
}

fn user(app: &App, request: &Request) -> Result<User, HttpError> {
    let token = header(request, "Authorization")
        .and_then(|h| h.strip_prefix("Bearer ").map(|t| t.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .ok_or(HttpError(401, "missing token"))?;
    let token_hash = sha256(&token);
    let user: Option<(String, bool, i64)> = app
        .db
        .lock()
        .unwrap()
        .query_row("SELECT name, banned, contributor FROM users WHERE token_hash = ?1", [&token_hash], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    match user {
        None => Err(HttpError(401, "unknown token")),
        Some((_, true, _)) => Err(HttpError(403, "banned")),
        Some((name, false, level)) => Ok(User { token_hash, name, level }),
    }
}

/// POST /sync {since, rows: [[id, hash, en, it, status]...], gsince, glossary, nsince, usince, fsince}
///   -> {rows: [[id, hash, en, it, status, author, seq]...], seq, more, next_s, accepted,
///       glossary, gseq, gmore, names: [name...], nseq, unnames, useq, full: [[id, hash, it, seq]...], fseq, fmore}
fn sync(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    let User { token_hash, name, level } = user(app, request)?;
    limit(app, format!("sync:{token_hash}"), RATE)?;
    let input = body(request)?;
    let mut db = app.db.lock().unwrap();

    let since = input["since"].as_i64().filter(|&s| s > 0).unwrap_or(0);
    let sent = input["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
    let sent = if app.frozen { &[][..] } else { sent };
    if sent.len() > PUSH_MAX {
        return Err(HttpError(413, "too many rows"));
    }
    let mut rows: Vec<Value> = Vec::new();
    let names = english_names(app, &db)?;
    let terms = if sent.is_empty() { Vec::new() } else { glossary::all(&db)? };
    for (mut row, proof) in sent.iter().filter_map(valid) {
        // Anyone can write contributor.json: only the contributors' hand corrections count as such.
        // The sixth element is the author (the sender), the seventh the trust: see schema.sql.
        let trust = match (row[4] == "manual", level) {
            (true, 2..) => 3,
            (true, 1) => 2,
            _ => {
                row[4] = "auto".into();
                // A machine translation this server made, with its proof: it beats the ones
                // the addons make themselves.
                let server_made = proof.is_some_and(|(signature, answered)| {
                    sign::verify(&app.secret, row[2].as_str().unwrap_or(""), row[3].as_str().unwrap_or(""), &signature, &answered)
                });
                i64::from(server_made)
            }
        };
        // A machine translation of a text that is a name that stays in English: the name, at once.
        if trust <= 1
            && let Some(name) = names.whole(row[2].as_str().unwrap_or(""))
        {
            row[3] = name.into();
        }
        // A `%` a translator added cuts the text in the game: repaired in every row.
        let repaired = gw2it_text::markup::repair(row[2].as_str().unwrap_or(""), row[3].as_str().unwrap_or(""));
        row[3] = repaired.into();
        // The glossary's fixes (the contributors' terms, the old consistency job's), at once.
        if trust <= 1 {
            let fixed = gw2it_text::glossary::fix(row[2].as_str().unwrap_or(""), row[3].as_str().unwrap_or(""), &terms);
            row[3] = fixed.into();
        }
        row.as_array_mut().unwrap().extend([Value::Null, trust.into()]);
        rows.push(row);
    }
    if !rows.is_empty() {
        let json = Value::Array(rows.clone()).to_string();
        let now = now_ms();
        let tx = db.transaction()?;
        tx.execute(SQL_HISTORY, params![json, name, now])?;
        tx.execute(SQL_UPSERT, params![json, name, now, 0])?;
        tx.execute(SQL_SAME_HISTORY, params![json, name, now])?;
        tx.execute(SQL_SAME, params![json, name, now, 0])?;
        tx.commit()?;
    }

    let mut stmt = db.prepare_cached(SQL_PULL)?;
    let pulled: Vec<Value> = stmt
        .query_map(params![since, PULL_MAX], |r| {
            Ok(json!([
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?
            ]))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    let seq = pulled.last().and_then(|r| r[6].as_i64()).unwrap_or(since);
    let more = pulled.len() as i64 == PULL_MAX;

    // The glossary: the contributors' terms in, everyone's updates out.
    let sent_terms = input["glossary"].as_array().map(Vec::as_slice).unwrap_or_default();
    if sent_terms.len() > glossary::PUSH_MAX {
        return Err(HttpError(413, "too many terms"));
    }
    if level >= 1 && !app.frozen {
        let trust = if level >= 2 { 3 } else { 2 };
        let now = now_ms();
        let tx = db.transaction()?;
        for term in sent_terms.iter().filter_map(glossary::term_of) {
            glossary::put(&tx, &term, &name, trust, now, 0)?;
        }
        tx.commit()?;
    }
    let gsince = input["gsince"].as_i64().filter(|&s| s > 0).unwrap_or(0);
    let (terms, gseq, gmore) = glossary::pull(&db, gsince)?;
    // The names that stay in English added after `nsince`: the addon gives them as they are. The
    // ones taken out after `usince`, to an addon that asks (older ones do not know them).
    let nsince = input["nsince"].as_i64().unwrap_or(0);
    let (english, nseq) = names::pull(&db, nsince)?;
    let (unnames, useq) = match input["usince"].as_i64() {
        Some(usince) => names::removed(&db, usince)?,
        None => (Vec::new(), 0),
    };
    // The all-Italian variant's rows (full.rs) changed after `fsince`, to an addon that asks (older
    // ones do not know them): [id, hash, it, seq]; an empty `it` is a row taken out.
    let (full_rows, fseq, fmore) = match input["fsince"].as_i64() {
        Some(fsince) => full::pull(&db, fsince.max(0), PULL_MAX)?,
        None => (Vec::new(), 0, false),
    };
    Ok(json!({
        "rows": pulled, "seq": seq, "more": more, "next_s": NEXT_S, "accepted": rows.len(),
        "glossary": terms, "gseq": gseq, "gmore": gmore, "names": english, "nseq": nseq, "unnames": unnames, "useq": useq,
        "full": full_rows, "fseq": fseq, "fmore": fmore,
    }))
}
