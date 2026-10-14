//! Microsoft Azure AI Translator (https://learn.microsoft.com/azure/ai-services/translator/):
//! a machine translator like DeepL, 2M characters a month on the free tier (F0). The texts go as
//! HTML, so that the `<x id="N"/>` placeholders are kept as markup.

use std::{sync::LazyLock, time::Duration};

use serde_json::{Value, json};
use ureq::Agent;

use crate::Error;

static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .into()
});

/// `region`: the resource's region (e.g. westeurope); empty for a global resource.
pub fn translate(key: &str, region: &str, texts: &[&str]) -> Result<Vec<String>, Error> {
    let body: Vec<Value> = texts.iter().map(|t| json!({ "Text": t })).collect();
    let mut request = AGENT
        .post("https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&from=en&to=it&textType=html")
        .header("Ocp-Apim-Subscription-Key", key);
    if !region.is_empty() {
        request = request.header("Ocp-Apim-Subscription-Region", region);
    }
    let mut response = request.send_json(&body).map_err(|e| Error::Other(e.to_string()))?;

    let status = response.status().as_u16();
    if status != 200 {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let error = serde_json::from_str::<Value>(&text).map(|v| v["error"].clone()).unwrap_or_default();
        let code = error["code"].as_u64().unwrap_or(0);
        let message = error["message"].as_str().map_or(text.clone(), str::to_owned);
        return Err(match (status, code) {
            // 403001: the free tier's characters for this month are used up.
            (403, 403001) => Error::QuotaExceeded,
            (429, _) => Error::TooManyRequests,
            (401 | 403, _) => Error::Forbidden,
            (400, _) => Error::BadRequest(message),
            (s, _) => Error::Other(format!("HTTP {s} {message}")),
        });
    }

    let reply: Value = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    let translations: Vec<String> = reply
        .as_array()
        .map(|items| items.iter().filter_map(|i| i["translations"][0]["text"].as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    if translations.len() != texts.len() {
        return Err(Error::Other(format!("{} translations for {} texts", translations.len(), texts.len())));
    }
    Ok(translations)
}
