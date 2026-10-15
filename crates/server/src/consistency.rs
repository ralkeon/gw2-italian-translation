//! The consistency job: `gw2it-server consistency --translators translators.json`, run by cron.
//!
//! The glossary (glossary.rs) fixes a word; it cannot make "Soldato Serafino", "Soldato dei
//! Serafini" and "Guardia dei Serafini" one way of saying it. This job does, once a night:
//!
//! 1. `gw2it_text::consistency::groups` finds the English words that the translations render in
//!    more than one form.
//! 2. Only names are settled: a faction, a race, a place, a character, an organisation ("Seraph",
//!    "Pact"), whose Italian must be one fixed form. A common word ("Black", "Crystal") takes the
//!    forms grammar asks for (nero, nera, neri), and is left alone. The model tells which words
//!    are names, fifty at a time with a few texts each; the answer is kept (`consistency_words`).
//! 3. For each name (the biggest first, a few per run) a language model is asked whether the forms
//!    are really inconsistent and, if so, which single form and construction to use, and to rewrite
//!    the texts that differ. Hand corrections (trust 2 and 3) are never changed: they are the
//!    examples, and what a contributor put in the glossary is the form to follow.
//! 4. The answer is checked: the markup must be the one of the English, the text must have the
//!    chosen form and be different from before. Machine translations (trust 1 and below) are
//!    rewritten, with the old text in `history` and `consistency_log`, and printed; the form chosen
//!    goes into the glossary with trust 1 (below every contributor), with the old forms the model
//!    calls mistakes as wrong ones, so that the next translations come out right.
//!
//! A group the model found consistent, or that the job has settled, is not asked again until its
//! forms change (`consistency_state`).

use gw2it_text::{
    consistency::{self, Entry, Group},
    glossary::Term,
    markup,
};
use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::{glossary, now_ms};

pub const SCHEMA: &str = "
-- The groups the job has looked at, and their forms then.
CREATE TABLE IF NOT EXISTS consistency_state (
    word      TEXT    PRIMARY KEY,
    signature TEXT    NOT NULL,
    checked   INTEGER NOT NULL
);
-- Every text the job rewrote, to undo it if the model got it wrong.
CREATE TABLE IF NOT EXISTS consistency_log (
    id   INTEGER NOT NULL,
    hash TEXT    NOT NULL,
    word TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    at   INTEGER NOT NULL
);
-- Whether the model called the word a name (a group worth settling), once for every word.
CREATE TABLE IF NOT EXISTS consistency_words (
    word    TEXT    PRIMARY KEY,
    name    INTEGER NOT NULL,
    checked INTEGER NOT NULL
);";

/// Texts of a group given to the model: enough to show every form.
const ITEMS_MAX: usize = 40;
const TEXT_MAX: usize = 4000;
/// Words asked about in one request whether they are names, and texts shown with each.
const CLASSIFY_BATCH: usize = 50;
const CLASSIFY_EXAMPLES: usize = 3;
/// The glossary's author for what the job decides.
const AUTHOR: &str = "consistency";

const CLASSIFY: &str = "You help keep the Italian translation of the game Guild Wars 2 consistent. You get \
English words, each with a few game texts that contain it, as JSON. Tell which words are NAMES whose \
Italian translation must be one fixed form everywhere: a faction, group, organisation, place, \
character or unique thing of the game's world (Seraph, Pact, Vigil, Kryta, Priory). Not names: \
common nouns, adjectives, verbs and game terms, even when written with a capital letter in item names \
(Black, Crystal, Lion, Skin, Double, Mini, Glider, Champion, Insight, Torch, Charge), and kinds of \
creature (Centaur, Griffon, Dragon): their Italian takes the forms grammar and context ask for. When in \
doubt, it is not a name.

