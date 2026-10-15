//! Names that stay in English (gw2it_text::names): `gw2it-server names`, run by cron.
//!
//! - `--fetch` takes the list from the official API: professions, specializations, the skills of
//!   the professions, traits, legendary weapons, armour, backs, trinkets and relics; boons and
//!   conditions are added here. A name, once known, stays (the API drops skills it retires).
//! - Then the machine translations (trust 1 and below) that lost a name get it back: a text that
//!   is the name becomes the name; in any other a language model of translators.json puts the
//!   English name where its Italian is, changing as little as it can (a translator given the
//!   names as placeholders scrambles the sentence around them). What it answers is checked: the
//!   names there, the markup of the English. Hand corrections are only counted. Every rewrite goes
//!   into `history` and `names_log`, and to the addons with /sync.
//! - The words of the mechanics in `gw2it_text::names::TERMS` (boon, condition) are put back the
//!   same way, in any case; where the model says the word is not the mechanic ("a great boon" of
//!   a dialogue) the text goes into `names_skip`, not asked again while it stays the same.

use std::time::Duration;

use gw2it_text::{
    markup,
    names::{self, Names},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use ureq::Agent;

use crate::now_ms;

pub const SCHEMA: &str = "
-- The names that stay in English, from the official API.
CREATE TABLE IF NOT EXISTS english_names (
    name  TEXT    PRIMARY KEY,
    kind  TEXT    NOT NULL,       -- profession, specialization, skill, trait, item, boon, condition
    added INTEGER NOT NULL
);
-- Every text the job rewrote, to undo it if needed.
CREATE TABLE IF NOT EXISTS names_log (
    id   INTEGER NOT NULL,
    hash TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    at   INTEGER NOT NULL
);
-- Texts where a word of TERMS is not the game's mechanic, as the model said: left as they are.
CREATE TABLE IF NOT EXISTS names_skip (
    id   INTEGER NOT NULL,
    hash TEXT    NOT NULL,
    it   TEXT    NOT NULL,
    PRIMARY KEY (id, hash)
);";

const API: &str = "https://api.guildwars2.com/v2";
const PAGE_SIZE: usize = 200;
const BOONS: &[&str] = &[
    "Aegis", "Alacrity", "Fury", "Might", "Protection", "Quickness", "Regeneration", "Resistance", "Resolution",
    "Stability", "Swiftness", "Vigor",
];
const CONDITIONS: &[&str] = &[
    "Bleeding", "Blind", "Burning", "Chilled", "Confusion", "Crippled", "Fear", "Immobile", "Poison", "Slow", "Taunt",
    "Torment", "Vulnerability", "Weakness",
];
const ITEM_TYPES: &[&str] = &["Weapon", "Armor", "Back", "Trinket", "Relic"];
/// Texts given to the model at once.
const BATCH: usize = 20;
const TEXT_MAX: usize = 4000;
const AUTHOR: &str = "names";

const SYSTEM: &str = "You fix the Italian translation of texts of the game Guild Wars 2. In each text, \
the names in \"keep\" (skills, traits, specializations, professions, legendary items, boons, conditions) \
must stay exactly as in the English, not translated, so that players can talk about them with players \
of other languages. Rewrite the Italian changing only what is needed: put each English name where its \
Italian translation is, and fix only the words right around it (articles, prepositions: \"l'Elisir B\" \
becomes \"l'Elixir B\"). Do not translate anything again, do not change the rest. Keep every %token%, \
every <tag>, every [tag] and every line break exactly as they are.

The lowercase words in \"keep\" are words of the game's mechanics: \"boon\" (benedizione, vantaggio) and \"condition\" (condizione, alterazione). Use the English word wherever the Italian translated it, as an Italian loanword: it does not change in the plural (\"i boon\", \"le condition\"; \"boon\" is masculine, \"condition\" feminine), it is lowercase in a sentence and capitalised in a title or label (\"Barriera per Boon\"), and it is never elided (\"il boon\", \"della condition\", never \"l'boon\"). The attributes \"Condition Damage\", \"Condition Duration\" and \"Boon Duration\" stay whole in English (\"+100 Condition Damage\"). \"Black Lion\" in \"keep\" is the name of the company: it stays in English and does not change, with no article or preposition before it (\"forziere Black Lion\", \"statuetta Black Lion\", \"biglietto di riscatto Black Lion\", never \"del Leone Nero\"); \"Black Lion Trading Company\" stays whole in English. Nothing else goes back to English: other attributes (Precisione, Vitalità, Potenza), skill types and labels (\"Incantesimo minore.\", \"Passiva del Sigillo:\") stay as the Italian has them. If in a text the word is not the mechanic (\"your help was a great boon\", \"in good condition\"), answer {\"n\": <n>, \"skip\": true} for it instead.

Answer with JSON: {\"texts\": [{\"n\": <the text's n>, \"it\": \"<the fixed Italian>\"}]}";

/// The names, without the plain English words (gw2it_text::names).
pub fn load(con: &Connection) -> rusqlite::Result<Names> {
    let mut stmt = con.prepare_cached("SELECT name FROM english_names")?;
    let list: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(Names::new(list))
}

/// The names added after `since` (unix ms), and the time of the newest (`since` if none), for /sync.
pub fn pull(con: &Connection, since: i64) -> rusqlite::Result<(Vec<String>, i64)> {
    let mut stmt = con.prepare_cached("SELECT name, added FROM english_names WHERE added > ?1")?;
    let mut newest = since;
    let mut list = Vec::new();
    for row in stmt.query_map([since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (name, added) = row?;
        newest = newest.max(added);
        list.push(name);
    }
    Ok((list, newest))
}

/// A name worth keeping: letters, spaces, apostrophes, hyphens, dots; no placeholder names.
fn usable(name: &str) -> bool {
    let name = name.trim();
    name.chars().count() >= 3
        && name.chars().next().is_some_and(char::is_alphabetic)
        && name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '\'' | '’' | '-' | '.' | ':' | '!'))
}

fn get(agent: &Agent, url: &str) -> Result<(Value, usize), String> {
    let mut response = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let status = response.status().as_u16();
    let pages = response.headers().get("X-Page-Total").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(1);
    let text = response.body_mut().with_config().limit(64 << 20).read_to_string().map_err(|e| format!("{url}: {e}"))?;
    if status != 200 {
        return Err(format!("{url}: HTTP {status}"));
    }
    Ok((serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))?, pages))
}

