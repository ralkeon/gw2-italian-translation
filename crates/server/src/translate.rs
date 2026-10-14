//! POST /translate {texts: [xml...], neighbours} -> {translations: [xml...]}, with a sync token:
//! DeepL with the server's key, for the players who have none. The addon prepares the texts and
//! restores the markup (crates/addon/src/markup.rs) as with its own key, then the result reaches
//! everyone through /sync like any other translation.
//!
//! Each player has a daily budget of characters, so that a single token cannot use up the key.
//! Errors keep DeepL's status codes (456 quota, 429, 400), which the addon already handles.

use std::{sync::LazyLock, time::Duration};

use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tiny_http::Request;
use ureq::Agent;

use crate::{App, HttpError, body, limit, now_ms, user};

/// Characters a player can have translated per day (UTC).
const DAILY_CHARS: i64 = 30_000;
const TEXTS_MAX: usize = 50;
const TEXT_MAX: usize = 4000;
const NEIGHBOURS_MAX: usize = 6000;
/// Requests per minute, per token: a burst of new texts arrives in batches of up to 50.
const RATE: u32 = 60;

/// The same as the addon's (crates/addon/src/deepl.rs): keep them alike.
const CONTEXT: &str = "Guild Wars 2, a fantasy MMORPG: in-game texts such as item, skill and \
    title names, tooltips, achievements, dialogue and user interface labels.";

/// Shorter than the addon's own timeout (15 s), so that it gets our answer.
static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into()
});

pub const SCHEMA: &str = "
-- Characters translated with the server's DeepL key, per player and day (days since 1970, UTC).
CREATE TABLE IF NOT EXISTS usage (
    day   INTEGER NOT NULL,
    user  TEXT    NOT NULL,          -- users.token_hash
    chars INTEGER NOT NULL,
    PRIMARY KEY (day, user)
);";

fn today() -> i64 {
    now_ms() / 86_400_000
}

pub fn handle(app: &App, request: &mut Request) -> Result<Value, HttpError> {
    let Some(key) = app.deepl_key.as_deref() else {
        return Err(HttpError(503, "no translations on this server"));
    };
    let (token_hash, _) = user(app, request)?;
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

    let used = used(&app.db.lock().unwrap(), &token_hash)?;
    if used + chars > DAILY_CHARS {
        return Err(HttpError(456, "daily limit"));
    }

    let translations = deepl(key, &texts, &neighbours)?;
    app.db.lock().unwrap().execute(
        "INSERT INTO usage (day, user, chars) VALUES (?1, ?2, ?3)
         ON CONFLICT (day, user) DO UPDATE SET chars = chars + excluded.chars",
        params![today(), token_hash, chars],
    )?;
    Ok(json!({ "translations": translations }))
}

fn used(db: &Connection, token_hash: &str) -> rusqlite::Result<i64> {
    db.query_row("SELECT coalesce(sum(chars), 0) FROM usage WHERE day = ?1 AND user = ?2", params![today(), token_hash], |r| r.get(0))
}

fn deepl(key: &str, texts: &[&str], neighbours: &str) -> Result<Vec<String>, HttpError> {
    let base = if key.ends_with(":fx") { "https://api-free.deepl.com/v2" } else { "https://api.deepl.com/v2" };
    let body = json!({
        "text": texts,
        "source_lang": "EN",
        "target_lang": "IT",
        "tag_handling": "xml",
        "formality": "prefer_less",
        "context": format!("{CONTEXT}\nTexts shown together on screen:\n{neighbours}"),
    });
    let mut response = AGENT
        .post(format!("{base}/translate"))
        .header("Authorization", format!("DeepL-Auth-Key {key}"))
        .send_json(&body)
        .map_err(|e| {
            eprintln!("deepl: {e}");
            HttpError(502, "translator unreachable")
        })?;
    let status = response.status().as_u16();
    if status != 200 {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        eprintln!("deepl: HTTP {status} {}", text.chars().take(300).collect::<String>());
        return Err(match status {
            456 => HttpError(456, "quota exceeded"),
            429 => HttpError(429, "too many requests"),
            400 => HttpError(400, "texts refused"),
            _ => HttpError(503, "translator unavailable"),
        });
    }
    let reply: Value = response.body_mut().read_json().map_err(|_| HttpError(502, "bad answer from the translator"))?;
    let translations: Vec<String> = reply["translations"]
        .as_array()
        .map(|t| t.iter().filter_map(|t| t["text"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if translations.len() != texts.len() {
        return Err(HttpError(502, "bad answer from the translator"));
    }
    Ok(translations)
}
