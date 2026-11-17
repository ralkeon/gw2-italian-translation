//! gw2it-server bulk --file texts.jsonl|map_it.db --translators translators.json [--max-texts N] [--dry-run]
//!
//! Texts nobody has seen in the game yet, translated ahead: one `[id, hash, en]` per line, `hash`
//! the XXH3-64 of the English in UTF-8 as the game gives it. They go the same way as in the addon
//! (crates/addon/src/translate.rs): a name that stays in English is the name, the same English
//! under another ID is copied, the rest is translated in batches of neighbouring IDs (the same
//! tooltip, the same dialogue: context for each other), the markup put back, the glossary's and
//! the plural marks' fixes. They are written as machine translations made by this server (trust
//! 1, author `bulk`): a player's machine translation does not replace them, a hand correction
//! does; the review, the names and the plurals jobs take them like any other row.
//!
//! The language models come first here: they follow the project's rules (LLM_INSTRUCTIONS) and the
//! glossary, and the players' /translate keeps DeepL's quota. Stopped and started again, it goes
//! on from the texts not written yet.

use std::{collections::HashSet, time::Duration};

use gw2it_text::{glossary, markup, plural};
use gw2it_translate::{Chain, Engine, Error};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{SQL_UPSERT, names, now_ms};

const BATCH: usize = 50;
const NEIGHBOURS_MAX: usize = 3000;
/// Waiting in a row for translators that are all paused or busy, before stopping.
const PATIENCE: Duration = Duration::from_secs(2 * 3600);
const WAIT: Duration = Duration::from_secs(60);

pub struct Options {
    pub max_texts: usize,
    pub dry_run: bool,
    /// Translated by hand instead of by the translators (see `ByHand`).
    pub by_hand: Option<ByHand>,
}

/// The translations given by hand (the translator agent, Claude Code on a subscription):
/// `AskTo(file)`: the texts to translate go into the file, one JSON line each: {"id", "hash", "en",
/// "src": the text as the translators get it (markup as <x id="N"/> tags), "terms": the glossary's
/// [en, it] in it, "keep": the names in it that stay English, "refs": what others made of the text,
/// to understand it (the game's fr/es/de, the Local Translator's Italian, GW1 names: refs.rs)};
/// nothing is written to the database.
/// `Answers(file)`: the same lines with "answer": the Italian of "src", tags kept. Each answer goes
/// through what a translator's answer goes through (markup put back and checked, the glossary's fix,
/// the plural marks) and is written as `byhand`: above the review and the names job (priority.rs),
/// which take a person's judgment as final; the agents may still correct it. A text with no answer
/// stays for later.
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
    /// Markup broken by every translator: left to the addon, which shows the English.
    pub rejected: usize,
    pub stopped: Option<String>,
}

/// The language models first, then the machine translators.
pub fn order(engines: Vec<Engine>) -> Vec<Engine> {
    let (machines, models): (Vec<_>, Vec<_>) =
        engines.into_iter().partition(|e| matches!(e, Engine::Deepl { .. } | Engine::Azure { .. } | Engine::Server { .. }));
    models.into_iter().chain(machines).collect()
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

pub fn run(con: &Connection, chain: &Chain, file: &str, options: &Options) -> Result<Report, String> {
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
    let asking = matches!(options.by_hand, Some(ByHand::AskTo(_)));
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
        Some(ByHand::AskTo(file)) => {
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
            return Ok(report);
        }
        Some(ByHand::Answers(file)) => {
            let answers = read_answers(file)?;
            let mut rows = Vec::new();
            for (text, prepared) in &todo {
                let Some(answer) = answers.get(&text.hash) else { continue };
                match prepared.restore(answer).and_then(|it| markup::finish(&text.en, &it)) {
                    Some(it) => {
                        let it = glossary::fix(&text.en, &it, &terms);
                        rows.push((*text, plural::pluralize(&text.en, &it)));
                    }
                    None => {
                        eprintln!("bulk: {} rejected (markup changed): {:?} -> {:?}", text.id, text.en, answer);
                        report.rejected += 1;
                    }
                }
            }
            report.translated = rows.len();
            write_as(con, &rows, options.dry_run, crate::priority::BY_HAND).map_err(db)?;
            if options.dry_run {
                for (text, it) in &rows {
                    println!("bulk: {} {:?} -> {:?}", text.id, text.en, it);
                }
            }
            return Ok(report);
        }
        None => {}
    }

    let mut waited = Duration::ZERO;
    let mut start = 0;
    while start < todo.len() {
        let batch = &todo[start..(start + BATCH).min(todo.len())];
        let xml: Vec<&str> = batch.iter().map(|(_, p)| p.xml.as_str()).collect();
        let mut neighbours = String::new();
        for x in &xml {
            if neighbours.len() + x.len() > NEIGHBOURS_MAX {
                break;
            }
            neighbours.push_str(x);
            neighbours.push('\n');
        }
        let mut translated = match chain.translate(&xml, &neighbours, &terms) {
            Ok(t) => t,
            Err(Error::BadRequest(why)) => {
                eprintln!("bulk: {} texts from id {} refused by every translator: {why}", batch.len(), batch[0].0.id);
                report.rejected += batch.len();
                start += batch.len();
                continue;
            }
            Err(e) => {
                if waited + WAIT > PATIENCE {
                    report.stopped = Some(e.to_string());
                    break;
                }
                eprintln!("bulk: {e}; again in {}s", WAIT.as_secs());
                std::thread::sleep(WAIT);
                waited += WAIT;
                continue;
            }
        };
        waited = Duration::ZERO;
        // The texts whose markup came back broken go to the next translators, as in the addon.
        let mut pending: Vec<usize> = (0..batch.len()).collect();
        let mut skip = Vec::new();
        let mut rows = Vec::new();
        loop {
            skip.push(translated.slot);
            let mut still = Vec::new();
            for (&i, answer) in pending.iter().zip(&translated.texts) {
                let (text, prepared) = &batch[i];
                match prepared.restore(answer).and_then(|it| markup::finish(&text.en, &it)) {
                    Some(it) => {
                        let it = glossary::fix(&text.en, &it, &terms);
                        rows.push((*text, plural::pluralize(&text.en, &it)));
                    }
                    None => still.push(i),
                }
            }
            if still.is_empty() {
                break;
            }
            pending = still;
            let xml: Vec<&str> = pending.iter().map(|&i| batch[i].1.xml.as_str()).collect();
            match chain.translate_without(&skip, &xml, &neighbours, &terms) {
                Ok(next) => translated = next,
                Err(_) => {
                    for &i in &pending {
                        eprintln!("bulk: {} rejected (markup changed): {:?}", batch[i].0.id, batch[i].0.en);
                    }
                    report.rejected += pending.len();
                    break;
                }
            }
        }
        report.translated += rows.len();
        write(con, &rows, options.dry_run).map_err(db)?;
        if options.dry_run {
            for (text, it) in &rows {
                println!("bulk: {} {:?} -> {:?}", text.id, text.en, it);
            }
        }
        start += batch.len();
        if start % 1000 < BATCH {
            println!("bulk: {start}/{} translated by {} so far", todo.len(), translated.by);
        }
    }
    Ok(report)
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
