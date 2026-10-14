//! Any service with OpenAI's chat completions API (POST {url}/chat/completions): Groq,
//! OpenRouter, Mistral, Cerebras, OpenAI itself... Same input and output as deepl.rs: XML texts
//! with `<x id="N"/>` placeholders.

use std::{sync::LazyLock, time::Duration};

use serde_json::{Value, json};
use ureq::Agent;

use crate::{Error, LLM_INSTRUCTIONS};

static AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .http_status_as_error(false)
        .build()
        .into()
});

/// For `ask`: long answers (a rewrite of dozens of texts).
static SLOW_AGENT: LazyLock<Agent> = LazyLock::new(|| {
    Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .into()
});

/// Services with a free tier, for the options panel: (name, url, a fast model).
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("Groq", "https://api.groq.com/openai/v1", "openai/gpt-oss-120b"),
    ("Cerebras", "https://api.cerebras.ai/v1", "gpt-oss-120b"),
    ("OpenRouter", "https://openrouter.ai/api/v1", "google/gemma-4-31b-it:free"),
    ("Mistral", "https://api.mistral.ai/v1", "mistral-small-latest"),
];

/// `url`: the API base, e.g. https://api.groq.com/openai/v1.
pub fn translate(url: &str, key: &str, model: &str, texts: &[&str], glossary: &str) -> Result<Vec<String>, Error> {
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": format!("{LLM_INSTRUCTIONS}{glossary}\nAnswer with a JSON object: {{\"translations\": [...]}}.") },
            { "role": "user", "content": Value::from(texts).to_string() },
        ],
        "response_format": { "type": "json_object" },
        "temperature": 0.3,
    });
    let content = complete(&AGENT, url, key, &body)?;
    let answer = parse(&content)?;
    let list = if answer.is_array() { &answer } else { &answer["translations"] };
    let translations: Vec<String> =
        list.as_array().map(|t| t.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect()).unwrap_or_default();
    if translations.len() != texts.len() {
        return Err(Error::BadRequest(format!("{} translations for {} texts", translations.len(), texts.len())));
    }
    Ok(translations)
}

/// A question to the model with a JSON answer of any shape (the consistency job in crates/server).
pub fn ask(url: &str, key: &str, model: &str, system: &str, user: &str) -> Result<Value, Error> {
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "response_format": { "type": "json_object" },
        "temperature": 0.2,
    });
    parse(&complete(&SLOW_AGENT, url, key, &body)?)
}

/// Some models think aloud before the JSON.
fn parse(content: &str) -> Result<Value, Error> {
    let json = content.find(['{', '[']).map_or("", |i| &content[i..]);
    serde_json::from_str(json).map_err(|_| Error::BadRequest(format!("not JSON: {}", content.chars().take(100).collect::<String>())))
}

/// The text of the model's first answer to `body`, or why it did not answer.
fn complete(agent: &Agent, url: &str, key: &str, body: &Value) -> Result<String, Error> {
    let mut response = agent
        .post(format!("{}/chat/completions", url.trim_end_matches('/')))
        .header("Authorization", format!("Bearer {key}"))
        .send_json(body)
        .map_err(|e| Error::Other(e.to_string()))?;

    let status = response.status().as_u16();
    if status != 200 {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().or(v["message"].as_str()).map(str::to_owned))
            .unwrap_or(text);
        let message: String = message.chars().take(300).collect();
        let quota = ["quota", "credit", "insufficient", "per day"].iter().any(|w| message.to_lowercase().contains(w));
        return Err(match status {
            402 => Error::QuotaExceeded,
            429 if quota => Error::QuotaExceeded,
            429 => Error::TooManyRequests,
            401 | 403 => Error::Forbidden,
            400 => Error::BadRequest(message),
            s => Error::Other(format!("HTTP {s} {message}")),
        });
    }

    let reply: Value = response.body_mut().read_json().map_err(|e| Error::Other(e.to_string()))?;
    Ok(reply["choices"][0]["message"]["content"].as_str().unwrap_or("").to_owned())
}
