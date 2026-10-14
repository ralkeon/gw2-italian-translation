//! POST /translate {texts: [xml...], neighbours} -> {translations: [xml...], by}, with a sync
//! token: the server's own translators (gw2it-translate: DeepL, then Gemini while DeepL is out of
//! quota or does not answer) for the players who have no keys. The addon prepares the texts and
//! restores the markup (crates/addon/src/markup.rs) as with its own keys, then the result reaches
//! everyone through /sync like any other translation.
//!
//! Each player has a daily budget of characters, so that a single token cannot use up the keys.
//! Errors keep DeepL's status codes (456 quota, 429, 400), which the addon already handles.

use gw2it_translate::Error;
use rusqlite::params;
use serde_json::{Value, json};
use tiny_http::Request;

use crate::{App, HttpError, body, client, limit, now_ms, user};

/// Characters a player can have translated per day (UTC).
const DAILY_CHARS: i64 = 30_000;
/// Per address and day: a few tokens from one place cannot multiply the player's budget.
const DAILY_CHARS_IP: i64 = 60_000;
/// All players together, per day: whatever the number of tokens, the keys' quota is safe.
const DAILY_CHARS_ALL: i64 = 300_000;
const TEXTS_MAX: usize = 50;
const TEXT_MAX: usize = 4000;
const NEIGHBOURS_MAX: usize = 6000;
/// Requests per minute, per token: a burst of new texts arrives in batches of up to 50.
const RATE: u32 = 60;

pub const SCHEMA: &str = "
-- Characters translated with the server's keys, per player and day (days since 1970, UTC).
CREATE TABLE IF NOT EXISTS usage (
    day   INTEGER NOT NULL,
    user  TEXT    NOT NULL,          -- users.token_hash
    chars INTEGER NOT NULL,
    PRIMARY KEY (day, user)
);
-- The same, per client address.
CREATE TABLE IF NOT EXISTS usage_ip (
    day   INTEGER NOT NULL,
    ip    TEXT    NOT NULL,
    chars INTEGER NOT NULL,
    PRIMARY KEY (day, ip)
);";

fn today() -> i64 {
    now_ms() / 86_400_000
}

pub fn handle(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    if app.translators.is_empty() {
        return Err(HttpError(503, "no translations on this server"));
    }
    let token_hash = user(app, request)?.token_hash;
    limit(app, format!("translate:{token_hash}"), RATE)?;
    let input = body(request)?;

    let texts: Vec<&str> = input["texts"]
        .as_array()
        .filter(|t| !t.is_empty() && t.len() <= TEXTS_MAX)
        .ok_or(HttpError(400, "texts: 1 to 50"))?
        .iter()
        .map(|t| t.as_str().filter(|t| t.chars().count() <= TEXT_MAX))
        .collect::<Option<_>>()
        .ok_or(HttpError(400, "bad text"))?;
    let neighbours: String = input["neighbours"].as_str().unwrap_or("").chars().take(NEIGHBOURS_MAX).collect();
    let chars: i64 = texts.iter().map(|t| t.chars().count() as i64).sum();

    let ip = client(request);
    {
        let db = app.db.lock().unwrap();
        let day = today();
        let used = |sql: &str, key: Option<&str>| -> rusqlite::Result<i64> {
            match key {
                Some(key) => db.query_row(sql, params![day, key], |r| r.get(0)),
                None => db.query_row(sql, params![day], |r| r.get(0)),
            }
        };
        if used("SELECT coalesce(sum(chars), 0) FROM usage WHERE day = ?1 AND user = ?2", Some(&token_hash))? + chars > DAILY_CHARS
            || used("SELECT coalesce(sum(chars), 0) FROM usage_ip WHERE day = ?1 AND ip = ?2", Some(&ip))? + chars > DAILY_CHARS_IP
        {
            return Err(HttpError(456, "daily limit"));
        }
        if used("SELECT coalesce(sum(chars), 0) FROM usage WHERE day = ?1", None)? + chars > DAILY_CHARS_ALL {
            eprintln!("translate: global daily cap reached");
            return Err(HttpError(456, "daily limit"));
        }
    }

    let translated = app.translators.translate(&texts, &neighbours).map_err(|e| {
        eprintln!("translate: {e}");
        match e {
            Error::QuotaExceeded => HttpError(456, "quota exceeded"),
            Error::TooManyRequests => HttpError(429, "too many requests"),
            Error::BadRequest(_) => HttpError(400, "texts refused"),
            Error::Forbidden | Error::Other(_) | Error::Paused => HttpError(503, "translators unavailable"),
        }
    })?;
    if translated.by != "deepl" {
        eprintln!("translate: {} texts by {}", texts.len(), translated.by);
    }
    let db = app.db.lock().unwrap();
    db.execute(
        "INSERT INTO usage (day, user, chars) VALUES (?1, ?2, ?3)
         ON CONFLICT (day, user) DO UPDATE SET chars = chars + excluded.chars",
        params![today(), token_hash, chars],
    )?;
    db.execute(
        "INSERT INTO usage_ip (day, ip, chars) VALUES (?1, ?2, ?3)
         ON CONFLICT (day, ip) DO UPDATE SET chars = chars + excluded.chars",
        params![today(), ip, chars],
    )?;
    Ok(json!({ "translations": translated.texts, "by": translated.by }))
}
