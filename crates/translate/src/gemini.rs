//! Gemini API client (https://ai.google.dev/api/generate-content): the fallback when DeepL is
//! out of quota or does not answer. Same input and output as deepl.rs: XML texts with
//! `<x id="N"/>` placeholders, which the addon checks and restores like DeepL's.

use std::{sync::LazyLock, time::Duration};

use serde_json::{Value, json};
use ureq::Agent;

use crate::{Error, LLM_INSTRUCTIONS};

/// A language model is slower than DeepL: a batch of 50 texts takes a few seconds.
static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .http_status_as_error(false)
        .build()
        .into()
});

/// `model`: e.g. gemini-3.1-flash-lite. `neighbours` is not used: the batch itself is the
/// context, as the texts that arrived together.
pub fn translate(key: &str, model: &str, texts: &[&str]) -> Result<Vec<String>, Error> {
    let body = json!({
        "systemInstruction": { "parts": [{ "text": format!("{LLM_INSTRUCTIONS}\nAnswer with a JSON array of the translations.") }] },
        "contents": [{ "role": "user", "parts": [{ "text": Value::from(texts).to_string() }] }],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseSchema": { "type": "ARRAY", "items": { "type": "STRING" } },
        },
    });
    let mut response = AGENT
        .post(format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent"))
        .header("x-goog-api-key", key)
        .send_json(&body)
        .map_err(|e| Error::Other(e.to_string()))?;

    let status = response.status().as_u16();
    if status != 200 {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or(text);
        let message: String = message.chars().take(300).collect();
        return Err(match status {
            // Out of the free tier's requests for today, or only for this minute.
            429 if message.contains("PerDay") => Error::QuotaExceeded,
            429 => Error::TooManyRequests,
            401 | 403 => Error::Forbidden,
            400 if message.contains("API key") => Error::Forbidden,
            400 => Error::BadRequest(message),
            s => Error::Other(format!("HTTP {s} {message}")),
        });
    }

    let reply: Value = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    // The answer, without the model's thoughts if it shows them.
    let text: String = reply["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts.iter().filter(|p| p["thought"] != true).filter_map(|p| p["text"].as_str()).collect()
        })
        .unwrap_or_default();
    let translations: Vec<String> = serde_json::from_str(&text).map_err(|_| {
        let reason = reply["candidates"][0]["finishReason"].as_str().unwrap_or("?");
        Error::BadRequest(format!("not a JSON array (finish reason {reason})"))
    })?;
    if translations.len() != texts.len() {
        return Err(Error::BadRequest(format!("{} translations for {} texts", translations.len(), texts.len())));
    }
    Ok(translations)
}

#[cfg(test)]
mod tests {
    /// GW2IT_GEMINI_KEY=... cargo test -p gw2it-translate -- --ignored
    #[test]
    #[ignore]
    fn live() {
        let key = std::env::var("GW2IT_GEMINI_KEY").expect("GW2IT_GEMINI_KEY");
        let texts = ["Photonic Wall activated", "Deals <x id=\"1\"/> damage to foes &amp; allies."];
        let model = std::env::var("GW2IT_GEMINI_MODEL").unwrap_or_else(|_| "gemini-flash-lite-latest".into());
        let out = super::translate(&key, &model, &texts).unwrap();
        println!("{out:?}");
        assert!(out[1].contains("<x id=\"1\"/>"), "{out:?}");
    }
}
