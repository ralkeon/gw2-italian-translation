//! Review of the machine translations by a language model, soon after they reach the server: the
//! server runs it in the background on what /sync and the replication bring (the newest first,
//! then the older ones, a few thousand texts a day), and `gw2it-server review` runs it by hand.
//!
//! On /sync a machine translation already gets what needs no model: the glossary's fixes (the
//! terms of the contributors and of the nightly consistency job) and the names that are the whole
//! text. Here one question to the model does the rest, per text:
//! - a real mistake fixed: a wrong meaning ("Maximum Boon Scaling" is not a "scala"), a part
//!   missing or added, a broken sentence, agreement; not the style;
//! - the names and the terms that stay in English put back (names.rs), the glossary's terms kept.
//!
//! What it answers is checked as the names job checks its own (markup, names, terms, no English
//! word brought back but those); a text that fails stays as it is, and the nightly jobs try again.
//! A text (English and Italian) is reviewed once: `reviewed`. Every rewrite goes into `history`
//! and `review_log` with the model's reason, and to the addons with /sync. Hand corrections are
//! never touched.

use gw2it_text::{
    glossary::{self, Term},
    markup,
    names::{self, Names},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::{
    names::{Ask, elided_well, keep},
    now_ms,
};

pub const SCHEMA: &str = "
-- The texts (English by hash, Italian) the model has seen: not asked again.
CREATE TABLE IF NOT EXISTS reviewed (
    hash    TEXT    NOT NULL,
    it      TEXT    NOT NULL,
    verdict TEXT    NOT NULL,     -- ok, fixed (this is the model's fix), replaced, refused
    at      INTEGER NOT NULL,
    PRIMARY KEY (hash, it)
);
-- Every fix, to undo it if needed.
CREATE TABLE IF NOT EXISTS review_log (
    hash TEXT    NOT NULL,
    en   TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    why  TEXT    NOT NULL,
    rows INTEGER NOT NULL,
    at   INTEGER NOT NULL
);
-- Texts given to the model per day (days since 1970, UTC).
CREATE TABLE IF NOT EXISTS review_usage (
    day   INTEGER PRIMARY KEY,
    texts INTEGER NOT NULL
);";

/// Texts given to the model at once.
pub const BATCH: usize = 20;
const TEXT_MAX: usize = 4000;
const AUTHOR: &str = "review";
/// The kinds of mistake a fix may be for (the model also answers "style").
const FIXES: &[&str] = &["meaning", "missing", "grammar", "markup", "names"];

const SYSTEM: &str = "You review the Italian translations of texts of the game Guild Wars 2, made by \
machine translators. For each text compare the Italian with the English and fix only real mistakes:
- a wrong meaning: a false friend, a word taken in the wrong sense (\"Maximum Boon Scaling\" is how many \
boons count, not a \"scala\"), a game term translated as a plain word;
- something missing or added, a sentence that is broken or makes no sense;
- wrong gender or number agreement, English words left untranslated that are not names.
A word[pl:\"...\"] is followed by the form it takes in the plural. In the name of a dye the colour named after a thing (ottone, oliva, smeraldo) does not change in the plural, nor do the words that describe it: \"Tintura[pl:\\\"Tinture\\\"] ottone radiante\", plural \"Tinture ottone radiante\"; a colour that is a plain adjective agrees: \"Tintura[pl:\\\"Tinture\\\"] verde[pl:\\\"verdi\\\"]\".
Do not change what is correct: not the style, not word choices that are acceptable, not punctuation, apostrophes (’ or ') or quotes, not the same phrase worded otherwise in another text. In a line of a dialogue the speaker is a character you cannot see: keep the gender the translation gave them. If you are not sure something is a mistake, it is not one.

Some English must stay as it is, so that players can talk with players of other languages:
- \"keep\": names of skills, traits, specializations, professions, legendary items, boons and conditions. \
Each must be in the Italian exactly as written in the English; fix only the words right around it \
(\"l'Elisir B\" becomes \"l'Elixir B\").
- The lowercase words in \"keep\" are words of the game's mechanics: \"boon\" (benedizione, vantaggio) and \
\"condition\" (condizione, alterazione). Use the English word as an Italian loanword: it does not change \
in the plural (\"i boon\", \"le condition\"; \"boon\" is masculine, \"condition\" feminine), it is lowercase \
in a sentence and capitalised in a title or label, and it is never elided (\"il boon\", never \"l'boon\"). \
The attributes \"Condition Damage\", \"Condition Duration\" and \"Boon Duration\" stay whole in English. \
If in a text such a word is not the mechanic (\"your help was a great boon\"), leave it translated and \
add \"mechanic\": false.
- \"glossary\": how these English words are translated; use these Italian forms.
\"Black Lion\" in \"keep\" is the name of the company: it stays in English and does not change, with no article or preposition before it (\"forziere Black Lion\", \"statuetta Black Lion\", \"biglietto di riscatto Black Lion\", never \"del Leone Nero\"); \"Black Lion Trading Company\" stays whole in English. Nothing else goes back to English: boons and conditions written in lowercase in the English (\"inflict bleeding\", \"grant might\") stay translated, and so do other attributes (Precisione, Vitalità), skill types and labels \
(\"Incantesimo minore.\") stay in Italian. Keep every %token%, every <tag>, every [tag] and every line \
break exactly as they are. \"%%\" is the percent sign (\"25%%\"): never write a single \"%\", the game cuts the text there.

Answer with JSON: {\"texts\": [{\"n\": <the text's n>, \"ok\": true} for a text that is right, or \
{\"n\": <n>, \"it\": \"<the fixed Italian>\", \"why\": \"<the mistake, in a few Italian words>\", \"kind\": \"<meaning, missing, grammar, markup, names or style>\"}]}. Making a text like the other texts (consistency, uniformity, a standard form) is \"style\".";

/// A text to review: the English, by hash, with one machine translation of it.
#[derive(Clone, Debug)]
pub struct Text {
    pub hash: String,
    pub en: String,
    pub it: String,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Ok,
    Fixed { it: String, why: String },
    /// The model's fix failed a check (the reason): the text stays.
    Refused(String),
}

#[derive(Default, Debug)]
pub struct Report {
    pub asked: usize,
    pub ok: usize,
    pub fixed: usize,
    pub refused: usize,
    /// Rows rewritten (one text may be several rows: the same English under other IDs).
    pub rows: usize,
    pub stopped: Option<String>,
}

fn today() -> i64 {
    now_ms() / 86_400_000
}

/// Texts given to the model today.
pub fn used_today(con: &Connection) -> rusqlite::Result<usize> {
    con.query_row("SELECT coalesce(sum(texts), 0) FROM review_usage WHERE day = ?1", [today()], |r| r.get::<_, i64>(0))
        .map(|n| n as usize)
}

/// Up to `max` machine translations not reviewed yet, the newest first, one per text.
pub fn pending(con: &Connection, max: usize) -> rusqlite::Result<Vec<Text>> {
    // Not the rows of the jobs above the review (priority.rs): they would not be rewritten anyway.
    let mut stmt = con.prepare_cached(&format!(
        "SELECT t.hash, t.en, t.it FROM translations t
         WHERE t.trust <= 1 AND t.it <> t.en AND trim(t.en) <> '' AND t.author NOT IN ({})
           AND NOT EXISTS (SELECT 1 FROM reviewed r WHERE r.hash = t.hash AND r.it = t.it)
         GROUP BY t.hash, t.en, t.it ORDER BY max(t.seq) DESC LIMIT ?1",
        crate::priority::ABOVE_REVIEW
    ))?;
    stmt.query_map([max as i64], |r| Ok(Text { hash: r.get(0)?, en: r.get(1)?, it: r.get(2)? }))?.collect()
}

/// Why the model's `new` for `text` cannot be used, if it cannot.
fn check(names: &Names, terms: &[Term], text: &Text, new: &str, mechanic: bool) -> Option<&'static str> {
    let (en, old) = (text.en.as_str(), text.it.as_str());
    if new.trim().is_empty() || new.chars().count() > TEXT_MAX {
        return Some("empty or too long");
    }
    if !names.missing(en, new).is_empty() {
        return Some("a name is missing");
    }
    if mechanic && !names::terms_missing(en, new).is_empty() {
        return Some("boon or condition is missing");
    }
    if names::term_elided(new) || !elided_well(names, en, new) {
        return Some("a name is elided");
    }
    if !names::english_brought_in(en, old, new, &keep(names, en, old)).is_empty() {
        return Some("English brought back");
    }
    if !markup::token_mismatch(en, new).is_empty() || markup::stray_percent(new) || markup::wrap_broken(en, new) || old.contains("[pl:") != new.contains("[pl:") {
        return Some("markup changed");
    }
    if glossary::fix(en, new, terms) != new {
        return Some("a wrong form of the glossary");
    }
    None
}

/// Asks the model about `texts`: one verdict each, None for the ones it did not answer.
pub fn ask(ask: &Ask, names: &Names, terms: &[Term], texts: &[Text]) -> Result<Vec<Option<Verdict>>, String> {
    let list: Vec<Value> = texts
        .iter()
        .enumerate()
        .map(|(n, t)| {
            let found: Vec<Value> = glossary::relevant(terms, &[t.en.as_str()]).iter().map(|g| json!({ "en": g.en, "it": g.it })).collect();
            let mut v = json!({ "n": n, "en": t.en, "it": t.it });
            let keep = keep(names, &t.en, &t.it);
            if !keep.is_empty() {
                v["keep"] = json!(keep);
            }
            if !found.is_empty() {
                v["glossary"] = json!(found);
            }
            v
        })
        .collect();
    let answer = ask(SYSTEM, &json!({ "texts": list }).to_string())?;
    let mut out: Vec<Option<Verdict>> = texts.iter().map(|_| None).collect();
    for a in answer["texts"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(n) = a["n"].as_u64().map(|n| n as usize).filter(|&n| n < texts.len()) else { continue };
        let text = &texts[n];
        let mechanic = a["mechanic"] != false;
        // Right as it is, but only if nothing is missing.
        let new = a["it"].as_str().filter(|it| *it != text.it);
        let right = |text: &Text| match check(names, terms, text, &text.it, mechanic) {
            None => Verdict::Ok,
            Some(why) => Verdict::Refused(format!("ok, but {why}")),
        };
        out[n] = Some(match new {
            // Only mistakes: a change of style is no fix, the text is right as it is.
            None if a["ok"] == true || a["it"].is_string() => right(text),
            Some(_) if !FIXES.contains(&a["kind"].as_str().unwrap_or("")) => right(text),
            None => continue,
            Some(new) => match check(names, terms, text, new, mechanic) {
                None => Verdict::Fixed { it: new.to_owned(), why: a["why"].as_str().unwrap_or("").chars().take(300).collect() },
                Some(why) => Verdict::Refused(format!("{why}: {new:?}")),
            },
        });
    }
    Ok(out)
}

/// Writes the verdict on `text`: the fix on every machine translation of the same English that
/// still has that Italian. Returns the rows rewritten.
pub fn apply(con: &Connection, text: &Text, verdict: &Verdict) -> rusqlite::Result<usize> {
    let now = now_ms();
    let mark = |it: &str, verdict: &str| {
        con.execute(
            "INSERT OR REPLACE INTO reviewed (hash, it, verdict, at) VALUES (?1, ?2, ?3, ?4)",
            params![text.hash, it, verdict, now],
        )
    };
    let (new, why) = match verdict {
        Verdict::Ok => return mark(&text.it, "ok").map(|_| 0),
        Verdict::Refused(_) => return mark(&text.it, "refused").map(|_| 0),
        Verdict::Fixed { it, why } => (it, why),
    };
    let ids: Vec<i64> = {
        let mut stmt = con.prepare_cached(&format!(
            "SELECT id FROM translations WHERE hash = ?1 AND en = ?2 AND it = ?3 AND trust <= 1 AND author NOT IN ({}) ORDER BY id",
            crate::priority::ABOVE_REVIEW
        ))?;
        stmt.query_map(params![text.hash, text.en, text.it], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        con.execute(
            "INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
             SELECT id, hash, it, status, author, at, ?3, ?4 FROM translations WHERE id = ?1 AND hash = ?2",
            params![id, text.hash, AUTHOR, now],
        )?;
        // Each row its own seq, as upsert.sql does: an addon's pull may stop between two.
        con.execute(
            "UPDATE translations SET it = ?3, at = ?4, status = 'auto', author = ?5, trust = 1,
                 seq = (SELECT coalesce(max(seq), 0) FROM translations) + 1
             WHERE id = ?1 AND hash = ?2",
            params![id, text.hash, markup::straight_quotes(&new), now, AUTHOR],
        )?;
    }
    con.execute(
        "INSERT INTO review_log (hash, en, old, new, why, rows, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![text.hash, text.en, text.it, new, why, ids.len() as i64, now],
    )?;
    mark(&text.it, "replaced")?;
    mark(new, "fixed")?;
    Ok(ids.len())
}

/// Counts `n` texts given to the model today.
pub fn count(con: &Connection, n: usize) -> rusqlite::Result<()> {
    con.execute(
        "INSERT INTO review_usage (day, texts) VALUES (?1, ?2) ON CONFLICT (day) DO UPDATE SET texts = texts + excluded.texts",
        params![today(), n as i64],
    )?;
    Ok(())
}

/// One round: up to `max` texts, BATCH at a time. With `dry_run` nothing is written.
pub fn run(con: &Connection, model: &Ask, max: usize, dry_run: bool) -> rusqlite::Result<Report> {
    let names = crate::names::load(con)?;
    let terms = crate::glossary::all(con)?;
    let mut report = Report::default();
    let todo = pending(con, max)?;
    for chunk in todo.chunks(BATCH) {
        let verdicts = match ask(model, &names, &terms, chunk) {
            Ok(v) => v,
            Err(why) => {
                report.stopped = Some(why);
                break;
            }
        };
        report.asked += chunk.len();
        if !dry_run {
            count(con, chunk.len())?;
        }
        for (text, verdict) in chunk.iter().zip(verdicts) {
            let Some(verdict) = verdict else { continue };
            match &verdict {
                Verdict::Ok => report.ok += 1,
                Verdict::Fixed { it, why } => {
                    report.fixed += 1;
                    println!("review: {:?}\n    {:?}\n -> {:?}\n    ({why})", text.en, text.it, it);
                }
                Verdict::Refused(why) => {
                    report.refused += 1;
                    eprintln!("review: {:?}: refused, {why}", text.en);
                }
            }
            if !dry_run {
                report.rows += apply(con, text, &verdict)?;
            }
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
        con.execute_batch(crate::names::SCHEMA).unwrap();
        con.execute_batch(crate::consistency::SCHEMA).unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO english_names VALUES ('Might', 'boon', 1);
             INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (1, 'a', 'Maximum Boon Scaling', 'Scala massima del Boon', 'auto', 'x', 0, 1, 1),
               (2, 'a', 'Maximum Boon Scaling', 'Scala massima del Boon', 'auto', 'x', 0, 2, 0),
               (3, 'b', 'You gain Might.', 'Ottieni Might.', 'auto', 'x', 0, 3, 1),
               (4, 'c', 'Gain 5 Precision.', 'Ottieni 5 Precisione.', 'auto', 'x', 0, 4, 1),
               (5, 'a', 'Maximum Boon Scaling', 'Boon massimi', 'manual', 'x', 0, 5, 3);",
        )
        .unwrap();
        con
    }

    fn it(con: &Connection, id: i64) -> String {
        con.query_row("SELECT it FROM translations WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn a_mistake_is_fixed_on_every_machine_row_of_the_text_once() {
        let con = db();
        let model = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains("Scala massima") && user.contains("Ottieni Might") && user.contains("Precisione"), "{user}");
            Ok(json!({"texts": [
                {"n": 0, "it": "5 Precision ottenuti.", "why": "x", "kind": "meaning"},
                {"n": 1, "ok": true},
                {"n": 2, "it": "Boon massimi conteggiati", "why": "scaling non è scala", "kind": "meaning"},
            ]}))
        };
        let report = run(&con, &model, 100, false).unwrap();
        assert_eq!((report.asked, report.ok, report.fixed, report.refused, report.rows), (3, 1, 1, 1, 2));
        assert_eq!(it(&con, 1), "Boon massimi conteggiati");
        assert_eq!(it(&con, 2), "Boon massimi conteggiati");
        // The hand correction stays; English brought back is refused.
        assert_eq!(it(&con, 5), "Boon massimi");
        assert_eq!(it(&con, 4), "Ottieni 5 Precisione.");
        let seqs: i64 = con.query_row("SELECT count(DISTINCT seq) FROM translations", [], |r| r.get(0)).unwrap();
        assert_eq!(seqs, 5);
        // Nothing is asked again.
        let never = |_: &str, user: &str| -> Result<Value, String> { panic!("asked again: {user}") };
        assert_eq!(run(&con, &never, 100, false).unwrap().asked, 0);
        assert_eq!(used_today(&con).unwrap(), 3);
    }

    #[test]
    fn what_a_job_above_wrote_is_not_reviewed_nor_rewritten() {
        let con = db();
        con.execute("UPDATE translations SET author = 'names' WHERE id = 3", []).unwrap();
        // The text of row 3 is not even asked about.
        let model = |_: &str, user: &str| -> Result<Value, String> {
            assert!(!user.contains("Ottieni Might"), "{user}");
            Ok(json!({"texts": []}))
        };
        run(&con, &model, 100, false).unwrap();
        assert_eq!(it(&con, 3), "Ottieni Might.");
        // And a fix for a text it shares with another row only reaches the rows it may.
        con.execute("UPDATE translations SET author = 'consistency' WHERE id = 1", []).unwrap();
        let text = Text { hash: "a".into(), en: "Maximum Boon Scaling".into(), it: "Scala massima del Boon".into() };
        let fix = Verdict::Fixed { it: "Boon massimi conteggiati".into(), why: "x".into() };
        assert_eq!(apply(&con, &text, &fix).unwrap(), 1);
        assert_eq!(it(&con, 1), "Scala massima del Boon");
        assert_eq!(it(&con, 2), "Boon massimi conteggiati");
    }

    #[test]
    fn ok_with_a_name_missing_is_refused_and_a_dry_run_writes_nothing() {
        let con = db();
        con.execute("UPDATE translations SET it = 'Ottieni potenza.' WHERE id = 3", []).unwrap();
        let model = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains(r#""keep":["Might"]"#), "{user}");
            Ok(json!({"texts": [{"n": 1, "ok": true}]}))
        };
        let report = run(&con, &model, 100, true).unwrap();
        assert_eq!((report.refused, report.rows), (1, 0));
        assert_eq!(pending(&con, 100).unwrap().len(), 3);
    }
}
