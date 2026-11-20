//! gw2it-server bulk --file texts.jsonl|map_it.db (--ask-to <file> | --answers <file>) [--all] [--max-texts N] [--dry-run]
//!
//! Texts nobody has seen in the game yet, translated ahead by the agents: one `[id, hash, en]` per
//! line, `hash` the XXH3-64 of the English in UTF-8 as the game gives it. A name that stays in
//! English is the name, the same English under another ID is copied, the rest goes to the agent
//! (`ByHand`) with its neighbouring IDs (the same tooltip, the same dialogue: context for each
//! other); its answers get the markup put back and checked and the glossary's fixes; the plural
//! marks are the agent's.
//! Stopped and started again, it goes on from the texts not written yet.

use std::collections::HashSet;

use gw2it_text::{glossary, markup};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{SQL_UPSERT, names, now_ms};

/// The author of the rows answered by hand.
const BY_HAND: &str = "byhand";

pub struct Options {
    pub max_texts: usize,
    pub dry_run: bool,
    /// Every text goes to the agent: no name written at once, no copy of the same English, the
    /// same English twice asked twice. For a translation made from scratch (the agents' pilot: a
    /// database of its own, nothing it holds trusted).
    pub all: bool,
    pub by_hand: ByHand,
}

/// The translations given by hand (the translator agent, Claude Code on a subscription):
/// `AskTo(file)`: the texts to translate go into the file, one JSON line each: {"id", "hash", "en",
/// "src": the text as the translators get it (markup as <x id="N"/> tags), "terms": the glossary's
/// [en, it] in it, "keep": the names in it that stay English, "refs": what others made of the text,
/// to understand it (the game's fr/es/de, the Local Translator's Italian, GW1 names: refs.rs)};
/// nothing is written to the database.
/// `Answers(file)`: the same lines with "answer": the Italian of "src", tags kept. Each answer goes
/// through the checks (markup put back and checked, the glossary's fix) and is
/// written as `byhand` (trust 1); the names and the copies of the same English as `bulk`. A text
/// with no answer stays for later.
pub enum ByHand {
    AskTo(String),
    Answers(String),
}

#[derive(Default)]
pub struct Report {
    pub read: usize,
    pub known: usize,
    /// IDs we have, whose English in the file is new (a patch changed it, or the map now has the
    /// game's form with its plural marks): a new row, translated like an unknown text.
    pub changed: usize,
    pub names: usize,
    pub copied: usize,
    pub nothing: usize,
    pub translated: usize,
    /// Answers whose markup came back broken: left for later.
    pub rejected: usize,
}

struct Text {
    id: u32,
    hash: String,
    en: String,
}

fn read(file: &str) -> Result<Vec<Text>, String> {
    if file.ends_with(".db") {
        return read_map(file);
    }
    let content = std::fs::read_to_string(file).map_err(|e| format!("cannot read {file}: {e}"))?;
    let mut texts = Vec::new();
    for (n, line) in content.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("{file}:{}: {e}", n + 1))?;
        let (Some(id), Some(hash), Some(en)) = (v[0].as_u64(), v[1].as_str(), v[2].as_str()) else {
            return Err(format!("{file}:{}: not [id, hash, en]", n + 1));
        };
        if id > u32::MAX as u64 || hash.len() != 16 {
            return Err(format!("{file}:{}: bad id or hash", n + 1));
        }
        texts.push(Text { id: id as u32, hash: hash.to_owned(), en: en.to_owned() });
    }
    texts.sort_by_key(|t| t.id);
    Ok(texts)
}

/// The texts of the Local Translator module's map (`texts(key, en, ...)`, its release "mappa"):
/// only the ID and the English (its Italian is only a reference for the agents: refs.rs); the hash is ours. Texts without a letter
/// (placeholders, numbers) are left out.
fn read_map(file: &str) -> Result<Vec<Text>, String> {
    let con = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| format!("cannot open {file}: {e}"))?;
    let mut stmt = con.prepare("SELECT key, en FROM texts WHERE en <> '' ORDER BY key").map_err(|e| format!("{file}: {e}"))?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|e| format!("{file}: {e}"))?;
    Ok(rows
        .into_iter()
        .filter(|(id, en)| (0..=u32::MAX as i64).contains(id) && en.chars().any(char::is_alphabetic))
        .map(|(id, en)| Text { id: id as u32, hash: format!("{:016x}", xxhash_rust::xxh3::xxh3_64(en.as_bytes())), en })
        .collect())
}

