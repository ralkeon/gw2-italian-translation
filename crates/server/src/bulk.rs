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
}

#[derive(Default)]
pub struct Report {
    pub read: usize,
    pub known: usize,
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
/// only the ID and the English, never its Italian; the hash is ours. Texts without a letter
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

/// Writes the rows `[id, hash, en, it]` as this server's machine translations.
fn write(con: &Connection, rows: &[(&Text, String)], dry_run: bool) -> rusqlite::Result<()> {
    if rows.is_empty() || dry_run {
        return Ok(());
    }
    let json = Value::Array(rows.iter().map(|(t, it)| json!([t.id, t.hash, t.en, it, "auto", "bulk", 1])).collect());
    con.execute(SQL_UPSERT, params![json.to_string(), "bulk", now_ms(), 0])?;
    Ok(())
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
    for text in &texts {
        let known: bool = con
            .query_row("SELECT count(*) FROM translations WHERE id = ?1", [text.id], |r| r.get::<_, i64>(0))
            .map_err(db)?
            > 0;
        if known {
            report.known += 1;
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
    write(con, &quick, options.dry_run).map_err(db)?;
    println!(
        "bulk: {} texts, {} known, {} names, {} copied, {} with nothing to translate, {} to translate",
        report.read,
        report.known,
        report.names,
        report.copied,
        report.nothing,
        todo.len()
    );

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
    #[test]
    fn map_hash_is_the_game_s() {
        // Row 1102280 as the game sent it to the addons.
        let en = "%str1%%str2%Obsidian Light Gloves%str3%%str4%";
        assert_eq!(format!("{:016x}", xxhash_rust::xxh3::xxh3_64(en.as_bytes())), "07ed9f3d957e55a6");
    }
}