/// Every entry of an endpoint, page after page.
fn all(agent: &Agent, endpoint: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut page = 0;
    loop {
        let (list, pages) = get(agent, &format!("{API}/{endpoint}?page={page}&page_size={PAGE_SIZE}"))?;
        out.extend(list.as_array().cloned().unwrap_or_default());
        page += 1;
        if page >= pages {
            return Ok(out);
        }
    }
}

/// The names of the official API, by kind.
fn from_api() -> Result<Vec<(String, &'static str)>, String> {
    let agent: Agent = Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).build().into();
    let name = |v: &Value| v["name"].as_str().unwrap_or("").trim().to_owned();
    let mut out = Vec::new();
    for v in all(&agent, "professions")? {
        out.push((name(&v), "profession"));
    }
    for v in all(&agent, "specializations")? {
        out.push((name(&v), "specialization"));
    }
    for v in all(&agent, "skills")? {
        // The players' skills: those of monsters and pets have no profession.
        if v["professions"].as_array().is_some_and(|p| !p.is_empty()) {
            out.push((name(&v), "skill"));
        }
    }
    for v in all(&agent, "traits")? {
        out.push((name(&v), "trait"));
    }
    for v in all(&agent, "items")? {
        if v["rarity"] == "Legendary" && v["type"].as_str().is_some_and(|t| ITEM_TYPES.contains(&t)) {
            out.push((name(&v), "item"));
        }
    }
    out.extend(BOONS.iter().map(|b| ((*b).to_owned(), "boon")));
    out.extend(CONDITIONS.iter().map(|c| ((*c).to_owned(), "condition")));
    out.retain(|(n, _)| usable(n));
    Ok(out)
}