/// Whether the file's English `file` is the English `ours` we have for that ID, or only another way
/// of writing it that is not news: line breaks as `<br>` or as a newline (the game sends both), or
/// the plural spelled out where we have the game's form with its marks ("Icy Sacks" for our
/// "Icy Sack[s]"). The opposite, the marks where we have the spelled-out plural ("Phoenix Mace
/// Skin[s]" for our old "Phoenix Mace Skins"), is the game's form we lack: not the same.
fn same_english(ours: &str, file: &str) -> bool {
    let (ours, file) = (ours.replace("<br>", "\n"), file.replace("<br>", "\n"));
    ours == file || spelled_plural(&ours) == file
}

/// `en` with its plural marks resolved to the plural: "Sack[s]" -> "Sacks",
/// `Sandwich[pl:"Sandwiches"]` -> "Sandwiches".
fn spelled_plural(en: &str) -> String {
    let mut out = String::with_capacity(en.len());
    let mut rest = en;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("[s]") {
            out.push('s');
            rest = after;
        } else if let Some(inner) = rest.strip_prefix("[pl:\"")
            && let Some(end) = inner.find("\"]")
        {
            // The mark replaces the word before it.
            let word = out.trim_end_matches(char::is_alphabetic).len();
            out.truncate(word);
            out.push_str(&inner[..end]);
            rest = &inner[end + 2..];
        } else {
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Writes the rows `[id, hash, en, it]` as this server's machine translations.
fn write(con: &Connection, rows: &[(&Text, String)], dry_run: bool) -> rusqlite::Result<()> {
    write_as(con, rows, dry_run, "bulk")
}

fn write_as(con: &Connection, rows: &[(&Text, String)], dry_run: bool, author: &str) -> rusqlite::Result<()> {
    if rows.is_empty() || dry_run {
        return Ok(());
    }
    let json = Value::Array(rows.iter().map(|(t, it)| json!([t.id, t.hash, t.en, it, "auto", author, 1])).collect());
    con.execute(SQL_UPSERT, params![json.to_string(), author, now_ms(), 0])?;
    Ok(())
}

/// The answers by hand of `file`, by the hash of the English.
fn read_answers(file: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let mut out = std::collections::HashMap::new();
    for (n, line) in std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("{file}:{}: {e}", n + 1))?;
        if let (Some(hash), Some(answer)) = (v["hash"].as_str(), v["answer"].as_str()) {
            out.insert(hash.to_owned(), answer.to_owned());
        }
    }
    Ok(out)
}

