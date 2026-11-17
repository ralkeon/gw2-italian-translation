//! References for whoever translates or rewrites by hand (the agents: bulk.rs and full.rs
//! `--ask-to` lines carry them as "refs"): what others made of the same text, to understand it,
//! never to copy.
//!
//! - "fr", "es", "de": the game's own French, Spanish and German of the same text ID (gw2dat.rs,
//!   table `other` of data/gw2dat_en.db), when that file has the same English. German only for
//!   short texts: the lines go to a model, and two Romance languages say enough for a long one.
//! - "lt": the Local Translator's Italian (its map, mappa/map_it.db); "lt_hand": the last
//!   correction its reviewers saved for that text, when there is one. Another module's work, with
//!   its own rules: a hint, not ours.
//! - "gw1": [English, Italian] of the GW1 names found in the text (data/gw1.db, from the GW1
//!   client's own Italian; marks like [M]/[F]/[PF] in front are GW1's gender and number).
//!
//! A file that is not there gives nothing: the lines are the same as before.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Map, Value, json};

/// Texts up to this many characters also get the German.
const GERMAN_UP_TO: usize = 200;
/// GW1 names given at most per text, the longest first.
const GW1_MAX: usize = 6;

pub struct Refs {
    other: Option<Connection>,
    map: Option<Connection>,
    /// GW1 names: (English, Italian), longest English first.
    gw1: Vec<(String, String)>,
    /// Which references to give.
    languages: bool,
}

fn open(path: &Path) -> Option<Connection> {
    path.exists().then(|| Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()).flatten()
}

/// The GW1 Italian without its gender/number mark: "[PF]Cento Lame" -> "Cento Lame".
fn unmarked(it: &str) -> &str {
    match it.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some((mark, rest)) if mark.len() <= 3 && mark.chars().all(|c| c.is_ascii_uppercase()) => rest,
        _ => it,
    }
}

/// A GW1 pair worth giving: a name of two words or more (capitalized, short, no markup) whose Italian
/// is another; single words ("Armor", "Guild") are common words the glossary already covers.
fn a_name(en: &str, it: &str) -> bool {
    let words = en.split_whitespace().count();
    en.chars().count() >= 4
        && (2..=5).contains(&words)
        && en.chars().next().is_some_and(char::is_uppercase)
        && !en.contains(['%', '<', '[', '_'])
        && unmarked(it) != en
}

/// `name` in `text` as whole words; a one-word name only where it cannot be a sentence's first word.
fn found(text: &str, name: &str) -> bool {
    let one_word = !name.contains(' ');
    for (at, _) in text.match_indices(name) {
        let before = text[..at].chars().next_back();
        let after = text[at + name.len()..].chars().next();
        if before.is_some_and(char::is_alphanumeric) || after.is_some_and(char::is_alphanumeric) {
            continue;
        }
        let lead = text[..at].trim_end();
        let first = lead.is_empty() || lead.ends_with(['.', '!', '?', ':', '\n', '"']);
        if !one_word || !first || text.trim() == name {
            return true;
        }
    }
    false
}

impl Refs {
    /// The reference files around the database `db` (data/gw2it.db): data/gw2dat_en.db, data/gw1.db,
    /// mappa/map_it.db. `languages`: false gives only "lt" and "gw1" (the full-Italian rewrites).
    pub fn open(db: Option<&str>, languages: bool) -> Refs {
        let dir = db.and_then(|d| Path::new(d).parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
        let up = dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from(".."));
        let mut gw1 = Vec::new();
        if let Some(con) = open(&dir.join("gw1.db")) {
            if let Ok(mut stmt) = con.prepare("SELECT en, it FROM pairs") {
                if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                    gw1 = rows.flatten().filter(|(en, it)| a_name(en, it)).collect();
                }
            }
        }
        gw1.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
        Refs { other: if languages { open(&dir.join("gw2dat_en.db")) } else { None }, map: open(&up.join("mappa").join("map_it.db")), gw1, languages }
    }

    /// The references of the text `id` whose English is `en` (`same`: whether another file's English
    /// is that same text); None when there is none.
    pub fn of(&self, id: i64, en: &str, same: impl Fn(&str) -> bool) -> Option<Value> {
        let mut refs = Map::new();
        if let (true, Some(con)) = (self.languages, &self.other) {
            let english: Option<String> = con.query_row("SELECT en FROM texts WHERE key = ?1", [id], |r| r.get(0)).optional().ok().flatten();
            if english.as_deref().is_some_and(&same) {
                if let Ok(mut stmt) = con.prepare_cached("SELECT lang, text FROM other WHERE key = ?1") {
                    if let Ok(rows) = stmt.query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                        for (lang, text) in rows.flatten() {
                            if lang != "de" || en.chars().count() <= GERMAN_UP_TO {
                                refs.insert(lang, json!(text.trim()));
                            }
                        }
                    }
                }
            }
        }
        if let Some(con) = &self.map {
            let row: Option<(String, Option<String>)> =
                con.query_row("SELECT en, it FROM texts WHERE key = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok().flatten();
            if let Some((_, Some(it))) = row.filter(|(e, _)| same(e)) {
                if !it.trim().is_empty() {
                    refs.insert("lt".into(), json!(it.trim()));
                }
            }
            let hand: Option<(String, String)> = con
                .query_row(
                    "SELECT suggestion, en_source FROM suggestions WHERE key = ?1 ORDER BY id DESC LIMIT 1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .ok()
                .flatten();
            if let Some((it, source)) = hand {
                if (source.is_empty() || same(&source)) && !it.trim().is_empty() && refs.get("lt").and_then(Value::as_str) != Some(it.as_str()) {
                    refs.insert("lt_hand".into(), json!(it.trim()));
                }
            }
        }
        let mut gw1: Vec<[&str; 2]> = Vec::new();
        for (name, it) in &self.gw1 {
            if gw1.len() >= GW1_MAX {
                break;
            }
            // Not a name inside a longer one already given ("Ascalon" in "Ascalon City").
            if !gw1.iter().any(|[n, _]| n.contains(name.as_str())) && found(en, name) {
                gw1.push([name, it]);
            }
        }
        if !gw1.is_empty() {
            refs.insert("gw1".into(), json!(gw1));
        }
        (!refs.is_empty()).then_some(Value::Object(refs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gw1_names_are_whole_words_not_first_words() {
        assert!(found("The walls of Ascalon City fell.", "Ascalon City"));
        assert!(found("They fled Ascalon.", "Ascalon"));
        assert!(found("Ascalon City", "Ascalon City"));
        assert!(!found("Ascalonian guards", "Ascalon"));
        assert!(!found("Load the cannon. Load it now.", "Load"));
        assert!(found("Ascalon", "Ascalon"));
        assert!(found("Cast Inner Fire now", "Inner Fire"));
    }

    #[test]
    fn gw1_marks_and_names() {
        assert_eq!(unmarked("[PF]Cento Lame"), "Cento Lame");
        assert_eq!(unmarked("Limite di credito"), "Limite di credito");
        assert!(a_name("Inner Fire", "[M]Fuoco Interiore"));
        assert!(!a_name("Olias", "[U]Olias"));
        assert!(!a_name("Armor", "[F]Armatura"));
        assert!(a_name("Ascalon City", "[F]Città di Ascalon"));
        assert!(!a_name("insufficient funds", "Fondi non sufficienti"));
        assert!(!a_name("Hundred Blades (PvP) %str1%", "x"));
    }
}
