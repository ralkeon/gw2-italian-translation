//! Global translations database of Italian Translation, self-hosted: the same API, schema and
//! statements as the Cloudflare Worker in server/ (see server/README.md), on a local SQLite file.
//!
//! Plain HTTP on a local address: a reverse proxy (Apache, Caddy...) in front gives it HTTPS.
//!
//! gw2it-server [--db gw2it.db] [--listen 127.0.0.1:8790]
//!              [--translators translators.json]   (see translate.rs)
//!              [--review-daily 3000]   (texts the background review gives the model per day; 0: off)
//! gw2it-server replicate [--db gw2it.db] --to <spare URL> --key-file <file>   (see replicate.rs)
//! gw2it-server consistency [--db gw2it.db] --translators translators.json [--max-groups 15] [--dry-run] [--include-manual]
//!                                                                         (see consistency.rs)
//! gw2it-server names [--db gw2it.db] [--fetch] [--translators translators.json] [--max-texts 300] [--dry-run]
//!                                                                         (see names.rs)
//! gw2it-server repair [--db gw2it.db] [--dry-run]   (the `%` a translator added, in every row)
//! gw2it-server review [--db gw2it.db] --translators translators.json [--max-texts 200] [--dry-run]
//!                                                                         (see review.rs)

use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use gw2it_text::names::Names;
use gw2it_translate::{Chain, Engine};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};

mod consistency;
mod glossary;
mod names;
mod plurals;
mod replicate;
mod review;
mod sign;
mod translate;

const SCHEMA: &str = include_str!("../../../server/schema.sql");
const SQL_HISTORY: &str = include_str!("../../../server/sql/history.sql");
const SQL_UPSERT: &str = include_str!("../../../server/sql/upsert.sql");
const SQL_PULL: &str = include_str!("../../../server/sql/pull.sql");
const SQL_SAME_HISTORY: &str = include_str!("../../../server/sql/same_history.sql");
const SQL_SAME: &str = include_str!("../../../server/sql/same.sql");

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
/// Texts the background review gives the model per day, by default.
const REVIEW_DAILY: usize = 3000;
/// Between two rounds of the background review; after an error of the model, longer.
const REVIEW_PAUSE: Duration = Duration::from_secs(20);
const REVIEW_BACKOFF: Duration = Duration::from_secs(600);

struct App {
    db: Mutex<Connection>,
    /// For /translate, in order of preference: none, no translations.
    translators: gw2it_translate::Chain,
    /// Signs the translations /translate makes (sign.rs).
    secret: Vec<u8>,
    /// key -> (start of the current minute, requests in it)
    rate: Mutex<HashMap<String, (Instant, u32)>>,
    /// The names that stay in English (names.rs), and when they were read.
    names: Mutex<Option<(Instant, Arc<Names>)>>,
}

/// How long the names read from the database are used before reading them again (the nightly
/// job adds new ones).
const NAMES_FRESH: Duration = Duration::from_secs(600);