Answer with JSON: {\"names\": [\"<word>\", ...]}, the words exactly as given; an empty list if none.";

const SYSTEM: &str = "You keep the Italian translation of the game Guild Wars 2 consistent. You get an \
English name of the game's world (a faction, race, place, character...) and the texts that contain it, \
each with its Italian translation, as JSON. Decide whether the Italian renders the name in more than one \
way that a player would see as a mistake: different spellings or number of the same name (Serafino / \
Serafini), or different constructions for the same kind of thing (\"Soldato Serafino\" / \"Soldato dei \
Serafini\" / \"Guardia dei Serafini\"). Differences that grammar requires (plural, possessive, gender \
agreement, a different meaning of the word) are fine; if the word is not really a name, it is fine.

If it is fine, answer {\"consistent\": true}.

Otherwise choose ONE Italian form for the word and ONE construction, and rewrite the texts that differ.
- The name stays in English, as in the original game (\"Inquest\", not \"Inquisizione\"; \"Seraph\", not \
\"Serafini\"), unless \"glossary\" or the hand corrections give an Italian form: then that one. Only the \
words around it are Italian; do not change a construction that is already fine.
- Names of a faction or group are used as in \"<noun> dei <Name>\": \"Guardia dei Seraph\", \"Soldato \
dell'Inquest\".
- Texts with \"editable\": false are hand corrections by the translators: never change them, and follow \
their form. If \"glossary\" is given, it is the form the translators chose: use it.
- Rewrite only texts with \"editable\": true, and only what is needed. Keep every %token%, every \
<tag>...</tag> and every line break exactly as they are; keep names that are left in English.
- Natural Italian, as in a video game.
- The word can be part of different names (\"Thunderhead Peaks\" and \"Thunderhead Keep\" are two places; \
\"Jahai\" alone is not \"Jahai Bluffs\"): never put into a text a word of a name that its English does not have.

Answer with JSON: {\"consistent\": false, \"term\": \"<the form of the word alone, ONE word as it appears in \
the texts, e.g. Seraph or Serafini; never a longer name like Picchi di Thunderhead>\",\"wrong\": [\"<each other form of the word in the texts that is a mistake, \
e.g. Serafino; never a form grammar asks for>\"], \"rewrites\": [{\"n\": <the text's n>, \"it\": \"<the \
new Italian text>\"}]}";

#[derive(Default, Debug)]
pub struct Report {
    pub groups: usize,
    /// Words the model was asked about, and how many of them are names.
    pub classified: usize,
    pub names: usize,
    pub checked: usize,
    pub consistent: usize,
    pub rewritten: usize,
    pub terms: usize,
    /// Why the run stopped early, if it did.
    pub stopped: Option<String>,
}

pub struct Options {
    pub max_groups: usize,
    /// Ask and check, but change nothing.
    pub dry_run: bool,
    /// The hand corrections too, once, to settle what was there before the job (`--include-manual`):
    /// they are rewritten like the machine translations (still hand corrections, same author and
    /// trust), the English name beats their Italian one, and the term goes into the glossary
    /// above the contributors' (trust 3).
    pub include_manual: bool,
}

/// A text the job may rewrite: a machine translation (any text with `include_manual`), with no
/// plural marks (`[pl:"..."]`, made by the addon from the singular: a rewrite would break them).
fn editable(entry: &Entry, include_manual: bool) -> bool {
    (entry.trust <= 1 || include_manual) && !entry.it.contains("[pl:") && !entry.it.contains("[s]")
}

fn load(con: &Connection) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt = con.prepare("SELECT id, hash, en, it, trust FROM translations ORDER BY id, hash")?;
    let rows = stmt.query_map([], |r| {
        Ok(Entry { id: r.get(0)?, hash: r.get(1)?, en: r.get(2)?, it: r.get(3)?, trust: r.get(4)? })
    })?;
    rows.collect()
}

/// Up to `ITEMS_MAX` texts of the group: every form is there, hand corrections first.
fn sample(group: &Group, entries: &[Entry]) -> Vec<usize> {
    let mut chosen: Vec<usize> = group.entries.clone();
    chosen.sort_by_key(|&i| (entries[i].trust <= 1, i));
    chosen.truncate(ITEMS_MAX);
    chosen.sort_unstable();
    chosen
}

/// The glossary term of `word` and its trust, if any.
fn glossary_term(con: &Connection, word: &str) -> rusqlite::Result<Option<(String, i64)>> {
    con.query_row("SELECT it, trust FROM glossary WHERE en = ?1", [word], |r| Ok((r.get(0)?, r.get(1)?))).optional()
}

/// Runs the job. `ask` is the language model: system prompt and user message in, JSON out; an Err
/// stops the run (out of quota, no model), except for an answer that is not usable.
pub fn run(
    con: &Connection,
    ask: &dyn Fn(&str, &str) -> Result<Value, Failure>,
    options: &Options,
) -> rusqlite::Result<Report> {
    let mut report = Report::default();
    let entries = load(con)?;
    let found = consistency::groups(&entries);
    report.groups = found.len();
    // Not settled yet, or changed since.
    let mut open = Vec::new();
    for group in found {
        let known: Option<String> =
            con.query_row("SELECT signature FROM consistency_state WHERE word = ?1", [&group.word], |r| r.get(0)).optional()?;
        if known.as_deref() != Some(group.signature().as_str()) {
            open.push(group);
        }
    }
    let mut names: HashMap<String, bool> = {
        let mut stmt = con.prepare("SELECT word, name FROM consistency_words")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?
    };
    for i in 0..open.len() {
        if report.checked >= options.max_groups {
            break;
        }
        if !names.contains_key(&open[i].word) {
            let batch: Vec<&Group> = open[i..].iter().filter(|g| !names.contains_key(&g.word)).take(CLASSIFY_BATCH).collect();
            if let Err(why) = classify(con, &entries, &batch, ask, options, &mut names, &mut report)? {
                report.stopped = Some(why);
                break;
            }
        }
        let group = &open[i];
        if names.get(&group.word) != Some(&true) {
            continue;
        }
        report.checked += 1;
        match settle(con, &entries, group, ask, options)? {
            Outcome::Consistent => {
                report.consistent += 1;
                remember(con, group, options)?;
            }
            Outcome::Rewrote { texts, term } => {
                report.rewritten += texts;
                report.terms += usize::from(term);
            }
            Outcome::Unusable(why) => eprintln!("consistency: {}: {why}", group.word),
            Outcome::Stop(why) => {
                report.stopped = Some(why);
                break;
            }
        }
    }
    Ok(report)
}

/// Asks the model which words of `batch` are names, and keeps the answer in `names` (and in
/// `consistency_words`, except in a dry run). Err: the run must stop. Words of an answer that is
/// no good are skipped in this run.
fn classify(
    con: &Connection,
    entries: &[Entry],
    batch: &[&Group],
    ask: &dyn Fn(&str, &str) -> Result<Value, Failure>,
    options: &Options,
    names: &mut HashMap<String, bool>,
    report: &mut Report,
) -> rusqlite::Result<Result<(), String>> {
    let words: Vec<Value> = batch
        .iter()
        .map(|g| {
            let examples: Vec<&str> = g.entries.iter().take(CLASSIFY_EXAMPLES).map(|&i| entries[i].en.as_str()).collect();
            json!({ "word": g.word, "texts": examples })
        })
        .collect();
    let answer = match ask(CLASSIFY, &json!({ "words": words }).to_string()) {
        Ok(answer) => answer,
        Err(Failure::Stop(why)) => return Ok(Err(why)),
        Err(Failure::Unusable(why)) => {
            eprintln!("consistency: which words are names: {why}");
            for g in batch {
                names.insert(g.word.clone(), false);
            }
            return Ok(Ok(()));
        }
    };
    let said: Vec<String> =
        answer["names"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(|w| w.as_str()).map(str::to_lowercase).collect();
    let now = now_ms();
    for g in batch {
        let name = said.contains(&g.word);
        names.insert(g.word.clone(), name);
        report.classified += 1;
        report.names += usize::from(name);
        if name {
            eprintln!("consistency: {} is a name", g.word);
        }
        if !options.dry_run {
            con.execute(
                "INSERT OR REPLACE INTO consistency_words (word, name, checked) VALUES (?1, ?2, ?3)",
                params![g.word, name, now],
            )?;
        }
    }
    Ok(Ok(()))
}

/// Why the model could not be asked.
pub enum Failure {
    /// Out of quota, nobody to ask: the run ends.
    Stop(String),
    /// This answer is no good: the group is left for another night.
    Unusable(String),
}

enum Outcome {
    Consistent,
    Rewrote { texts: usize, term: bool },
    Unusable(String),
    Stop(String),
}

fn remember(con: &Connection, group: &Group, options: &Options) -> rusqlite::Result<()> {
    if !options.dry_run {
        con.execute(
            "INSERT OR REPLACE INTO consistency_state (word, signature, checked) VALUES (?1, ?2, ?3)",
            params![group.word, group.signature(), now_ms()],
        )?;
    }
    Ok(())
}

fn settle(
    con: &Connection,
    entries: &[Entry],
    group: &Group,
    ask: &dyn Fn(&str, &str) -> Result<Value, Failure>,
    options: &Options,
) -> rusqlite::Result<Outcome> {
    let chosen = sample(group, entries);
    let all = options.include_manual;
    // Reviewing the hand corrections too: the English name beats the translators' Italian one.
    let existing = if all { None } else { glossary_term(con, &group.word)? };
    let texts: Vec<Value> = chosen
        .iter()
        .enumerate()
        .map(|(n, &i)| json!({ "n": n, "en": entries[i].en, "it": entries[i].it, "editable": editable(&entries[i], all) }))
        .collect();
    let user = json!({ "word": group.word, "glossary": existing.as_ref().map(|(it, _)| it), "texts": texts }).to_string();
    let answer = match ask(SYSTEM, &user) {
        Ok(answer) => answer,
        Err(Failure::Stop(why)) => return Ok(Outcome::Stop(why)),
        Err(Failure::Unusable(why)) => return Ok(Outcome::Unusable(why)),
    };
    if answer["consistent"].as_bool() == Some(true) {
        return Ok(Outcome::Consistent);
    }

    // What the translators chose beats what the model says.
    let decided = existing.as_ref().filter(|(_, trust)| *trust >= 2).map(|(it, _)| it.clone());
    let Some(term) = decided.or_else(|| answer["term"].as_str().map(str::to_owned)) else {
        return Ok(Outcome::Unusable("no term in the answer".into()));
    };
    let candidate = Term { en: group.word.clone(), it: term.clone(), wrong: Vec::new() };
    if !candidate.is_valid() || !consistency::term_fits(&group.word, &term) {
        return Ok(Outcome::Unusable(format!("term {term:?} is not valid")));
    }

    let mut done: Vec<(usize, String)> = Vec::new();
    for rewrite in answer["rewrites"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let (Some(n), Some(new)) = (rewrite["n"].as_u64(), rewrite["it"].as_str()) else {
            continue;
        };
        let Some(&i) = chosen.get(n as usize) else {
            continue;
        };
        let entry = &entries[i];
        let usable = editable(entry, all)
            && new != entry.it
            && !new.trim().is_empty()
            && new.chars().count() <= TEXT_MAX
            && markup::token_mismatch(&entry.en, new).is_empty()
            && new.to_lowercase().contains(&term.to_lowercase())
            && consistency::rewrite_fits(&entry.en, &group.word, &entry.it, new, &term);
        if usable {
            eprintln!("consistency: {}: {} {:?} -> {:?}", group.word, entry.id, entry.it, new);
            done.push((i, new.to_owned()));
        } else {
            eprintln!("consistency: {}: rewrite of text {} refused", group.word, entry.id);
        }
    }
    if done.is_empty() {
        return Ok(Outcome::Unusable("nothing to rewrite".into()));
    }

    // The old forms the model calls mistakes, if the texts have them: never one grammar asks for.
    let found: Vec<&str> = group.variants.iter().filter_map(|v| v.rsplit(' ').next()).collect();
    let wrong: Vec<String> = answer["wrong"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|w| w.as_str())
        .map(str::to_lowercase)
        .filter(|w| found.contains(&w.as_str()) && !w.eq_ignore_ascii_case(&term))
        .collect();
    eprintln!("consistency: {}: term {term:?}, wrong {wrong:?}", group.word);
    let mut set = false;
    if !options.dry_run {
        let now = now_ms();
        for (i, new) in &done {
            let entry = &entries[*i];
            con.execute(
                "INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
                 SELECT id, hash, it, status, author, at, ?3, ?4 FROM translations WHERE id = ?1 AND hash = ?2",
                params![entry.id, entry.hash, AUTHOR, now],
            )?;
            // Only if it is still what the model saw, and still a machine translation (unless
            // `include_manual`: a hand correction stays one, with its author and trust).
            let changed = con.execute(
                "UPDATE translations SET it = ?3, at = ?5,
                     status = CASE WHEN trust >= 2 THEN status ELSE 'auto' END,
                     author = CASE WHEN trust >= 2 THEN author ELSE ?4 END,
                     trust = CASE WHEN trust >= 2 THEN trust ELSE 1 END,
                     seq = (SELECT coalesce(max(seq), 0) FROM translations) + 1
                 WHERE id = ?1 AND hash = ?2 AND it = ?6 AND (trust <= 1 OR ?7)",
                params![entry.id, entry.hash, new, AUTHOR, now, entry.it, all],
            )?;
            if changed > 0 {
                con.execute(
                    "INSERT INTO consistency_log (id, hash, word, old, new, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![entry.id, entry.hash, group.word, entry.it, new, now],
                )?;
            }
        }
        let said = Term { en: group.word.clone(), it: term, wrong };
        set = glossary::put(con, &said, AUTHOR, if all { 3 } else { 1 }, now, 0)?;
    }
    Ok(Outcome::Rewrote { texts: done.len(), term: set })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(include_str!("../../../server/schema.sql")).unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (1, '0000000000000001', 'Seraph Guard', 'Guardia dei Serafini', 'manual', 'Ana', 1, 1, 3),
               (2, '0000000000000002', 'Seraph Soldier', 'Soldato Serafino', 'auto', 'Bea', 1, 2, 0),
               (3, '0000000000000003', 'Seraph %str1%', 'Serafino %str1%', 'auto', 'Bea', 1, 3, 1),
               (4, '0000000000000004', 'Seraph Mage', 'Mago Serafino', 'manual', 'Cia', 1, 4, 2),
               (5, '0000000000000005', 'Seraph Archer', 'Arciere Serafini', 'auto', 'Bea', 1, 5, 1);",
        )
        .unwrap();
        con
    }

    fn it(con: &Connection, id: i64) -> String {
        con.query_row("SELECT it FROM translations WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    /// A model that calls "seraph" a name and always gives `answer` about it.
    fn model(answer: Value) -> impl Fn(&str, &str) -> Result<Value, Failure> {
        move |system: &str, _: &str| Ok(if system == CLASSIFY { json!({"names": ["Seraph"]}) } else { answer.clone() })
    }

    #[test]
    fn rewrites_machine_translations_and_teaches_the_glossary() {
        let con = db();
        let ask = model(json!({
            "consistent": false, "term": "Serafini", "wrong": ["Serafino", "Serafica"],
            "rewrites": [
                {"n": 1, "it": "Soldato dei Serafini"},      // machine: rewritten
                {"n": 2, "it": "Serafini"},                    // markup lost: refused
                {"n": 3, "it": "Mago dei Serafini"},           // a hand correction: never
                {"n": 0, "it": "Guardia dei Serafini ok"},     // a hand correction: never
                {"n": 4, "it": "Arciere dei Serafini"},        // has the name right already: left
                {"n": 9, "it": "Nowhere dei Serafini"},        // not a text of the group
            ]
        }));
        let report = run(&con, &ask, &Options { max_groups: 5, dry_run: false, include_manual: false }).unwrap();
        assert_eq!((report.checked, report.rewritten, report.terms), (1, 1, 1), "{report:?}");
        assert_eq!(it(&con, 2), "Soldato dei Serafini");
        assert_eq!(it(&con, 5), "Arciere Serafini");
        // Hand corrections, and the text whose markup would be lost, are as they were.
        assert_eq!((it(&con, 1), it(&con, 3), it(&con, 4)), ("Guardia dei Serafini".into(), "Serafino %str1%".into(), "Mago Serafino".into()));

        // The old text is in history and in the log; the row is the job's, with a new seq.
        let old: String = con.query_row("SELECT it FROM history WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(old, "Soldato Serafino");
        let logged: i64 = con.query_row("SELECT count(*) FROM consistency_log", [], |r| r.get(0)).unwrap();
        assert_eq!(logged, 1);
        let (author, trust, seq): (String, i64, i64) =
            con.query_row("SELECT author, trust, seq FROM translations WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((author.as_str(), trust), ("consistency", 1));
        assert!(seq > 5);

        // The form goes into the glossary with the old ones as wrong; a contributor can still change it.
        let terms = glossary::all(&con).unwrap();
        assert_eq!(terms, [Term { en: "seraph".into(), it: "Serafini".into(), wrong: vec!["serafino".into()] }]);
        let trust: i64 = con.query_row("SELECT trust FROM glossary", [], |r| r.get(0)).unwrap();
        assert_eq!(trust, 1);
    }

    #[test]
    fn a_consistent_group_is_not_asked_again() {
        let con = db();
        let calls = RefCell::new(0);
        let ask = |system: &str, _: &str| {
            *calls.borrow_mut() += 1;
            Ok(if system == CLASSIFY { json!({"names": ["seraph"]}) } else { json!({"consistent": true}) })
        };
        let options = Options { max_groups: 5, dry_run: false, include_manual: false };
        assert_eq!(run(&con, &ask, &options).unwrap().consistent, 1);
        assert_eq!(run(&con, &ask, &options).unwrap().checked, 0);
        // Once whether it is a name, once about its forms.
        assert_eq!(*calls.borrow(), 2);
        // The texts change: its forms change, and it is looked at again.
        con.execute("UPDATE translations SET it = 'Arciere Serafica' WHERE id = 5", []).unwrap();
        assert_eq!(run(&con, &ask, &options).unwrap().checked, 1);
    }

    #[test]
    fn the_translators_choice_beats_the_models_and_a_dry_run_changes_nothing() {
        let con = db();
        // A contributor decided "Serafim".
        glossary::put(&con, &Term { en: "seraph".into(), it: "Serafim".into(), wrong: vec![] }, "Ana", 2, 1, 0).unwrap();
        let ask = model(json!({
            "consistent": false, "term": "Serafini",
            "rewrites": [{"n": 1, "it": "Soldato dei Serafini"}, {"n": 4, "it": "Arciere dei Serafim"}]
        }));
        let dry = run(&con, &ask, &Options { max_groups: 5, dry_run: true, include_manual: false }).unwrap();
        assert_eq!(dry.rewritten, 1, "only the text with the translators' form");
        assert_eq!(it(&con, 2), "Soldato Serafino");
        let state: i64 = con.query_row("SELECT count(*) FROM consistency_state", [], |r| r.get(0)).unwrap();
        assert_eq!(state, 0);
        run(&con, &ask, &Options { max_groups: 5, dry_run: false, include_manual: false }).unwrap();
        assert_eq!(it(&con, 5), "Arciere dei Serafim");
        assert_eq!(it(&con, 2), "Soldato Serafino");
    }

    #[test]
    fn a_word_that_is_no_name_is_left_alone_and_not_asked_about_again() {
        let con = db();
        let calls = RefCell::new(Vec::new());
        let ask = |system: &str, user: &str| {
            calls.borrow_mut().push(system == CLASSIFY);
            assert!(system != CLASSIFY || user.contains("Seraph Guard"), "examples go with the word: {user}");
            Ok(if system == CLASSIFY { json!({"names": []}) } else { json!({"consistent": false, "term": "Serafini"}) })
        };
        let options = Options { max_groups: 5, dry_run: false, include_manual: false };
        let report = run(&con, &ask, &options).unwrap();
        assert_eq!((report.classified, report.names, report.checked), (1, 0, 0));
        run(&con, &ask, &options).unwrap();
        assert_eq!(*calls.borrow(), [true]);
        assert_eq!(it(&con, 2), "Soldato Serafino");
    }

    #[test]
    fn with_include_manual_the_hand_corrections_follow_the_english_name_too() {
        let con = db();
        // The translators' Italian form, which the English one beats in this run.
        glossary::put(&con, &Term { en: "seraph".into(), it: "Serafini".into(), wrong: vec![] }, "Ana", 3, 1, 0).unwrap();
        let ask = model(json!({
            "consistent": false, "term": "Seraph", "wrong": ["Serafini", "Serafino"],
            "rewrites": [
                {"n": 0, "it": "Guardia dei Seraph"},   // the owner's hand correction
                {"n": 1, "it": "Soldato dei Seraph"},   // machine
                {"n": 3, "it": "Mago dei Seraph"},      // a contributor's hand correction
            ]
        }));
        let report = run(&con, &ask, &Options { max_groups: 5, dry_run: false, include_manual: true }).unwrap();
        assert_eq!(report.rewritten, 3, "{report:?}");
        let row = |id: i64| -> (String, String, String, i64) {
            con.query_row("SELECT it, status, author, trust FROM translations WHERE id = ?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
        };
        // Still hand corrections, by the same people; the machine one is the job's.
        assert_eq!(row(1), ("Guardia dei Seraph".into(), "manual".into(), "Ana".into(), 3));
        assert_eq!(row(4), ("Mago dei Seraph".into(), "manual".into(), "Cia".into(), 2));
        assert_eq!(row(2), ("Soldato dei Seraph".into(), "auto".into(), "consistency".into(), 1));
        // The English name is the glossary's now, above the contributors.
        let (it, trust): (String, i64) = con.query_row("SELECT it, trust FROM glossary", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((it.as_str(), trust), ("Seraph", 3));
    }

    #[test]
    fn running_out_of_quota_stops_the_run() {
        let con = db();
        let ask = |_: &str, _: &str| Err(Failure::Stop("quota".into()));
        let report = run(&con, &ask, &Options { max_groups: 5, dry_run: false, include_manual: false }).unwrap();
        assert_eq!(report.stopped.as_deref(), Some("quota"));
        assert_eq!(it(&con, 2), "Soldato Serafino");
    }
}