/// Adds the names of the official API. Returns how many are new.
pub fn fetch(con: &Connection) -> Result<usize, String> {
    let list = from_api()?;
    let now = now_ms();
    let tx = con.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut added = 0;
    for (name, kind) in list {
        added += tx
            .execute("INSERT OR IGNORE INTO english_names (name, kind, added) VALUES (?1, ?2, ?3)", params![name, kind, now])
            .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(added)
}

#[derive(Default, Debug)]
pub struct Report {
    /// Machine translations that lost a name: the text is the name, or it is inside a text.
    pub whole: usize,
    pub inside: usize,
    pub rewritten: usize,
    /// The model gave no fix that keeps the names and the markup: left as they are.
    pub failed: usize,
    /// The model said a word of TERMS is not the mechanic there.
    pub skipped: usize,
    /// Hand corrections without a name: only counted.
    pub manual: usize,
    pub stopped: Option<String>,
}

pub struct Options {
    pub dry_run: bool,
    /// Texts given to the model in this run (the ones that are the name do not count).
    pub max_texts: usize,
}

/// The language model: system prompt and user message in, JSON out; an Err stops the run.
pub type Ask<'a> = dyn Fn(&str, &str) -> Result<Value, String> + 'a;

struct Row {
    id: i64,
    hash: String,
    en: String,
    it: String,
}

/// What `it` must keep of `en`: the names, as written in `en`, and the terms.
pub(crate) fn keep<'a>(names: &Names, en: &'a str, it: &str) -> Vec<&'a str> {
    let mut out = names.missing(en, it);
    out.extend(names::terms_missing(en, it));
    out
}

/// The text, if the whole of `en` is a name or a term ("Boons"): it stays as it is.
fn whole(names: &Names, en: &str) -> Option<String> {
    let en = en.trim();
    let term = names::TERMS.iter().any(|(_, forms)| forms.iter().any(|f| en.eq_ignore_ascii_case(f)));
    names.whole(en).map(str::to_owned).or_else(|| term.then(|| en.to_owned()))
}

/// Whether every name of `en` that `it` elides into ("l'Elixir B") starts with a vowel: "l'Toss
/// Elixir H" is not Italian.
pub(crate) fn elided_well(names: &Names, en: &str, it: &str) -> bool {
    names.find(en).into_iter().map(|(from, to)| &en[from..to]).all(|name| {
        let vowel = name.chars().next().is_some_and(|c| "AEIOUHaeiouh".contains(c));
        vowel || it.match_indices(name).all(|(at, _)| !it[..at].ends_with(['\'', '’']))
    })
}

