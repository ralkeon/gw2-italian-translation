//! Global translations database of Italian Translation, self-hosted: the same API, schema and
//! statements as the Cloudflare Worker in server/ (see server/README.md), on a local SQLite file.
//!
//! Plain HTTP on a local address: a reverse proxy (Apache, Caddy...) in front gives it HTTPS.
//!
//! gw2it-server [--db gw2it.db] [--listen 127.0.0.1:8790]
//! gw2it-server replicate [--db gw2it.db] --to <spare URL> --key-file <file>   (see replicate.rs)

use std::{
    collections::HashMap,
    io::Read,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};

mod replicate;

const SCHEMA: &str = include_str!("../../../server/schema.sql");
const SQL_HISTORY: &str = include_str!("../../../server/sql/history.sql");
const SQL_UPSERT: &str = include_str!("../../../server/sql/upsert.sql");
const SQL_PULL: &str = include_str!("../../../server/sql/pull.sql");

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
const THREADS: usize = 4;

struct App {
    db: Mutex<Connection>,
    /// key -> (start of the current minute, requests in it)
    rate: Mutex<HashMap<String, (Instant, u32)>>,
}

struct HttpError(u16, &'static str);

impl From<rusqlite::Error> for HttpError {
    fn from(e: rusqlite::Error) -> Self {
        eprintln!("database: {e}");
        Self(500, "internal error")
    }
}

const USAGE: &str = "usage: gw2it-server [--db gw2it.db] [--listen 127.0.0.1:8790]
       gw2it-server replicate [--db gw2it.db] --to <spare URL> --key-file <file>";

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let replicate = args.first().is_some_and(|a| a == "replicate");
    if replicate {
        args.remove(0);
    }
    let mut options = HashMap::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            (name @ ("--db" | "--listen" | "--to" | "--key-file"), Some(v)) => {
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
    let listen = options.remove("--listen").unwrap_or_else(|| "127.0.0.1:8790".into());

    let con = open(&db).unwrap_or_else(|e| {
        eprintln!("cannot open {db}: {e}");
        std::process::exit(1);
    });
    let server = Arc::new(Server::http(&listen).unwrap_or_else(|e| {
        eprintln!("cannot listen on {listen}: {e}");
        std::process::exit(1);
    }));
    eprintln!("gw2it-server: {db} on http://{listen}");

    let app = Arc::new(App { db: Mutex::new(con), rate: Mutex::new(HashMap::new()) });
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

fn open(path: &str) -> rusqlite::Result<Connection> {
    let con = Connection::open(path)?;
    con.busy_timeout(Duration::from_secs(5))?;
    con.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
    con.execute_batch(SCHEMA)?;
    Ok(con)
}

fn handle(app: &App, mut request: Request) {
    let started = Instant::now();
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    let result = match (&method, path.as_str()) {
        (Method::Post, "/register") => register(app, &mut request),
        (Method::Post, "/sync") => sync(app, &mut request),
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

pub(crate) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn limit(app: &App, key: String) -> Result<(), HttpError> {
    let mut rate = app.rate.lock().unwrap();
    let now = Instant::now();
    // Forget the idle ones now and then.
    if rate.len() > 10_000 {
        rate.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
    }
    let (start, count) = rate.entry(key).or_insert((now, 0));
    if now.duration_since(*start) >= Duration::from_secs(60) {
        (*start, *count) = (now, 0);
    }
    *count += 1;
    if *count > RATE { Err(HttpError(429, "too many requests")) } else { Ok(()) }
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

/// The client's address: the first one in X-Forwarded-For when behind a proxy.
fn client(request: &Request) -> String {
    header(request, "X-Forwarded-For")
        .and_then(|h| h.split(',').next().map(|a| a.trim().to_owned()))
        .or_else(|| request.remote_addr().map(|a| a.ip().to_string()))
        .unwrap_or_else(|| "?".into())
}

/// POST /register {name} -> {token}: one token per installation, kept in its config.toml.
fn register(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    limit(app, format!("register:{}", client(request)))?;
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

/// A row sent by the addon: [id, hash, en, it, status]. None if malformed.
fn valid(row: &Value) -> Option<Value> {
    let [id, hash, en, it, status] = row.as_array()?.as_slice() else {
        return None;
    };
    let id = id.as_u64().filter(|&id| id <= u32::MAX as u64)?;
    let hash = hash.as_str().filter(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))?;
    let en = en.as_str().filter(|en| en.chars().count() <= TEXT_MAX)?;
    let it = it.as_str().filter(|it| !it.trim().is_empty() && it.chars().count() <= TEXT_MAX)?;
    let status = status.as_str().filter(|s| *s == "auto" || *s == "manual")?;
    Some(json!([id, hash, en, it, status]))
}

/// POST /sync {since, rows: [[id, hash, en, it, status]...]}
///   -> {rows: [[id, hash, en, it, status, author, seq]...], seq, more, next_s, accepted}
fn sync(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    let token = header(request, "Authorization")
        .and_then(|h| h.strip_prefix("Bearer ").map(|t| t.trim().to_owned()))
        .filter(|t| !t.is_empty())
        .ok_or(HttpError(401, "missing token"))?;
    limit(app, format!("sync:{token}"))?;
    let input = body(request)?;

    let mut db = app.db.lock().unwrap();
    let user: Option<(String, bool)> = db
        .query_row("SELECT name, banned FROM users WHERE token_hash = ?1", [sha256(&token)], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let name = match user {
        None => return Err(HttpError(401, "unknown token")),
        Some((_, true)) => return Err(HttpError(403, "banned")),
        Some((name, false)) => name,
    };

    let since = input["since"].as_i64().filter(|&s| s > 0).unwrap_or(0);
    let sent = input["rows"].as_array().map(Vec::as_slice).unwrap_or_default();
    if sent.len() > PUSH_MAX {
        return Err(HttpError(413, "too many rows"));
    }
    let rows: Vec<Value> = sent.iter().filter_map(valid).collect();
    if !rows.is_empty() {
        let json = Value::Array(rows.clone()).to_string();
        let now = now_ms();
        let tx = db.transaction()?;
        tx.execute(SQL_HISTORY, params![json, name, now])?;
        tx.execute(SQL_UPSERT, params![json, name, now, 0])?;
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
    let seq = pulled.last().and_then(|r| r[6].as_i64()).unwrap_or(since);
    let more = pulled.len() as i64 == PULL_MAX;
    Ok(json!({ "rows": pulled, "seq": seq, "more": more, "next_s": NEXT_S, "accepted": rows.len() }))
}
