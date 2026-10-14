//! The project's server as a translator (POST /translate, crates/server/src/translate.rs), for
//! players without keys of their own. It runs the same chain with its own keys.

use std::{sync::LazyLock, time::Duration};

use serde_json::{Value, json};
use ureq::Agent;

use crate::{Error, check};

/// Above the server's own chain (DeepL, then Gemini), so that we get its answer.
static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(40)))
        .http_status_as_error(false)
        .build()
        .into()
});

/// The translations and who made them on the server ("deepl", "gemini"). Errors have DeepL's
/// status codes; 401/403 is about the sync token.
pub fn translate(url: &str, token: &str, texts: &[&str], neighbours: &str) -> Result<(Vec<String>, String), Error> {
    let mut response = AGENT
        .post(format!("{url}/translate"))
        .header("Authorization", format!("Bearer {token}"))
        .send_json(json!({ "texts": texts, "neighbours": neighbours }))
        .map_err(|e| Error::Other(e.to_string()))?;
    check(&mut response)?;
    let reply: Value = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    let translations: Vec<String> = reply["translations"]
        .as_array()
        .map(|t| t.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if translations.len() != texts.len() {
        return Err(Error::Other(format!("{} translations for {} texts", translations.len(), texts.len())));
    }
    Ok((translations, reply["by"].as_str().unwrap_or("deepl").to_owned()))
}
