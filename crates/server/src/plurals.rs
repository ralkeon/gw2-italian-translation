//! The plurals of the item names, checked by a language model: `gw2it-server plurals`, once a night
//! (and by hand). The addon marks plurals with rules (gw2it-text plural.rs) and cannot know what a
//! rule does not: that "Funghi" is already a plural of "Mushroom[s]", that "Banana" has a plural
//! although the English has the same word, that the adjective that ends a name agrees with the noun.
//!
//! Only the texts whose English has a plural mark (`[s]`, `[pl:"..."]`) and no `%num`, machine
//! translations only (hand corrections are never touched). For each the model answers the Italian in
//! the singular, with `[pl:"..."]` after every word that changes in the plural; or that it is right.
//! The answer is checked like the review's: the same text once the marks are taken off, nothing but
//! the number may change in what the checks (markup, names, terms, glossary) look at. Answered by
//! hand (byhand.rs) only the marks and the markup are checked, the rest is the person's judgment,
//! and the fix is written as `byhand` (priority.rs); a row of the jobs' order keeps its author.
//! A text (English and Italian) is asked once: `plural_reviewed`. Every rewrite goes into `history`
//! and `plural_log`, and to the addons with /sync.

use gw2it_text::{
    glossary::{self, Term},
    markup,
    names::{self, Names},
    plural::{marks_ok, singular},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::{
    names::{Ask, elided_well, keep},
    now_ms,
    review::BATCH,
};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS plural_reviewed (
    hash    TEXT    NOT NULL,
    it      TEXT    NOT NULL,
    verdict TEXT    NOT NULL,     -- ok, fixed (the model's fix), replaced, refused
    at      INTEGER NOT NULL,
    PRIMARY KEY (hash, it)
);
CREATE TABLE IF NOT EXISTS plural_log (
    hash TEXT    NOT NULL,
    en   TEXT    NOT NULL,
    old  TEXT    NOT NULL,
    new  TEXT    NOT NULL,
    why  TEXT    NOT NULL,
    rows INTEGER NOT NULL,
    at   INTEGER NOT NULL
);";

const TEXT_MAX: usize = 1000;
const AUTHOR: &str = "plurals";

const SYSTEM: &str = "You check the plurals in the Italian names of items of the game Guild Wars 2. \
The game shows one text for one item and the plural for a stack: in the English a word[s] or word[pl:\"Words\"] \
marks the noun that changes in the plural. The Italian must be written the same way: the SINGULAR text, with \
word[pl:\"parole\"] right after every word that changes in the plural, and nothing after the words that do not.
- The text without the marks must be the singular: \"Mushroom[s]\" is \"Fungo[pl:\\\"Funghi\\\"]\", never \"Funghi\".
- The noun, and the adjectives and articles that agree with it, take a mark: \"Sigillo[pl:\\\"Sigilli\\\"] di ghiaccio superiore[pl:\\\"superiori\\\"]\", \
\"Banana[pl:\\\"Banane\\\"]\". The adjective can be after a preposition, still agreeing with the noun.
- Nothing for words that do not change: invariable nouns and loanwords (Sushi, Skin, Kit, Pass, Mini, Tesi), \
proper names, the words of a name (\"Frammento[pl:\\\"Frammenti\\\"] di Zhaitan\"), a word after a preposition that names \
a material or a thing (\"Spada[pl:\\\"Spade\\\"] di ferro\"). In the name of a dye the colour named after a thing \
(ottone, oliva, smeraldo) does not change, nor do the words that describe it.
- The plural is the one of the word that has the mark in the English: find its Italian counterpart. Only that word, and the words that agree with it, change. A word that names what the item is for or made of, after \"per\", \"di\", \"da\", does not change, nor do the words that describe it: \"Aetherized Mace Skin[s]\" is \"Skin per mazza etereizzata\", with NO mark, because \"Skin\" does not change and \"mazza\" is the weapon the skin is for; \"Skin per scudo\" is never \"Skin per scudo[pl:\\\"scudi\\\"]\". In \"Gioiello[pl:\\\"Gioielli\\\"] di ametista intricato[pl:\\\"intricati\\\"]\" the adjective describes the jewel, so it changes. After a preposition only such adjectives change, never a noun: \"Torta[pl:\\\"Torte\\\"] di mele e frutto della passione\" has no other mark. Never replace a word by another word (\"Colorante\" is not \"Tintura\").
- Compound nouns: the mark goes where Italian puts the plural (\"Pesce[pl:\\\"Pesci\\\"] gatto\" is an example of the usual case).
Change nothing else: not a word choice, not the style, not a name or a term, not a quote or a % or a <tag>. The only \
changes you may make are the singular form of a word, and the marks. If the Italian is already right, answer ok.
Answer with JSON: {\"texts\": [{\"n\": <the text's n>, \"ok\": true} for a text that is right, or \
{\"n\": <n>, \"it\": \"<the Italian, singular, with the marks>\", \"why\": \"<what was wrong, in a few Italian words>\"}]}.";

/// A text to check: the English, by hash, with one machine translation of it.
#[derive(Clone, Debug)]
pub struct Text {
    pub hash: String,
    pub en: String,
    pub it: String,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Ok,
    /// `by_hand`: answered by hand (byhand.rs).
    Fixed { it: String, why: String, by_hand: bool },
    Refused(String),
}

#[derive(Default, Debug)]
pub struct Report {
    pub asked: usize,
    pub ok: usize,
    pub fixed: usize,
    pub refused: usize,
    pub rows: usize,
    pub stopped: Option<String>,
}

/// Whether a plural mark comes after a `:`, `(` or `,` (outside the marks): the name ends there
/// ("Ricetta[pl:\"Ricette\"]: Tentacolo"), what follows is not the plural of the item.
fn mark_after_break(marked: &str) -> bool {
    let mut rest = marked;
    let mut broken = false;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("[pl:\"") {
            if broken {
                return true;
            }
            rest = rest.find("\"]").map_or("", |i| &rest[i + 2..]);
            continue;
        }
        broken |= matches!(c, ':' | '(' | ',');
        rest = &rest[c.len_utf8()..];
    }
    false
}

/// Whether a plural mark comes after the first preposition and none before it.
fn complement_marked(marked: &str) -> bool {
    let mut seen_mark = false;
    for word in marked.split_whitespace() {
        let core = singular(word).trim_matches(|c: char| !c.is_alphabetic()).to_lowercase();
        if gw2it_text::plural::STOP.contains(&core.as_str()) {
            return marked[marked.find(word).unwrap_or(0)..].contains("[pl:") && !seen_mark;
        }
        seen_mark |= word.contains("[pl:");
    }
    false
}

/// Up to `max` machine translations whose English has a plural mark, not checked yet, the newest first.
pub fn pending(con: &Connection, max: usize) -> rusqlite::Result<Vec<Text>> {
    let mut stmt = con.prepare_cached(
        "SELECT t.hash, t.en, t.it FROM translations t
         WHERE t.trust <= 1 AND t.it <> t.en AND (instr(t.en, '[s]') > 0 OR instr(t.en, '[pl:') > 0)
           AND instr(t.en, '%num') = 0
           AND NOT EXISTS (SELECT 1 FROM plural_reviewed r WHERE r.hash = t.hash AND r.it = t.it)
         GROUP BY t.hash, t.en, t.it ORDER BY max(t.seq) DESC LIMIT ?1",
    )?;
    stmt.query_map([max as i64], |r| Ok(Text { hash: r.get(0)?, en: r.get(1)?, it: r.get(2)? }))?.collect()
}

/// Why the model's `new` for `text` cannot be used, if it cannot. By hand, only what would break
/// the text in the game.
fn check(names: &Names, terms: &[Term], text: &Text, new: &str, by_hand: bool) -> Option<&'static str> {
    let en = text.en.as_str();
    if new.trim().is_empty() || new.chars().count() > TEXT_MAX || !marks_ok(new) {
        return Some("empty, too long or bad marks");
    }
    if by_hand {
        let broken = !markup::token_mismatch(en, new).is_empty() || markup::stray_percent(new) || markup::wrap_broken(en, new);
        return broken.then_some("markup changed");
    }
    // A mark after a preposition needs one before it: the head noun changes too (it has an adjective
    // that agrees), otherwise it is the complement ("Skin per scudo") that was marked.
    if complement_marked(new) || mark_after_break(new) {
        return Some("a mark on the complement, not on the head noun");
    }
    let (old, new) = (singular(&text.it), singular(new));
    // The same words: one that is not in the old text must be the singular or plural of one that is.
    let stem = |w: &str| w.chars().take(3).collect::<String>().to_lowercase();
    let old_words: Vec<String> = old.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(stem).collect();
    let new_words: Vec<String> = new.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty()).map(stem).collect();
    if new_words.iter().any(|w| !old_words.contains(w)) || old_words.iter().any(|w| !new_words.contains(w)) {
        return Some("a word was replaced or dropped");
    }
    if !names.missing(en, &new).is_empty() || !names::terms_missing(en, &new).is_empty() {
        return Some("a name is missing");
    }
    if names::term_elided(&new) || !elided_well(names, en, &new) {
        return Some("a name is elided");
    }
    if !names::english_brought_in(en, &old, &new, &keep(names, en, &old)).is_empty() {
        return Some("English brought back");
    }
    if !markup::token_mismatch(en, &new).is_empty() || markup::stray_percent(&new) || markup::wrap_broken(en, &new) {
        return Some("markup changed");
    }
    if markup::straight_quotes(&new) != new {
        return Some("curly quotes");
    }
    if glossary::fix(en, &new, terms) != new {
        return Some("a wrong form of the glossary");
    }
    None
}

/// Asks the model about `texts`: one verdict each, None for the ones it did not answer.
pub fn ask(ask: &Ask, names: &Names, terms: &[Term], texts: &[Text]) -> Result<Vec<Option<Verdict>>, String> {
    let list: Vec<Value> = texts.iter().enumerate().map(|(n, t)| json!({ "n": n, "en": t.en, "it": t.it })).collect();
    let answer = ask(SYSTEM, &json!({ "texts": list }).to_string())?;
    let by_hand = answer[crate::byhand::FLAG] == true;
    let mut out: Vec<Option<Verdict>> = texts.iter().map(|_| None).collect();
    for a in answer["texts"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let Some(n) = a["n"].as_u64().map(|n| n as usize).filter(|&n| n < texts.len()) else { continue };
        let text = &texts[n];
        out[n] = Some(match a["it"].as_str().filter(|it| *it != text.it) {
            None if a["ok"] == true || a["it"].is_string() => Verdict::Ok,
            None => continue,
            Some(new) => match check(names, terms, text, new, by_hand) {
                None => Verdict::Fixed { it: new.to_owned(), why: a["why"].as_str().unwrap_or("").chars().take(300).collect(), by_hand },
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
        con.execute("INSERT OR REPLACE INTO plural_reviewed (hash, it, verdict, at) VALUES (?1, ?2, ?3, ?4)", params![text.hash, it, verdict, now])
    };
    let (new, why, author) = match verdict {
        Verdict::Ok => return mark(&text.it, "ok").map(|_| 0),
        Verdict::Refused(_) => return mark(&text.it, "refused").map(|_| 0),
        Verdict::Fixed { it, why, by_hand } => (it, why, if *by_hand { crate::priority::BY_HAND } else { AUTHOR }),
    };
    let ids: Vec<i64> = {
        let mut stmt = con.prepare_cached("SELECT id FROM translations WHERE hash = ?1 AND en = ?2 AND it = ?3 AND trust <= 1 ORDER BY id")?;
        stmt.query_map(params![text.hash, text.en, text.it], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        con.execute(
            "INSERT INTO history (id, hash, it, status, author, at, replaced_by, replaced_at)
             SELECT id, hash, it, status, author, at, ?3, ?4 FROM translations WHERE id = ?1 AND hash = ?2",
            params![id, text.hash, author, now],
        )?;
        // Only the form changes: a row of the order (priority.rs) keeps its author.
        con.execute(
            &format!(
                "UPDATE translations SET it = ?3, at = ?4, status = 'auto', trust = 1,
                     author = CASE WHEN author IN ({}) THEN author ELSE ?5 END,
                     seq = (SELECT coalesce(max(seq), 0) FROM translations) + 1
                 WHERE id = ?1 AND hash = ?2",
                crate::priority::ABOVE_REVIEW
            ),
            params![id, text.hash, new, now, author],
        )?;
    }
    con.execute(
        "INSERT INTO plural_log (hash, en, old, new, why, rows, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![text.hash, text.en, text.it, new, why, ids.len() as i64, now],
    )?;
    mark(&text.it, "replaced")?;
    mark(new, "fixed")?;
    Ok(ids.len())
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
        for (text, verdict) in chunk.iter().zip(verdicts) {
            let Some(verdict) = verdict else { continue };
            match &verdict {
                Verdict::Ok => report.ok += 1,
                Verdict::Fixed { it, why, .. } => {
                    report.fixed += 1;
                    println!("plurals: {:?}\n    {:?}\n -> {:?}\n    ({why})", text.en, text.it, it);
                }
                Verdict::Refused(why) => {
                    report.refused += 1;
                    eprintln!("plurals: {:?}: refused, {why}", text.en);
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
            "INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (1, 'a', 'Mushroom[s]', 'Funghi', 'auto', 'x', 0, 1, 1),
               (2, 'a', 'Mushroom[s]', 'Funghi', 'auto', 'x', 0, 2, 0),
               (3, 'b', 'Banana[s]', 'Banana', 'auto', 'x', 0, 3, 1),
               (4, 'c', 'Sushi[pl:\"Sushi\"]', 'Sushi', 'auto', 'x', 0, 4, 1),
               (5, 'a', 'Mushroom[s]', 'Fungo[pl:\"Funghi\"]', 'manual', 'x', 0, 5, 3),
               (6, 'd', 'Sword', 'Spada', 'auto', 'x', 0, 6, 1);",
        )
        .unwrap();
        con
    }

    fn it(con: &Connection, id: i64) -> String {
        con.query_row("SELECT it FROM translations WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn the_marks() {
        assert_eq!(singular("Sigillo[pl:\"Sigilli\"] di ghiaccio superiore[pl:\"superiori\"]"), "Sigillo di ghiaccio superiore");
        assert!(marks_ok("Fungo[pl:\"Funghi\"]"));
        assert!(!marks_ok("[pl:\"Funghi\"]"));
        assert!(!marks_ok("Fungo[pl:\"\"]"));
        assert!(!marks_ok("Fungo[pl:\"Funghi\""));
        assert!(marks_ok("Fungo"));
    }

    #[test]
    fn a_mark_on_the_complement_is_not_the_head_noun() {
        assert!(complement_marked("Skin per scudo[pl:\"scudi\"] etereizzato"));
        assert!(!complement_marked("Skin per scudo etereizzato"));
        assert!(!complement_marked("Gioiello[pl:\"Gioielli\"] di ametista intricato[pl:\"intricati\"]"));
        assert!(!complement_marked("Rottame[pl:\"Rottami\"] metallico[pl:\"metallici\"]"));
    }

    #[test]
    fn marks_after_the_name_are_refused() {
        assert!(mark_after_break("Ricetta[pl:\"Ricette\"]: Tentacolo[pl:\"Tentacoli\"]"));
        assert!(!mark_after_break("Ricetta[pl:\"Ricette\"]: Tentacolo di Gorseval"));
        assert!(!mark_after_break("Gioiello[pl:\"Gioielli\"] di ametista"));
    }

    #[test]
    fn a_word_replaced_is_refused() {
        let con = db();
        let model = |_: &str, _: &str| -> Result<Value, String> {
            Ok(json!({"texts": [{"n": 2, "it": "Porcino[pl:\"Porcini\"]", "why": "x"}]}))
        };
        let report = run(&con, &model, 100, true).unwrap();
        assert_eq!((report.fixed, report.refused), (0, 1));
    }

    #[test]
    fn a_plural_written_for_a_singular_is_fixed() {
        let con = db();
        let model = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains("Funghi") && user.contains("Banana") && !user.contains("Spada"), "{user}");
            Ok(json!({"texts": [
                {"n": 0, "ok": true},
                {"n": 1, "it": "Banana[pl:\"Banane\"]", "why": "manca il plurale"},
                {"n": 2, "it": "Fungo[pl:\"Funghi\"]", "why": "era al plurale"},
            ]}))
        };
        let report = run(&con, &model, 100, false).unwrap();
        assert_eq!((report.asked, report.ok, report.fixed, report.refused, report.rows), (3, 1, 2, 0, 3));
        assert_eq!(it(&con, 1), "Fungo[pl:\"Funghi\"]");
        assert_eq!(it(&con, 2), "Fungo[pl:\"Funghi\"]");
        assert_eq!(it(&con, 3), "Banana[pl:\"Banane\"]");
        // The hand correction and the text without a plural stay; nothing is asked again.
        assert_eq!(it(&con, 5), "Fungo[pl:\"Funghi\"]");
        assert_eq!(it(&con, 6), "Spada");
        let never = |_: &str, user: &str| -> Result<Value, String> { panic!("asked again: {user}") };
        assert_eq!(run(&con, &never, 100, false).unwrap().asked, 0);
    }

    #[test]
    fn a_row_of_the_order_keeps_its_author_a_fix_by_hand_is_byhand() {
        let con = db();
        con.execute("UPDATE translations SET author = 'names' WHERE id = 1", []).unwrap();
        let text = Text { hash: "a".into(), en: "Mushroom[s]".into(), it: "Funghi".into() };
        let fix = Verdict::Fixed { it: "Fungo[pl:\"Funghi\"]".into(), why: "x".into(), by_hand: true };
        assert_eq!(apply(&con, &text, &fix).unwrap(), 2);
        let author = |id: i64| con.query_row("SELECT author FROM translations WHERE id = ?1", [id], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!((author(1).as_str(), author(2).as_str()), ("names", "byhand"));
    }

    #[test]
    fn what_changes_more_than_the_number_is_refused() {
        let con = db();
        let model = |_: &str, _: &str| -> Result<Value, String> {
            Ok(json!({"texts": [
                {"n": 2, "it": "Fungo[pl:\"\"]", "why": "x"},
                {"n": 1, "it": "L’Banana[pl:\"Banane\"]", "why": "x"},
            ]}))
        };
        let report = run(&con, &model, 100, true).unwrap();
        assert_eq!((report.fixed, report.refused, report.rows), (0, 2, 0));
        assert_eq!(pending(&con, 100).unwrap().len(), 3);
    }
}
