//! DeepL API client (https://developers.deepl.com/api-reference/translate).

use std::{sync::LazyLock, time::Duration};

use serde::Deserialize;
use serde_json::json;
use ureq::Agent;

/// Fixed context for every request: steers the translation, not translated nor billed.
const CONTEXT: &str = "Guild Wars 2, a fantasy MMORPG: in-game texts such as item, skill and \
    title names, tooltips, achievements, dialogue and user interface labels.";

/// Short enough that unloading the addon (which waits for a request in flight) stays quick.
static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .into()
});

pub enum Error {
    /// 456: monthly character quota used up.
    QuotaExceeded,
    /// 429: too many requests, retry later.
    TooManyRequests,
    /// 403: wrong key.
    Forbidden,
    /// 400: DeepL refused these texts; retrying them would fail again.
    BadRequest(String),
    /// Anything else: network, 5xx, unexpected answer.
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuotaExceeded => f.write_str("quota mensile di caratteri esaurita"),
            Self::TooManyRequests => f.write_str("troppe richieste, riprovo tra poco"),
            Self::Forbidden => f.write_str("chiave API non valida"),
            Self::BadRequest(e) => write!(f, "richiesta rifiutata: {e}"),
            Self::Other(e) => f.write_str(e),
        }
    }
}

pub struct Usage {
    pub used: u64,
    pub limit: u64,
}

fn base_url(key: &str) -> &'static str {
    if key.ends_with(":fx") { "https://api-free.deepl.com/v2" } else { "https://api.deepl.com/v2" }
}

fn auth(key: &str) -> String {
    format!("DeepL-Auth-Key {key}")
}

/// Translates XML texts (see markup.rs) from English to Italian, in the same order.
/// `neighbours` are the texts that arrived together, given as extra context: XML as well, since
/// with tag handling DeepL parses the context too.
pub fn translate(key: &str, texts: &[&str], neighbours: &str) -> Result<Vec<String>, Error> {
    #[derive(Deserialize)]
    struct Reply {
        translations: Vec<Translation>,
    }
    #[derive(Deserialize)]
    struct Translation {
        text: String,
    }

    let body = json!({
        "text": texts,
        "source_lang": "EN",
        "target_lang": "IT",
        "tag_handling": "xml",
        "formality": "prefer_less",
        "context": format!("{CONTEXT}\nTexts shown together on screen:\n{neighbours}"),
    });
    let mut response = AGENT
        .post(format!("{}/translate", base_url(key)))
        .header("Authorization", auth(key))
        .send_json(&body)
        .map_err(|e| Error::Other(e.to_string()))?;
    check(&mut response)?;
    let reply: Reply = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    if reply.translations.len() != texts.len() {
        return Err(Error::Other(format!("{} translations for {} texts", reply.translations.len(), texts.len())));
    }
    Ok(reply.translations.into_iter().map(|t| t.text).collect())
}

pub fn usage(key: &str) -> Result<Usage, Error> {
    #[derive(Deserialize)]
    struct Reply {
        character_count: u64,
        character_limit: u64,
    }

    let mut response = AGENT
        .get(format!("{}/usage", base_url(key)))
        .header("Authorization", auth(key))
        .call()
        .map_err(|e| Error::Other(e.to_string()))?;
    check(&mut response)?;
    let reply: Reply = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    Ok(Usage { used: reply.character_count, limit: reply.character_limit })
}

fn check(response: &mut ureq::http::Response<ureq::Body>) -> Result<(), Error> {
    #[derive(Deserialize)]
    struct Failure {
        message: String,
    }

    let status = response.status().as_u16();
    if status == 200 {
        return Ok(());
    }
    let message = response
        .body_mut()
        .read_to_string()
        .map(|body| serde_json::from_str::<Failure>(&body).map_or(body, |f| f.message))
        .unwrap_or_default();
    Err(match status {
        456 => Error::QuotaExceeded,
        429 => Error::TooManyRequests,
        401 | 403 => Error::Forbidden,
        400 => Error::BadRequest(message),
        s => Error::Other(format!("HTTP {s} {message}")),
    })
}
