//! Daily copy between this server (the main one) and the spare (the Cloudflare Worker in server/),
//! run by cron: gw2it-server replicate --to https://... --key-file replica.key
//!
//! 1. Back: what the addons wrote on the spare while this server did not answer (its seq start at
//!    SEQ_FLOOR), the users that registered there and its history, merged here with the usual
//!    rules and their own authors.
//! 2. Forth: what changed here since the last run (seq > sent_seq), as it is here, and all the
//!    users. The first call drops on the spare what step 1 took back.
//!
//! Afterwards the spare holds this server's rows with this server's seq: an addon that falls back
//! to it carries on from the seq it had here.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::time::Duration;
use ureq::Agent;

use crate::{SQL_HISTORY, SQL_UPSERT, now_ms};

/// Rows per call: the spare accepts up to 300 in one D1 statement.
const CHUNK: usize = 300;
/// Users registered on the spare are looked for this far before the last run.
const MARGIN_MS: i64 = 3_600_000;

pub fn run(mut con: Connection, to: &str, key: &str) -> Result<(), String> {
    let db = |e: rusqlite::Error| format!("database: {e}");
    con.execute_batch("CREATE TABLE IF NOT EXISTS replica_state (k TEXT PRIMARY KEY, v INTEGER NOT NULL);")
        .map_err(db)?;
    let started = now_ms();
    let sent_at = state(&con, "sent_at").map_err(db)?;
    let agent: Agent = Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .build()
        .into();

    // 1. Back.
    let (mut back, mut users_back, mut history_back) = (0, 0, 0);
    let mut after = 0i64;
    loop {
        let reply = post(&agent, to, key, "/replica/export", &json!({ "after": after, "since": sent_at - MARGIN_MS }))?;
        let rows = list(&reply, "translations");
        let tx = con.transaction().map_err(db)?;
        if after == 0 {
            for u in list(&reply, "users") {
                users_back += tx
                    .execute(
                        "INSERT OR IGNORE INTO users (token_hash, name, created, banned, contributor)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![u[0].as_str(), u[1].as_str(), u[2].as_i64(), u[3].as_i64(), u[4].as_i64().unwrap_or(0)],
                    )
                    .map_err(db)?;
            }
            for h in list(&reply, "history") {
                tx.execute(
                    "INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        h[0].as_i64(),
                        h[1].as_str(),
                        h[2].as_str(),
                        h[3].as_str(),
                        h[4].as_str(),
                        h[5].as_i64(),
                        h[6].as_str(),
                        h[7].as_i64()
                    ],
                )
                .map_err(db)?;
                history_back += 1;
            }
        }
        if !rows.is_empty() {
            // [id, hash, en, it, status, author]: the statements of /sync, with each row's author.
            let merged = Value::Array(rows.iter().map(|r| json!([r[0], r[1], r[2], r[3], r[4], r[5]])).collect()).to_string();
            let now = now_ms();
            tx.execute(SQL_HISTORY, params![merged, "replica", now]).map_err(db)?;
            tx.execute(SQL_UPSERT, params![merged, "replica", now, 0]).map_err(db)?;
            back += rows.len();
            after = rows.last().and_then(|r| r[7].as_i64()).unwrap_or(after);
        }
        tx.commit().map_err(db)?;
        if rows.len() < CHUNK {
            break;
        }
    }

    // 2. Forth.
    let users: Vec<Value> = {
        let mut stmt = con.prepare("SELECT token_hash, name, created, banned, contributor FROM users").map_err(db)?;
        stmt.query_map([], |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?
            ]))
        })
        .map_err(db)?
        .collect::<rusqlite::Result<_>>()
        .map_err(db)?
    };
    let mut sent = 0;
    let mut first = true;
    let mut last = state(&con, "sent_seq").map_err(db)?;
    loop {
        let rows: Vec<Value> = {
            let mut stmt = con
                .prepare_cached(
                    "SELECT id, hash, en, it, status, author, at, seq FROM translations
                     WHERE seq > ?1 ORDER BY seq LIMIT ?2",
                )
                .map_err(db)?;
            stmt.query_map(params![last, CHUNK as i64], |r| {
                Ok(json!([
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?
                ]))
            })
            .map_err(db)?
            .collect::<rusqlite::Result<_>>()
            .map_err(db)?
        };
        let body = json!({
            "clear": first,
            "translations": rows,
            "users": if first { users.clone() } else { Vec::new() },
        });
        post(&agent, to, key, "/replica/import", &body)?;
        first = false;
        sent += rows.len();
        if let Some(seq) = rows.last().and_then(|r| r[7].as_i64()) {
            last = seq;
            set_state(&con, "sent_seq", last).map_err(db)?;
        }
        if rows.len() < CHUNK {
            break;
        }
    }
    set_state(&con, "sent_at", started).map_err(db)?;
    println!(
        "replicate: back {back} translations, {users_back} users, {history_back} history; \
         forth {sent} translations (up to seq {last}), {} users",
        users.len()
    );
    Ok(())
}

fn state(con: &Connection, k: &str) -> rusqlite::Result<i64> {
    Ok(con.query_row("SELECT v FROM replica_state WHERE k = ?1", [k], |r| r.get(0)).optional()?.unwrap_or(0))
}

fn set_state(con: &Connection, k: &str, v: i64) -> rusqlite::Result<()> {
    con.execute("INSERT OR REPLACE INTO replica_state (k, v) VALUES (?1, ?2)", params![k, v])?;
    Ok(())
}

fn list<'a>(reply: &'a Value, name: &str) -> &'a [Value] {
    reply[name].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn post(agent: &Agent, to: &str, key: &str, path: &str, body: &Value) -> Result<Value, String> {
    let mut response = agent
        .post(format!("{to}{path}"))
        .header("X-Replica-Key", key)
        .send_json(body)
        .map_err(|e| format!("{path}: {e}"))?;
    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().map_err(|e| format!("{path}: {e}"))?;
    if status != 200 {
        return Err(format!("{path}: HTTP {status} {}", text.chars().take(200).collect::<String>()));
    }
    serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))
}