struct HttpError(u16, &'static str);

impl From<rusqlite::Error> for HttpError {
    fn from(e: rusqlite::Error) -> Self {
        eprintln!("database: {e}");
        Self(500, "internal error")
    }
}

const USAGE: &str = "usage: gw2it-server [--db gw2it.db] [--listen 127.0.0.1:8790]
                    [--translators translators.json]
       gw2it-server replicate [--db gw2it.db] --to <spare URL> --key-file <file>
       gw2it-server consistency [--db gw2it.db] --translators translators.json [--max-groups 15] [--dry-run] [--include-manual]
       gw2it-server names [--db gw2it.db] [--fetch] [--translators translators.json] [--max-texts 300] [--dry-run]
       gw2it-server review [--db gw2it.db] --translators translators.json [--max-texts 200] [--dry-run]
       gw2it-server plurals [--db gw2it.db] --translators translators.json [--max-texts 200] [--dry-run]
       gw2it-server repair [--db gw2it.db] [--dry-run]";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let replicate = args.first().is_some_and(|a| a == "replicate");
    let consistency = args.first().is_some_and(|a| a == "consistency");
    let english_names = args.first().is_some_and(|a| a == "names");
    let review = args.first().is_some_and(|a| a == "review");
    let plurals = args.first().is_some_and(|a| a == "plurals");
    let repair = args.first().is_some_and(|a| a == "repair");
    if replicate || consistency || english_names || review || plurals || repair {
        args.remove(0);
    }
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let include_manual = args.iter().any(|a| a == "--include-manual");
    let fetch = args.iter().any(|a| a == "--fetch");
    args.retain(|a| a != "--dry-run" && a != "--include-manual" && a != "--fetch");
    let mut options = HashMap::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            (name @ ("--db" | "--listen" | "--to" | "--key-file" | "--translators" | "--max-groups" | "--max-texts" | "--review-daily"), Some(v)) => {
                options.insert(name.to_owned(), v);
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
    }
    let db = options.remove("--db").unwrap_or_else(|| "gw2it.db".into());

    if replicate {
        let (Some(to), Some(key_file)) = (options.remove("--to"), options.remove("--key-file")) else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let key = std::fs::read_to_string(&key_file).unwrap_or_else(|e| {
            eprintln!("cannot read {key_file}: {e}");
            std::process::exit(1);
        });
        let con = open(&db).unwrap_or_else(|e| {
            eprintln!("cannot open {db}: {e}");
            std::process::exit(1);
        });
        if let Err(e) = replicate::run(con, to.trim_end_matches('/'), key.trim()) {
            eprintln!("replicate: {e}");
            std::process::exit(1);
        }
        return;
    }
    if consistency {
        let Some(file) = options.remove("--translators") else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let max_groups = options.remove("--max-groups").and_then(|n| n.parse().ok()).unwrap_or(15);
        run_consistency(&db, &file, max_groups, dry_run, include_manual);
        return;
    }
    if english_names {
        let max_texts = options.remove("--max-texts").and_then(|n| n.parse().ok()).unwrap_or(300);
        run_names(&db, fetch, options.remove("--translators").as_deref(), max_texts, dry_run);
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
    if plurals {
        let Some(file) = options.remove("--translators") else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let max_texts = options.remove("--max-texts").and_then(|n| n.parse().ok()).unwrap_or(200);
        run_plurals(&db, &file, max_texts, dry_run);
        return;
    }
    if review {
        let Some(file) = options.remove("--translators") else {
            eprintln!("{USAGE}");
            std::process::exit(2);
        };
        let max_texts = options.remove("--max-texts").and_then(|n| n.parse().ok()).unwrap_or(200);
        run_review(&db, &file, max_texts, dry_run);
        return;
    }
    let listen = options.remove("--listen").unwrap_or_else(|| "127.0.0.1:8790".into());
    let translators_file = options.remove("--translators");
    let engines = translators_file.as_deref().map(translators).unwrap_or_default();
    let review_daily = options.remove("--review-daily").and_then(|n| n.parse().ok()).unwrap_or(REVIEW_DAILY);
    // Its own pauses: a model out of quota for the review is not paused for /translate.
    let mut reviewers = Chain::default();
    reviewers.set(engines.clone());
    let names: Vec<String> = engines.iter().map(Engine::name).collect();
    let mut translators = Chain::default();
    translators.set(engines);

    let con = open(&db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    let server = Arc::new(Server::http(&listen).unwrap_or_else(|e| {
        eprintln!("cannot listen on {listen}: {e}");
        std::process::exit(1);
    }));
    eprintln!("gw2it-server: {db} on http://{listen}, translators: {}", if names.is_empty() { "none".into() } else { names.join(", ") });

    let secret = sign::secret(&con).unwrap_or_else(|e| {
        eprintln!("cannot read the signing secret: {e}");
        std::process::exit(1);
    });
    let app = Arc::new(App { db: Mutex::new(con), translators, secret, rate: Mutex::new(HashMap::new()), names: Mutex::new(None) });
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
    if review_daily > 0 && !reviewers.is_empty() {
        let app = app.clone();
        thread::spawn(move || review_in_background(&app, &reviewers, review_daily));
    }
    for w in workers {
        let _ = w.join();
    }
}

/// The nightly consistency job (consistency.rs): the language models of translators.json settle the
/// English words that the translations render in more than one way.
fn run_consistency(db: &str, translators_file: &str, max_groups: usize, dry_run: bool, include_manual: bool) {
    let mut chain = Chain::default();
    chain.set(translators(translators_file));
    let con = open(db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    let ask = |system: &str, user: &str| {
        chain.ask(system, user).map_err(|e| match e {
            gw2it_translate::Error::BadRequest(why) => consistency::Failure::Unusable(why),
            e => consistency::Failure::Stop(e.to_string()),
        })
    };
    match consistency::run(&con, &ask, &consistency::Options { max_groups, dry_run, include_manual }) {
        Ok(report) => {
            println!(
                "consistency: {} groups, {} words classified ({} names), {} looked at, {} consistent, {} texts rewritten, {} terms{}{}",
                report.groups,
                report.classified,
                report.names,
                report.checked,
                report.consistent,
                report.rewritten,
                report.terms,
                if dry_run { " (dry run: nothing changed)" } else { "" },
                report.stopped.map(|why| format!("; stopped: {why}")).unwrap_or_default(),
            );
        }
        Err(e) => {
            eprintln!("consistency: database: {e}");
            std::process::exit(1);
        }
    }
}

/// The names that stay in English (names.rs): the official API's list with `fetch`, then the
/// machine translations that lost one, fixed (by the language models of translators.json, if
/// given; without, only the texts that are the name).
fn run_names(db: &str, fetch: bool, translators_file: Option<&str>, max_texts: usize, dry_run: bool) {
    let con = open(db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    if fetch {
        match names::fetch(&con) {
            Ok(added) => println!("names: {added} new from the API"),
            Err(e) => {
                eprintln!("names: {e}");
                std::process::exit(1);
            }
        }
    }
    let mut chain = Chain::default();
    chain.set(translators_file.map(translators).unwrap_or_default());
    let ask = |system: &str, user: &str| chain.ask(system, user).map_err(|e| e.to_string());
    let ask: Option<&names::Ask> = if chain.is_empty() { None } else { Some(&ask) };
    match names::run(&con, ask, &names::Options { dry_run, max_texts }) {
        Ok(r) => println!(
            "names: {} texts that are a name, {} with one inside, {} rewritten, {} failed, {} not the mechanic, {} hand corrections without their name{}{}",
            r.whole,
            r.inside,
            r.rewritten,
            r.failed,
            r.skipped,
            r.manual,
            if dry_run { " (dry run: nothing changed)" } else { "" },
            r.stopped.map(|why| format!("; stopped: {why}")).unwrap_or_default(),
        ),
        Err(e) => {
            eprintln!("names: database: {e}");
            std::process::exit(1);
        }
    }
}

/// The `%` signs a translator added or moved and the wrapper (`%str1%%str2%`... `<c=...>`) it moved
/// into the sentence (markup::repair), in every row, hand corrections too: the game cuts the text
/// or shows it broken, the curly quotes, and the plural mark of the adjective that ends a name (plural::fix_trailing).
/// Returns the rows repaired.
fn run_repair(con: &Connection, dry_run: bool) -> rusqlite::Result<usize> {
    let rows: Vec<(i64, String, String, String)> = {
        let mut stmt = con.prepare("SELECT id, hash, en, it FROM translations WHERE instr(it, '%') > 0 OR instr(it, '</c>') > 0 OR instr(it, '[pl:') > 0 OR instr(it, '’') > 0 OR instr(it, '‘') > 0 OR instr(it, '“') > 0 OR instr(it, '”') > 0")?;
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

/// The review (review.rs) by hand: up to `max_texts` texts not reviewed yet, the newest first.
fn run_review(db: &str, translators_file: &str, max_texts: usize, dry_run: bool) {
    let mut chain = Chain::default();
    chain.set(translators(translators_file));
    let con = open(db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    let ask = |system: &str, user: &str| chain.ask(system, user).map_err(|e| e.to_string());
    match review::run(&con, &ask, max_texts, dry_run) {
        Ok(r) => println!(
            "review: {} texts, {} right, {} fixed ({} rows), {} fixes refused{}{}",
            r.asked,
            r.ok,
            r.fixed,
            r.rows,
            r.refused,
            if dry_run { " (dry run: nothing changed)" } else { "" },
            r.stopped.map(|why| format!("; stopped: {why}")).unwrap_or_default(),
        ),
        Err(e) => {
            eprintln!("review: database: {e}");
            std::process::exit(1);
        }
    }
}

/// The plurals check (plurals.rs) by hand or from cron: up to `max_texts` texts not checked yet.
fn run_plurals(db: &str, translators_file: &str, max_texts: usize, dry_run: bool) {
    let mut chain = Chain::default();
    chain.set(translators(translators_file));
    let con = open(db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    let ask = |system: &str, user: &str| chain.ask(system, user).map_err(|e| e.to_string());
    match plurals::run(&con, &ask, max_texts, dry_run) {
        Ok(r) => println!(
            "plurals: {} texts, {} right, {} fixed ({} rows), {} fixes refused{}{}",
            r.asked,
            r.ok,
            r.fixed,
            r.rows,
            r.refused,
            if dry_run { " (dry run: nothing changed)" } else { "" },
            r.stopped.map(|why| format!("; stopped: {why}")).unwrap_or_default(),
        ),
        Err(e) => {
            eprintln!("plurals: database: {e}");
            std::process::exit(1);
        }
    }
}

/// The review in the server: the texts that arrive, a batch every REVIEW_PAUSE, up to `daily`
/// a day; the database is not held while the model thinks.
fn review_in_background(app: &App, chain: &Chain, daily: usize) {
    let ask = |system: &str, user: &str| chain.ask(system, user).map_err(|e| e.to_string());
    loop {
        thread::sleep(REVIEW_PAUSE);
        let todo = {
            let db = app.db.lock().unwrap();
            let left = review::used_today(&db).map(|used| daily.saturating_sub(used));
            match left.and_then(|left| Ok((review::pending(&db, review::BATCH.min(left))?, english_names(app, &db)?, glossary::all(&db)?))) {
                Ok(todo) => todo,
                Err(e) => {
                    eprintln!("review: database: {e}");
                    continue;
                }
            }
        };
        let (texts, names, terms) = todo;
        if texts.is_empty() {
            continue;
        }
        let verdicts = match review::ask(&ask, &names, &terms, &texts) {
            Ok(v) => v,
            Err(why) => {
                eprintln!("review: {why}; again in {} s", REVIEW_BACKOFF.as_secs());
                thread::sleep(REVIEW_BACKOFF);
                continue;
            }
        };
        let db = app.db.lock().unwrap();
        let written = review::count(&db, texts.len()).and_then(|()| {
            let mut fixed = 0;
            for (text, verdict) in texts.iter().zip(&verdicts) {
                match verdict {
                    Some(verdict @ review::Verdict::Fixed { it, why }) => {
                        eprintln!("review: {:?} -> {:?} ({why})", text.it, it);
                        fixed += review::apply(&db, text, verdict)?;
                    }
                    Some(verdict) => {
                        review::apply(&db, text, verdict)?;
                    }
                    None => {}
                }
            }
            Ok(fixed)
        });
        if let Err(e) = written {
            eprintln!("review: database: {e}");
        }
    }
}

/// translators.json: the translators of /translate in order of preference, with their keys
/// (Engine::from_json in gw2it-translate). Readable by this user only.
fn translators(file: &str) -> Vec<Engine> {
    let fail = |e: String| -> ! {
        eprintln!("{file}: {e}");
        std::process::exit(1);
    };
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| fail(e.to_string()));
    let list: Value = serde_json::from_str(&text).unwrap_or_else(|e| fail(e.to_string()));
    let mut engines = Vec::new();
    for entry in list.as_array().unwrap_or_else(|| fail("not a JSON array".into())) {
        engines.extend(Engine::from_json(entry).unwrap_or_else(|e| fail(e)));
    }
    engines
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
    con.execute_batch(translate::SCHEMA)?;
    con.execute_batch(consistency::SCHEMA)?;
    con.execute_batch(names::SCHEMA)?;
    con.execute_batch(review::SCHEMA)?;
    con.execute_batch(plurals::SCHEMA)?;
    Ok(con)
}

fn handle(app: &App, mut request: Request) {
    let started = Instant::now();
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    let result = match (&method, path.as_str()) {
        (Method::Post, "/register") => register(app, &mut request),
        (Method::Post, "/sync") => sync(app, &mut request),
        (Method::Post, "/translate") => translate::handle(app, &mut request),
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

/// A row sent by the addon: [id, hash, en, it, status], and for a text made by this server's
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

/// POST /sync {since, rows: [[id, hash, en, it, status]...], gsince, glossary, nsince}
///   -> {rows: [[id, hash, en, it, status, author, seq]...], seq, more, next_s, accepted,
///       glossary, gseq, gmore, names: [name...], nseq}
fn sync(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    let User { token_hash, name, level } = user(app, request)?;
    limit(app, format!("sync:{token_hash}"), RATE)?;
    let input = body(request)?;
    let mut db = app.db.lock().unwrap();

    let since = input["since"].as_i64().filter(|&s| s > 0).unwrap_or(0);
    let sent = input["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
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
        // The glossary's fixes (the contributors' terms, the consistency job's), at once; the
        // language model's review follows in the background (review.rs).
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
    if level >= 1 {
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
    // The names that stay in English added after `nsince`: the addon gives them as they are.
    let nsince = input["nsince"].as_i64().unwrap_or(0);
    let (english, nseq) = names::pull(&db, nsince)?;
    Ok(json!({
        "rows": pulled, "seq": seq, "more": more, "next_s": NEXT_S, "accepted": rows.len(),
        "glossary": terms, "gseq": gseq, "gmore": gmore, "names": english, "nseq": nseq,
    }))
}
