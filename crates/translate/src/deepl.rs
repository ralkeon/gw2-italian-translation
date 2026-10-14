//! DeepL API client (https://developers.deepl.com/api-reference/translate).

use std::{sync::LazyLock, time::Duration};

use serde_json::{Value, json};
use ureq::Agent;

use crate::{CONTEXT, Error, check};

/// Short enough that unloading the addon (which waits for a request in flight) stays quick.
static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into()
});

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

/// Translates XML texts (see markup.rs in the addon) from English to Italian, in the same order.
/// `neighbours` are the texts that arrived together, given as extra context: XML as well, since
/// with tag handling DeepL parses the context too.
pub fn translate(key: &str, texts: &[&str], neighbours: &str) -> Result<Vec<String>, Error> {
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
    let reply: Value = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    let translations: Vec<String> = reply["translations"]
        .as_array()
        .map(|t| t.iter().filter_map(|t| t["text"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if translations.len() != texts.len() {
        return Err(Error::Other(format!("{} translations for {} texts", translations.len(), texts.len())));
    }
    Ok(translations)
}

pub fn usage(key: &str) -> Result<Usage, Error> {
    let mut response = AGENT
        .get(format!("{}/usage", base_url(key)))
        .header("Authorization", auth(key))
        .call()
        .map_err(|e| Error::Other(e.to_string()))?;
    check(&mut response)?;
    let reply: Value = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    match (reply["character_count"].as_u64(), reply["character_limit"].as_u64()) {
        (Some(used), Some(limit)) => Ok(Usage { used, limit }),
        _ => Err(Error::Other("unexpected usage answer".into())),
    }
}