pub fn run(con: &Connection, file: &str, options: &Options) -> Result<Report, String> {
    let db = |e: rusqlite::Error| format!("database: {e}");
    let texts = read(file)?;
    let mut report = Report { read: texts.len(), ..Report::default() };
    let english = names::load(con).map_err(db)?;
    let terms = crate::glossary::all(con).map_err(db)?;

    let mut todo: Vec<(&Text, markup::Prepared)> = Vec::new();
    let mut quick: Vec<(&Text, String)> = Vec::new();
    let mut queued: HashSet<&str> = HashSet::new();
    let mut ours_of = con.prepare("SELECT en FROM translations WHERE id = ?1").map_err(db)?;
    for text in &texts {
        // A known ID is skipped only when its English is one we have: an ID whose English changed
        // since (a patch, or the map's own fix) gets a row for the new English.
        let ours: Vec<String> = ours_of.query_map([text.id], |r| r.get(0)).and_then(|rows| rows.collect()).map_err(db)?;
        if ours.iter().any(|en| same_english(en, &text.en)) {
            report.known += 1;
            continue;
        }
        if !ours.is_empty() {
            report.changed += 1;
        }
        if options.all {
            match markup::prepare(&text.en) {
                Some(prepared) if todo.len() < options.max_texts => todo.push((text, prepared)),
                Some(_) => {}
                None => report.nothing += 1,
            }
            continue;
        }
        if let Some(name) = english.whole(&text.en) {
            report.names += 1;
            quick.push((text, name));
            continue;
        }
        let same: Option<String> = con
            // The strongest, and among those the last changed: the one the jobs corrected.
            .query_row("SELECT it FROM translations WHERE hash = ?1 ORDER BY trust DESC, seq DESC LIMIT 1", [&text.hash], |r| r.get(0))
            .optional()
            .map_err(db)?;
        if let Some(it) = same {
            report.copied += 1;
            quick.push((text, it));
            continue;
        }
        // The same English twice in the file: the first is translated, the next run copies it.
        if !queued.insert(&text.hash) {
            continue;
        }
        match markup::prepare(&text.en) {
            Some(prepared) if todo.len() < options.max_texts => todo.push((text, prepared)),
            Some(_) => {}
            None => report.nothing += 1,
        }
    }
    let asking = matches!(options.by_hand, ByHand::AskTo(_));
    write(con, &quick, options.dry_run || asking).map_err(db)?;
    println!(
        "bulk: {} texts, {} known, {} with new English, {} names, {} copied, {} with nothing to translate, {} to translate",
        report.read,
        report.known,
        report.changed,
        report.names,
        report.copied,
        report.nothing,
        todo.len()
    );

    match &options.by_hand {
        ByHand::AskTo(file) => {
            use std::io::Write;
            let mut out = std::fs::OpenOptions::new().create(true).append(true).open(file).map_err(|e| format!("{file}: {e}"))?;
            let refs = crate::refs::Refs::open(con.path(), true);
            for (text, prepared) in &todo {
                let terms: Vec<[&str; 2]> = glossary::relevant(&terms, &[&text.en]).iter().map(|t| [t.en.as_str(), t.it.as_str()]).collect();
                let keep: Vec<&str> = english.find(&text.en).iter().map(|&(a, b)| &text.en[a..b]).collect();
                let mut line = json!({"id": text.id, "hash": text.hash, "en": text.en, "src": prepared.xml, "terms": terms, "keep": keep});
                if let Some(r) = refs.of(text.id as i64, &text.en, |e| same_english(e, &text.en) || same_english(&text.en, e)) {
                    line["refs"] = r;
                }
                writeln!(out, "{line}").map_err(|e| format!("{file}: {e}"))?;
            }
            println!("bulk: {} texts to translate by hand written to {file}", todo.len());
            Ok(report)
        }
        ByHand::Answers(file) => {
            let answers = read_answers(file)?;
            let mut rows = Vec::new();
            for (text, prepared) in &todo {
                let Some(answer) = answers.get(&text.hash) else { continue };
                match prepared.restore(answer).and_then(|it| markup::finish(&text.en, &it)) {
                    Some(it) => {
                        rows.push((*text, glossary::fix(&text.en, &it, &terms)));
                    }
                    None => {
                        eprintln!("bulk: {} rejected (markup changed): {:?} -> {:?}", text.id, text.en, answer);
                        report.rejected += 1;
                    }
                }
            }
            report.translated = rows.len();
            write_as(con, &rows, options.dry_run, BY_HAND).map_err(db)?;
            if options.dry_run {
                for (text, it) in &rows {
                    println!("bulk: {} {:?} -> {:?}", text.id, text.en, it);
                }
            }
            Ok(report)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{same_english, spelled_plural};

    #[test]
    fn plural_spelled_out() {
        assert_eq!(spelled_plural("Icy Sack[s]"), "Icy Sacks");
        assert_eq!(spelled_plural("Stick[s] of Cotton Candy"), "Sticks of Cotton Candy");
        assert_eq!(spelled_plural("Roasted Meaty Sandwich[pl:\"Sandwiches\"]"), "Roasted Meaty Sandwiches");
        assert_eq!(spelled_plural("No marks here"), "No marks here");
    }

    #[test]
    fn new_english_only_when_it_is_news() {
        // The same text, or only written another way: no new row.
        assert!(same_english("Upset Stomach", "Upset Stomach"));
        assert!(same_english("+5%% Strike Damage<br>+2%% Power", "+5%% Strike Damage\n+2%% Power"));
        assert!(same_english("+5%% Strike Damage\n+2%% Power", "+5%% Strike Damage<br>+2%% Power"));
        assert!(same_english("Icy Sack[s]", "Icy Sacks"));
        // The game's form with its marks where we only have the spelled-out plural: news.
        assert!(!same_english("Phoenix Mace Skins", "Phoenix Mace Skin[s]"));
        assert!(!same_english("Roasted Meaty Sandwiches", "Roasted Meaty Sandwich[pl:\"Sandwiches\"]"));
        // A patch changed the text: news.
        assert!(!same_english("Reduces incoming condition duration.", "Increases outgoing healing."));
    }

    #[test]
    fn map_hash_is_the_game_s() {
        // Row 1102280 as the game sent it to the addons.
        let en = "%str1%%str2%Obsidian Light Gloves%str3%%str4%";
        assert_eq!(format!("{:016x}", xxhash_rust::xxh3::xxh3_64(en.as_bytes())), "07ed9f3d957e55a6");
    }
}