pub fn run(con: &Connection, ask: Option<&Ask>, options: &Options) -> rusqlite::Result<Report> {
    let names = load(con)?;
    let mut report = Report::default();
    let skip: std::collections::HashSet<(i64, String, String)> = {
        let mut stmt = con.prepare("SELECT id, hash, it FROM names_skip")?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?
    };
    let rows: Vec<(Row, i64)> = {
        let mut stmt = con.prepare("SELECT id, hash, en, it, trust FROM translations ORDER BY id, hash")?;
        stmt.query_map([], |r| Ok((Row { id: r.get(0)?, hash: r.get(1)?, en: r.get(2)?, it: r.get(3)? }, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let mut done: Vec<(&Row, String)> = Vec::new();
    let mut todo: Vec<&Row> = Vec::new();
    for (row, trust) in &rows {
        if keep(&names, &row.en, &row.it).is_empty() || skip.contains(&(row.id, row.hash.clone(), row.it.clone())) {
            continue;
        }
        if *trust >= 2 {
            report.manual += 1;
        } else if let Some(name) = whole(&names, &row.en) {
            report.whole += 1;
            done.push((row, name));
        } else {
            report.inside += 1;
            todo.push(row);
        }
    }

    // The model puts the names back.
    let todo = &todo[..todo.len().min(options.max_texts)];
    if let Some(ask) = ask {
        for chunk in todo.chunks(BATCH) {
            let texts: Vec<Value> = chunk
                .iter()
                .enumerate()
                .map(|(n, r)| json!({ "n": n, "en": r.en, "it": r.it, "keep": keep(&names, &r.en, &r.it) }))
                .collect();
            let answer = match ask(SYSTEM, &json!({ "texts": texts }).to_string()) {
                Ok(answer) => answer,
                Err(why) => {
                    report.stopped = Some(why);
                    break;
                }
            };
            let mut fixed = vec![None; chunk.len()];
            let mut skipped = vec![false; chunk.len()];
            for t in answer["texts"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let Some(n) = t["n"].as_u64().map(|n| n as usize).filter(|&n| n < chunk.len()) else { continue };
                if let Some(it) = t["it"].as_str() {
                    fixed[n] = Some(it.to_owned());
                } else if t["skip"] == true {
                    skipped[n] = true;
                }
            }
            for ((row, it), skipped) in chunk.iter().zip(fixed).zip(skipped) {
                // Only a text whose names are fine may be skipped: the names are no guess.
                if skipped && it.is_none() && names.missing(&row.en, &row.it).is_empty() {
                    report.skipped += 1;
                    eprintln!("names: {} {:?}: not the mechanic, skipped", row.id, row.en);
                    if !options.dry_run {
                        con.execute(
                            "INSERT OR REPLACE INTO names_skip (id, hash, it) VALUES (?1, ?2, ?3)",
                            params![row.id, row.hash, row.it],
                        )?;
                    }
                    continue;
                }
                let usable = it.filter(|it| {
                    *it != row.it
                        && !it.trim().is_empty()
                        && it.chars().count() <= TEXT_MAX
                        && keep(&names, &row.en, it).is_empty()
                        && !names::term_elided(it)
                        && names::english_brought_in(&row.en, &row.it, it, &keep(&names, &row.en, &row.it)).is_empty()
                        && markup::token_mismatch(&row.en, it).is_empty()
                        && elided_well(&names, &row.en, it)
                        && row.it.contains("[pl:") == it.contains("[pl:")
                });
                match usable {
                    Some(it) => done.push((row, it)),
                    None => {
                        report.failed += 1;
                        eprintln!("names: {} {:?}: no usable fix", row.id, row.en);
                    }
                }
            }
        }
    }

    let now = now_ms();
    for (row, new) in &done {
        eprintln!("names: {} {:?} -> {:?}", row.id, row.it, new);
        if options.dry_run {
            continue;
        }
        con.execute(
            "INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
             SELECT id, hash, it, status, author, at, ?3, ?4 FROM translations WHERE id = ?1 AND hash = ?2",
            params![row.id, row.hash, AUTHOR, now],
        )?;
        // Only if it is still what the job saw, and still a machine translation.
        let changed = con.execute(
            "UPDATE translations SET it = ?3, at = ?5, status = 'auto', author = ?4, trust = 1,
                 seq = (SELECT coalesce(max(seq), 0) FROM translations) + 1
             WHERE id = ?1 AND hash = ?2 AND it = ?6 AND trust <= 1",
            params![row.id, row.hash, new, AUTHOR, now, row.it],
        )?;
        if changed > 0 {
            report.rewritten += 1;
            con.execute(
                "INSERT INTO names_log (id, hash, old, new, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![row.id, row.hash, row.it, new, now],
            )?;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(include_str!("../../../server/schema.sql")).unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(crate::consistency::SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO english_names VALUES ('Signet of Rage', 'skill', 1), ('Might', 'boon', 1), ('The Dreamer', 'item', 1);
             INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (1, 'a', 'Signet of Rage', 'Sigillo della rabbia', 'auto', 'x', 0, 1, 0),
               (2, 'b', 'You gain Might.', 'Ottieni potenza.', 'auto', 'x', 0, 2, 1),
               (3, 'c', 'Signet of Rage', 'Sigillo della Rabbia', 'manual', 'x', 0, 3, 2),
               (4, 'd', 'Craft The Dreamer.', 'Crea The Dreamer.', 'auto', 'x', 0, 4, 0);",
        )
        .unwrap();
        con
    }

    fn it(con: &Connection, id: i64) -> String {
        con.query_row("SELECT it FROM translations WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn machine_translations_get_their_names_back() {
        let con = db();
        let ask = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains(r#""keep":["Might"]"#), "{user}");
            Ok(json!({"texts": [{"n": 0, "it": "Ottieni Might."}]}))
        };
        let report = run(&con, Some(&ask), &Options { dry_run: false, max_texts: 10 }).unwrap();
        assert_eq!((report.whole, report.inside, report.rewritten, report.manual, report.failed), (1, 1, 2, 1, 0));
        assert_eq!(it(&con, 1), "Signet of Rage");
        assert_eq!(it(&con, 2), "Ottieni Might.");
        // The hand correction and the one that is fine stay.
        assert_eq!(it(&con, 3), "Sigillo della Rabbia");
        assert_eq!(it(&con, 4), "Crea The Dreamer.");
        let (trust, author): (i64, String) = con.query_row("SELECT trust, author FROM translations WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((trust, author.as_str()), (1, "names"));
        let logged: i64 = con.query_row("SELECT count(*) FROM names_log", [], |r| r.get(0)).unwrap();
        assert_eq!(logged, 2);
    }

    #[test]
    fn boons_and_conditions_stay_in_english_where_they_are_the_mechanic() {
        let con = db();
        con.execute_batch(
            "DELETE FROM translations;
             INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (5, 'e', 'Barrier per Boon', 'Barriera per benedizione', 'auto', 'x', 0, 5, 1),
               (6, 'f', 'Your help was a great boon.', 'Il tuo aiuto è stato una grande benedizione.', 'auto', 'x', 0, 6, 1),
               (7, 'g', 'Boons', 'Benedizioni', 'auto', 'x', 0, 7, 1),
               (8, 'h', 'Removes conditions.', 'Rimuove le condizioni.', 'auto', 'x', 0, 8, 1);",
        )
        .unwrap();
        let ask = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains(r#""keep":["boon"]"#) && user.contains(r#""keep":["condition"]"#), "{user}");
            Ok(json!({"texts": [
                {"n": 0, "it": "Barriera per Boon"},
                {"n": 1, "skip": true},
                {"n": 2, "it": "Rimuove l'condition."},
            ]}))
        };
        let report = run(&con, Some(&ask), &Options { dry_run: false, max_texts: 10 }).unwrap();
        assert_eq!((report.whole, report.inside, report.rewritten, report.skipped, report.failed), (1, 3, 2, 1, 1));
        assert_eq!(it(&con, 5), "Barriera per Boon");
        assert_eq!(it(&con, 7), "Boons");
        // Elided: refused.
        assert_eq!(it(&con, 8), "Rimuove le condizioni.");
        // The skipped text is not asked again.
        let never = |_: &str, user: &str| -> Result<Value, String> {
            assert!(!user.contains("great boon"), "{user}");
            Ok(json!({"texts": []}))
        };
        let report = run(&con, Some(&never), &Options { dry_run: false, max_texts: 10 }).unwrap();
        assert_eq!((report.inside, report.skipped), (1, 0));
    }

    #[test]
    fn the_addons_get_the_names_added_since_their_last_sync() {
        let con = db();
        con.execute("INSERT INTO english_names VALUES ('Firebrand', 'specialization', 5)", []).unwrap();
        let (mut all, newest) = pull(&con, 0).unwrap();
        all.sort();
        assert_eq!((all.len(), newest), (4, 5));
        assert_eq!(pull(&con, 0).unwrap().0.len(), 4);
        assert_eq!(pull(&con, 1).unwrap(), (vec!["Firebrand".to_owned()], 5));
        assert_eq!(pull(&con, 5).unwrap(), (vec![], 5));
    }

    #[test]
    fn a_dry_run_or_a_lost_name_changes_nothing() {
        let con = db();
        let lost = |_: &str, _: &str| -> Result<Value, String> { Ok(json!({"texts": [{"n": 0, "it": "Ottieni la potenza."}]})) };
        let report = run(&con, Some(&lost), &Options { dry_run: false, max_texts: 10 }).unwrap();
        assert_eq!((report.rewritten, report.failed), (1, 1));
        assert_eq!(it(&con, 2), "Ottieni potenza.");
        let names = Names::new(["Toss Elixir H", "Elixir B"]);
        assert!(!elided_well(&names, "Toss Elixir H now.", "Lancia l'Toss Elixir H ora."));
        assert!(elided_well(&names, "Drink Elixir B now.", "Bevi l'Elixir B ora."));
        let con = db();
        let report = run(&con, None, &Options { dry_run: true, max_texts: 10 }).unwrap();
        assert_eq!((report.whole, report.rewritten), (1, 0));
        assert_eq!(it(&con, 1), "Sigillo della rabbia");
    }
}
