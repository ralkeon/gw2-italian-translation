//! The model of the review (review.rs) and of the plurals check (plurals.rs) answered by hand: by
//! a person, or by a chat with a language model on a subscription instead of a paid API.
//!
//! `--ask-to <file>`: every text the job would give the model goes into the file, one JSON line
//! each ({"en", "it", and "keep", "glossary"... as the model gets them}); the job gets no answer,
//! so nothing is written (run it with --dry-run, or the texts are counted as given today).
//! `--answers <file>`: the same lines, each with "answer": what the model would answer for that
//! text ({"ok": true} or {"it", "why", "kind"...}, no "n"). The job runs as with a model, with the
//! same checks: a text with no line in the file gets no answer and stays for later.

use std::{collections::HashMap, fs, io::Write};

use serde_json::{Value, json};

use crate::names::Ask;

fn texts(user: &str) -> Vec<Value> {
    serde_json::from_str::<Value>(user).ok().and_then(|v| v["texts"].as_array().cloned()).unwrap_or_default()
}

fn key(text: &Value) -> (String, String) {
    (text["en"].as_str().unwrap_or("").to_owned(), text["it"].as_str().unwrap_or("").to_owned())
}

/// Writes the texts asked about into `file` (appending), and answers nothing.
pub fn ask_to(file: &str) -> std::io::Result<Box<Ask<'static>>> {
    let out = std::sync::Mutex::new(fs::OpenOptions::new().create(true).append(true).open(file)?);
    Ok(Box::new(move |_: &str, user: &str| {
        let mut out = out.lock().map_err(|e| e.to_string())?;
        for mut text in texts(user) {
            if let Some(o) = text.as_object_mut() {
                o.remove("n");
            }
            writeln!(out, "{text}").map_err(|e| e.to_string())?;
        }
        Ok(json!({ "texts": [] }))
    }))
}

/// Answers from `file` the texts it has an answer for.
pub fn answers(file: &str) -> std::io::Result<Box<Ask<'static>>> {
    let mut by_text = HashMap::new();
    for line in fs::read_to_string(file)?.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| std::io::Error::other(format!("{file}: {e}: {line}")))?;
        if v["answer"].is_object() {
            by_text.insert(key(&v), v["answer"].clone());
        }
    }
    Ok(Box::new(move |_: &str, user: &str| {
        let answered: Vec<Value> = texts(user)
            .iter()
            .filter_map(|text| {
                let mut a = by_text.get(&key(text))?.clone();
                a["n"] = text["n"].clone();
                Some(a)
            })
            .collect();
        Ok(json!({ "texts": answered }))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_texts_go_out_and_their_answers_come_back_by_text() {
        let dir = std::env::temp_dir().join(format!("gw2it-byhand-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let (asked, answered) = (dir.join("ask.jsonl"), dir.join("answers.jsonl"));
        let _ = fs::remove_file(&asked);
        let user = json!({"texts": [{"n": 0, "en": "A", "it": "a"}, {"n": 1, "en": "B", "it": "b", "keep": ["B"]}]}).to_string();
        let ask = ask_to(asked.to_str().unwrap()).unwrap();
        assert_eq!(ask("s", &user).unwrap(), json!({"texts": []}));
        let lines = fs::read_to_string(&asked).unwrap();
        assert_eq!(lines, "{\"en\":\"A\",\"it\":\"a\"}\n{\"en\":\"B\",\"it\":\"b\",\"keep\":[\"B\"]}\n");
        fs::write(&answered, "{\"en\":\"B\",\"it\":\"b\",\"answer\":{\"ok\":true}}\n{\"en\":\"A\",\"it\":\"x\",\"answer\":{\"ok\":true}}\n").unwrap();
        // Another order and other numbers: by English and Italian, the one with no answer left out.
        let user = json!({"texts": [{"n": 0, "en": "B", "it": "b"}, {"n": 1, "en": "A", "it": "a"}]}).to_string();
        let ask = answers(answered.to_str().unwrap()).unwrap();
        assert_eq!(ask("s", &user).unwrap(), json!({"texts": [{"ok": true, "n": 0}]}));
        fs::remove_dir_all(&dir).unwrap();
    }
}
