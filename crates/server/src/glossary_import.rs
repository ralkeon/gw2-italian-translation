//! The glossary of the Local Translator module (glossary_it.json), brought into ours:
//! `gw2it-server import-glossary --file glossary_it.json [--excluded server/glossary-import-excluded.json] [--dry-run]`.
//!
//! What is the same word in both sides ("Fractal": "Fractal") is a name that stays in English: it
//! goes into `english_names` (kind `imported`), which the server and the addons already use to keep a
//! name in a sentence and as a whole text (gw2it-text names.rs). What it translates (a word with an
//! Italian form: "Weaponsmith": "Fabbro") goes into the glossary, as the owner says it. Left out:
//! - the lowercase terms and the possessive ones ("Dragon's"): not names, but words of a sentence;
//! - the ones in the excluded file, each with the reason;
//! - what our mechanisms cannot say (several words translated, patterns, whole-text rules): listed.
//!
//! Nothing is rewritten here: the texts already translated follow when the names job runs.

use std::collections::HashMap;

use gw2it_text::glossary::Term;
use rusqlite::{Connection, params};
use serde_json::Value;

use crate::{glossary as server_glossary, now_ms};

pub const KIND: &str = "imported";

#[derive(Default, Debug, PartialEq)]
pub struct Plan {
    /// Names that stay in English.
    pub keep: Vec<String>,
    /// Words with an Italian form, for the glossary: (English lowercase, Italian).
    pub translated: Vec<(String, String)>,
    /// Left out, with the reason.
    pub left_out: Vec<(String, String)>,
    /// What our mechanisms cannot say: (what the imported glossary has, Italian, why).
    pub unsupported: Vec<(String, String, &'static str)>,
}

/// The excluded terms of the file: term -> reason.
pub fn excluded(file: &Value) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for class in file["classes"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let reason = class["reason"].as_str().unwrap_or("excluded");
        for t in class["terms"].as_array().map(Vec::as_slice).unwrap_or_default() {
            if let Some(t) = t.as_str() {
                out.insert(t.to_owned(), reason.to_owned());
            }
        }
    }
    out
}

pub fn plan(glossary: &Value, excluded: &HashMap<String, String>) -> Plan {
    let mut plan = Plan::default();
    let terms = glossary["terms"].as_object().cloned().unwrap_or_default();
    let mut entries: Vec<(&String, &Value)> = terms.iter().collect();
    entries.sort_by_key(|(k, _)| k.as_str());
    for (en, it) in entries {
        let Some(it) = it.as_str() else { continue };
        if en == it {
            if !en.chars().next().is_some_and(char::is_uppercase) {
                plan.left_out.push((en.clone(), "minuscolo: una parola di frase, non un nome".into()));
            } else if !en.contains(' ') && (en.ends_with("'s") || en.ends_with("’s")) {
                plan.left_out.push((en.clone(), "possessivo: un prefisso, non un nome".into()));
            } else if let Some(reason) = excluded.get(en.as_str()) {
                plan.left_out.push((en.clone(), reason.clone()));
            } else {
                plan.keep.push(en.clone());
            }
            continue;
        }
        // Translated: a word the glossary can say, else listed. The lowercase and the capitalised
        // form of the imported glossary are one word for us; the lowercase one's Italian is used.
        let word = en.to_lowercase();
        let term = Term { en: word.clone(), it: it.to_owned(), wrong: Vec::new() };
        if let Some(reason) = excluded.get(en.as_str()) {
            plan.left_out.push((en.clone(), reason.clone()));
        } else if !en.contains(' ') && term.is_valid() {
            let lowercase = en.chars().next().is_some_and(char::is_lowercase);
            match plan.translated.iter_mut().find(|(w, _)| *w == word) {
                Some(there) if lowercase => there.1 = it.to_owned(),
                Some(_) => {}
                None => plan.translated.push((word, it.to_owned())),
            }
        } else {
            plan.unsupported.push((en.clone(), it.to_owned(), "several words or not a plain word: the glossary says one English word"));
        }
    }
    for (en, it) in glossary["exact"].as_object().into_iter().flatten() {
        if Some(en.as_str()) != it.as_str() {
            plan.unsupported.push((en.clone(), it.as_str().unwrap_or("").to_owned(), "a rule for the whole text: not supported"));
        }
    }
    for (en, it) in glossary["patterns"].as_object().into_iter().flatten() {
        plan.unsupported.push((en.clone(), it.as_str().unwrap_or("").to_owned(), "a pattern with {X}: not supported"));
    }
    plan
}

/// Writes the plan; returns (names added, glossary terms changed). Already there: left as it is.
pub fn apply(con: &Connection, plan: &Plan) -> rusqlite::Result<(usize, usize)> {
    let now = now_ms();
    let (mut names, mut terms) = (0, 0);
    let tx = con.unchecked_transaction()?;
    for name in &plan.keep {
        names += tx.execute("INSERT OR IGNORE INTO english_names (name, kind, added) VALUES (?1, ?2, ?3)", params![name, KIND, now])?;
    }
    for (en, it) in &plan.translated {
        let term = Term { en: en.clone(), it: it.clone(), wrong: Vec::new() };
        // The owner says it (trust 3): the imported decisions are ours, until someone corrects them.
        if server_glossary::put(&tx, &term, KIND, 3, now, 0)? {
            terms += 1;
        }
    }
    tx.commit()?;
    Ok((names, terms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_goes_where() {
        let g = json!({
            "terms": {
                "Fractal": "Fractal", "Divinity's Reach": "Divinity's Reach", "Dragon's": "Dragon's", "runes": "runes",
                "Bolt": "Bolt", "Weaponsmith": "Fabbro", "weaponsmith": "fabbro", "Lion's Arch": "Arco del Leone"
            },
            "exact": {"Waypoint": "Waypoint", "Junk": "Ciarpame"},
            "patterns": {"{X} Longbow": "Arco lungo {X}"}
        });
        let file = json!({"classes": [{"reason": "parola comune", "terms": ["Bolt"]}]});
        let p = plan(&g, &excluded(&file));
        assert_eq!(p.keep, ["Divinity's Reach", "Fractal"]);
        assert_eq!(p.translated, [("weaponsmith".to_owned(), "fabbro".to_owned())]);
        let out: Vec<&str> = p.left_out.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(out, ["Bolt", "Dragon's", "runes"]);
        assert_eq!(p.left_out[0].1, "parola comune");
        let un: Vec<&str> = p.unsupported.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(un, ["Lion's Arch", "Junk", "{X} Longbow"]);
    }

    #[test]
    fn apply_adds_names_once_and_says_the_terms() {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(include_str!("../../../server/schema.sql")).unwrap();
        con.execute_batch(crate::names::SCHEMA).unwrap();
        con.execute("INSERT INTO english_names VALUES ('Fractal', 'item', 1)", []).unwrap();
        let p = Plan {
            keep: vec!["Fractal".into(), "Tyria".into()],
            translated: vec![("weaponsmith".into(), "Fabbro".into())],
            ..Plan::default()
        };
        assert_eq!(apply(&con, &p).unwrap(), (1, 1));
        assert_eq!(apply(&con, &p).unwrap(), (0, 0));
        let kind: String = con.query_row("SELECT kind FROM english_names WHERE name = 'Fractal'", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "item");
        assert_eq!(server_glossary::all(&con).unwrap()[0].it, "Fabbro");
    }
}
