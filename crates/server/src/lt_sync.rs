//! A copy of our translations adapted to the Local Translator's glossary (glossary_it.json), for the
//! owner of that glossary, who asked that what we give follows its choices:
//! `gw2it-server lt-sync --glossary <file or URL> [--db gw2it.db] [--lt-db lt_sync.db]
//! [--translators translators.json] [--max-texts 2000] [--dry-run]`.
//!
//! Our translations are only read: the copy is a table of its own in gw2it.db, `lt_sync`, and the job
//! writes nothing else. export.sh takes it into the nightly download with the rest of the database:
//! the owner of the glossary reads that table there. For each row of ours, the first that applies:
//! - `exact`: the English is an `exact` entry of the imported glossary, also inside its markup
//!   (`<c=@reminder>Junk</c>`): its Italian;
//! - `term`: the English is one of its `terms`;
//! - `pattern`: a `patterns` rule covers it ("{X} Longbow" = "Arco lungo {X}");
//! - `term-llm`: our Italian renders a term of the sentence in another way: a language model of
//!   translators.json puts the glossary's Italian in its place, changing as little as it can, with the
//!   checks of the names job (the markup and the plural marks kept, no word invented). At most
//!   --max-texts a night; the others wait as `pending`, with our Italian meanwhile, and after three
//!   unusable answers they keep ours (`failed`);
//! - `hybrid`: else our Italian as it is.
//!
//! A name with plural marks ("Large Bone[s]") is looked up in the singular, as the other module does.
//!
//! By hand (byhand.rs): `--ask-to <file> --dry-run` writes the texts the model would get, `--answers
//! <file>` reads an answer for each ({"it": ..., "plain": [...]}); a text with no answer stays as it is,
//! and the rewrite is checked for the markup, the plural marks and the glossary's Italian, not for
//! the words around it (that judgment is the person's). `--from-last` takes the texts from the last
//! ID: a session by hand does not work on the ones the night's model takes first.
//! A row is made again only when our Italian or what the glossary says for it changes. The texts we
//! do not have are not here: the owner of the glossary has them already. The copy follows the
//! imported glossary, not our rules (runes, sigils and relics translated, for one), and nothing of
//! ours reads it.

use std::{
    collections::{BTreeMap, HashMap},
    time::Duration,
};

use gw2it_text::markup;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use ureq::Agent;

use crate::{names, now_ms};

pub const SCHEMA: &str = "
-- Our translations adapted to the imported glossary (lt_sync.rs), for the glossary's owner: it follows
-- that glossary, not our rules, and nothing of ours reads it.
CREATE TABLE IF NOT EXISTS lt_sync (
    id      INTEGER NOT NULL,
    hash    TEXT    NOT NULL,
    en      TEXT    NOT NULL,
    it      TEXT    NOT NULL,
    source  TEXT    NOT NULL,  -- hybrid, exact, term, pattern, term-llm, term-hand, pending, failed
    ours    TEXT    NOT NULL,  -- XXH3-64 of our Italian it was made from: made again when ours changes
    rule    TEXT    NOT NULL,  -- XXH3-64 of what the glossary says for the text: made again when it changes
    tries   INTEGER NOT NULL DEFAULT 0,
    at      INTEGER NOT NULL,  -- unix ms
    PRIMARY KEY (id, hash)
);";

/// Texts given to the model at once.
const BATCH: usize = 20;
const TEXT_MAX: usize = 4000;
/// Unusable answers before a text keeps our Italian.
const TRIES_MAX: i64 = 3;
/// Texts given to the model in a night, by default.
pub const MAX_TEXTS: usize = 2000;
const GLOSSARY_MAX: u64 = 64 << 20;
/// Rows written in one transaction: the server's own writes wait at most a moment.
const STEP: usize = 5000;

const SYSTEM: &str = "You adapt the Italian translation of texts of the game Guild Wars 2 to a glossary. \
Each text has the English (\"en\"), its Italian (\"it\") and \"use\": terms of the English with the Italian the glossary \
wants for them (sometimes the English itself: the name stays in English). Rewrite the Italian changing only what is \
needed: put the wanted Italian of each term where the text renders that term in another way, and fix only the words \
right around it (articles, prepositions). Write the wanted Italian as given, the same words in the same order; \
only its first letter may change case, as the sentence needs. The article or preposition before it follows its first \
letter (\"dell'Arco del Leone\", \"lo Skyscale\", \"della Runa superiore dello studioso\") and its gender. Do not translate \
anything again, do not change the rest, add nothing. Keep every %token%, every <tag>, every [tag] (the plural marks \
[pl:\"...\"] too, after the word they belong to) and every line break exactly as they are. \"notes\", when given, are the \
glossary's advice on some words of the text: follow them.

A term is meant only where the English uses it as the glossary's name or word: where it is a verb, a common word of \
the sentence or another thing (\"might\" a verb, \"focus your mind\", \"Skin Color\" the colour of the skin, not a \
skin of the wardrobe), list its English in \"plain\" and leave that \
part of the Italian as it is. When unsure, leave the Italian: a term left as it was is a small loss, a broken \
sentence is not.

Answer with JSON: {\"texts\": [{\"n\": <the text's n>, \"it\": \"<the fixed Italian>\", \"plain\": [\"<English of a term left as it was>\"]}]}";

/// The imported glossary, ready to look up: its `exact`, `terms` (with the forms the other module
/// also takes), `patterns` and `advice`.
pub struct Glossary {
    exact: HashMap<String, String>,
    terms: BTreeMap<String, String>,
    /// The first word of each term (its first character, when that is no letter) -> the terms, longest first.
    index: HashMap<String, Vec<String>>,
    /// (before {X}, after {X}, Italian), longest first.
    patterns: Vec<(String, String, String)>,
    /// (word in lowercase, advice for the model).
    advice: Vec<(String, String)>,
}

/// What the copy says for a text.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// The glossary's Italian for the whole text: (source, Italian).
    Fixed(&'static str, String),
    /// Terms our Italian renders in another way: (English, the glossary's Italian), for the model.
    Ask(Vec<(String, String)>),
    /// Ours as it is.
    Ours,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_lowercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map(|c| c.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// The first word of `s`, or its first character when that is no letter.
fn first_key(s: &str) -> String {
    let word: String = s.chars().take_while(|&c| is_word(c)).collect();
    if word.is_empty() { s.chars().next().map(String::from).unwrap_or_default() } else { word }
}

/// The length of the markup at the start of `s` that the other module takes as no part of a name:
/// a `<tag>` or a `%token1%` (not `%%`).
fn token_at(s: &str) -> Option<usize> {
    if let Some(rest) = s.strip_prefix('<') {
        let end = rest.find(['<', '>', '\n'])?;
        return rest[end..].starts_with('>').then_some(end + 2);
    }
    let rest = s.strip_prefix('%')?;
    let letters = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
    if letters == 0 {
        return None;
    }
    let digits = rest[letters..].find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len() - letters);
    rest[letters + digits..].starts_with('%').then_some(letters + digits + 2)
}

fn only_tokens(mut s: &str) -> bool {
    while !s.is_empty() {
        match token_at(s) {
            Some(len) => s = &s[len..],
            None => return false,
        }
    }
    true
}

/// `s` as (opening markup, body, closing markup): "<c=@reminder>Pact Longbow</c>".
fn wrap(s: &str) -> (&str, &str, &str) {
    let mut start = 0;
    while let Some(len) = token_at(&s[start..]) {
        start += len;
    }
    let end = s[start..].char_indices().map(|(at, _)| start + at).find(|&at| only_tokens(&s[at..])).unwrap_or(s.len());
    (&s[..start], &s[start..end], &s[end..])
}

/// `s` without its grammar marks (`[s]`, `[pl:"..."]`) and with its markup as a mark that is no word:
/// a name glued to markup is a word, the words inside a tag are none.
fn bare(s: &str) -> String {
    let s = markup::strip_grammar(s);
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(c) = rest.chars().next() {
        match token_at(rest) {
            Some(len) => {
                out.push('\u{1}');
                rest = &rest[len..];
            }
            None => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// For comparing Italian: lowercase, straight quotes.
fn fold(s: &str) -> String {
    markup::straight_quotes(s).to_lowercase()
}

/// Whether `needle` is in `hay` as whole words.
fn has_word(hay: &str, needle: &str) -> bool {
    !needle.is_empty()
        && hay.match_indices(needle).any(|(at, _)| {
            !hay[..at].chars().next_back().is_some_and(is_word) && !hay[at + needle.len()..].chars().next().is_some_and(is_word)
        })
}

/// What the other module takes for {X}: capitalised words ("Pact", "Strong Bandit", "Hunter's") or a number.
fn prefix_words(x: &str) -> bool {
    let number = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let is_number = match x.split_once(['.', ',']) {
        Some((a, b)) => number(a) && number(b),
        None => number(x),
    };
    is_number
        || (!x.is_empty()
            && x.split(' ').all(|w| {
                let mut chars = w.chars();
                chars.next().is_some_and(|c| c.is_ascii_uppercase()) && chars.all(|c| is_word(c) || matches!(c, '\'' | '’' | '-'))
            }))
}

impl Glossary {
    pub fn from_json(g: &Value) -> Result<Self, String> {
        let object = |name: &str| -> BTreeMap<String, String> {
            g[name].as_object().into_iter().flatten().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect()
        };
        let exact: HashMap<String, String> = object("exact").into_iter().collect();
        let mut terms: BTreeMap<String, String> = object("terms").into_iter().filter(|(k, _)| !k.trim().is_empty()).collect();
        if exact.is_empty() && terms.is_empty() {
            return Err("no exact and no terms: not the glossary".into());
        }
        // any_case: the word in lowercase too, with its Italian in lowercase ("rune" = "runa").
        for word in g["any_case"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let word = word.trim();
            let low = lower_first(word);
            if let Some(it) = terms.get(word).map(|it| lower_first(it))
                && low != word
                && !terms.contains_key(&low)
            {
                terms.insert(low, it);
            }
        }
        // A term of two or more words also in lowercase and with only its first letter capitalised
        // ("raid god", "Raid god"); one written in the glossary wins.
        let mut variants = Vec::new();
        for (k, v) in &terms {
            if !k.trim().contains(' ') {
                continue;
            }
            let low = k.to_lowercase();
            let sentence = upper_first(&low);
            if low != *k && !terms.contains_key(&low) {
                variants.push((low.clone(), v.to_lowercase()));
            }
            if sentence != *k && sentence != low && !terms.contains_key(&sentence) {
                variants.push((sentence, upper_first(&v.to_lowercase())));
            }
        }
        for (k, v) in variants {
            terms.entry(k).or_insert(v);
        }
        let mut index: HashMap<String, Vec<String>> = HashMap::new();
        for k in terms.keys() {
            index.entry(first_key(k)).or_default().push(k.clone());
        }
        for list in index.values_mut() {
            list.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()).then(a.cmp(b)));
        }
        let mut patterns: Vec<(String, String, String)> = object("patterns")
            .into_iter()
            .filter_map(|(k, v)| {
                let (before, after) = k.split_once("{X}")?;
                (!after.contains("{X}")).then(|| (before.to_owned(), after.to_owned(), v))
            })
            .collect();
        patterns.sort_by(|a, b| (b.0.len() + b.1.len()).cmp(&(a.0.len() + a.1.len())).then(a.cmp(b)));
        let advice = object("advice").into_iter().map(|(k, v)| (k.trim().to_lowercase(), v)).filter(|(k, _)| !k.is_empty()).collect();
        Ok(Self { exact, terms, index, patterns, advice })
    }

    /// The Italian of a whole text (its body, without markup): (source, Italian).
    fn whole(&self, core: &str) -> Option<(&'static str, String)> {
        if let Some(it) = self.exact.get(core) {
            return Some(("exact", it.clone()));
        }
        if let Some(it) = self.terms.get(core) {
            return Some(("term", it.clone()));
        }
        self.patterns.iter().find_map(|(before, after, it)| {
            let x = (core.len() >= before.len() + after.len() && core.starts_with(before.as_str()) && core.ends_with(after.as_str()))
                .then(|| &core[before.len()..core.len() - after.len()])?;
            prefix_words(x).then(|| ("pattern", it.replace("{X}", x)))
        })
    }

    /// The terms in `s` (bare), leftmost and longest first, as the other module finds them.
    fn find(&self, s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let (mut at, mut after_word) = (0, false);
        while let Some(c) = s[at..].chars().next() {
            if !after_word
                && let Some(term) = self.index.get(&first_key(&s[at..])).and_then(|list| {
                    list.iter().find(|t| s[at..].starts_with(t.as_str()) && !s[at + t.len()..].chars().next().is_some_and(is_word))
                })
            {
                out.push(term.as_str());
                at += term.len();
                after_word = term.chars().next_back().is_some_and(is_word);
                continue;
            }
            after_word = is_word(c);
            at += c.len_utf8();
        }
        out
    }

    /// The terms of `en` that `it` does not render as the glossary: (English, Italian), sorted.
    fn needs(&self, en: &str, it: &str) -> Vec<(String, String)> {
        let it = fold(&bare(it));
        let mut out: Vec<(String, String)> = Vec::new();
        for term in self.find(&bare(en)) {
            let want = &self.terms[term];
            if !has_word(&it, &fold(want)) && !out.iter().any(|(e, _)| e == term) {
                out.push((term.to_owned(), want.clone()));
            }
        }
        out.sort();
        out
    }

    /// The advice of the glossary for the words of `en`.
    fn advice_for(&self, en: &str) -> Vec<&str> {
        let en = bare(en).to_lowercase();
        self.advice.iter().filter(|(word, _)| has_word(&en, word)).map(|(_, text)| text.as_str()).collect()
    }

    pub fn plan(&self, en: &str, it: &str) -> Plan {
        if let Some(x) = self.exact.get(en) {
            return Plan::Fixed("exact", x.clone());
        }
        let (pre, core, post) = wrap(en);
        if let Some((source, x)) = self.whole(core) {
            return Plan::Fixed(source, format!("{pre}{x}{post}"));
        }
        // A name with plural marks ("Large Bone[s]"): its entry is in the singular. An entry with its
        // own plural marks is used as it is, one that keeps the English gives the English with the
        // game's marks; else its Italian with our plural marks, which the model writes.
        let singular = markup::strip_grammar(core);
        if singular != core
            && let Some((source, x)) = self.whole(&singular)
        {
            if x.contains("[pl:") {
                return Plan::Fixed(source, format!("{pre}{x}{post}"));
            }
            if x == singular {
                return Plan::Fixed(source, en.to_owned());
            }
            if fold(&markup::strip_grammar(wrap(it).1)) == fold(&x) {
                return Plan::Ours;
            }
            return Plan::Ask(vec![(singular, x)]);
        }
        let needs = self.needs(en, it);
        if needs.is_empty() { Plan::Ours } else { Plan::Ask(needs) }
    }
}

/// Whether the model's `new` for `old` can go into the copy: the wanted Italian there, the markup,
/// the plural marks and the line breaks of before, and, for a model (`strict`), only words swapped
/// (names::swaps_only).
fn usable(en: &str, old: &str, new: &str, wanted: &[&(String, String)], strict: bool) -> bool {
    let folded = fold(&bare(new));
    let keep: Vec<&str> = wanted.iter().map(|(_, it)| it.as_str()).collect();
    new != old
        && !new.trim().is_empty()
        && new.chars().count() <= TEXT_MAX
        && wanted.iter().all(|(_, it)| has_word(&folded, &fold(it)))
        && markup::token_mismatch(en, new).is_empty()
        && old.contains("[pl:") == new.contains("[pl:")
        && old.matches('\n').count() == new.matches('\n').count()
        && (!strict || names::swaps_only(en, old, new, &keep))
}

pub struct Options {
    pub dry_run: bool,
    /// Texts given to the model in this run.
    pub max_texts: usize,
    /// The texts from the last ID.
    pub from_last: bool,
}

#[derive(Default, Debug)]
pub struct Report {
    pub rows: usize,
    /// Rows written again: new, or our Italian or what the glossary says changed.
    pub made: usize,
    /// Rows of the copy no longer among ours.
    pub removed: usize,
    /// Texts for the model, and how many were given to it in this run.
    pub waiting: usize,
    pub asked: usize,
    pub rewritten: usize,
    /// The model said every term of the text is a plain word there: ours.
    pub plain: usize,
    pub failed: usize,
    /// Rows of the copy by source, after the run.
    pub sources: Vec<(String, i64)>,
    pub stopped: Option<String>,
}

struct Row {
    id: i64,
    hash: String,
    en: String,
    it: String,
}

/// A text for the model: its row, the terms to put in, the unusable answers so far.
type Todo<'a> = (&'a Row, Vec<(String, String)>, i64);

/// The 16 hex digits of the XXH3-64 of `s`, as `translations.hash`.
fn digest(s: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(s.as_bytes()))
}

pub fn run(con: &Connection, glossary: &Glossary, ask: Option<&names::Ask>, options: &Options) -> rusqlite::Result<Report> {
    let mut report = Report::default();
    let rows: Vec<Row> = {
        let mut stmt = con.prepare("SELECT id, hash, en, it FROM translations ORDER BY id, hash")?;
        stmt.query_map([], |r| Ok(Row { id: r.get(0)?, hash: r.get(1)?, en: r.get(2)?, it: r.get(3)? }))?.collect::<rusqlite::Result<_>>()?
    };
    report.rows = rows.len();
    // (id, hash) -> (ours, rule, source, tries)
    let mut there: HashMap<(i64, String), (String, String, String, i64)> = {
        let mut stmt = con.prepare("SELECT id, hash, ours, rule, source, tries FROM lt_sync")?;
        stmt.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))))?
            .collect::<rusqlite::Result<_>>()?
    };

    let now = now_ms();
    let mut todo: Vec<Todo> = Vec::new();
    let mut written = 0;
    con.execute_batch("BEGIN")?;
    for row in &rows {
        let plan = glossary.plan(&row.en, &row.it);
        let (source, it, rule) = match &plan {
            Plan::Fixed(source, x) => (*source, x.as_str(), digest(&format!("{source}\t{x}"))),
            Plan::Ask(needs) => ("pending", row.it.as_str(), digest(&serde_json::to_string(needs).unwrap_or_default())),
            Plan::Ours => ("hybrid", row.it.as_str(), digest("")),
        };
        let ours = digest(&row.it);
        if let Some((old_ours, old_rule, old_source, tries)) = there.remove(&(row.id, row.hash.clone()))
            && old_ours == ours
            && old_rule == rule
        {
            if let (Plan::Ask(needs), "pending") = (plan, old_source.as_str()) {
                todo.push((row, needs, tries));
            }
            continue;
        }
        report.made += 1;
        if !options.dry_run {
            con.prepare_cached(
                "INSERT OR REPLACE INTO lt_sync (id, hash, en, it, source, ours, rule, tries, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
            )?
            .execute(params![row.id, row.hash, row.en, it, source, ours, rule, now])?;
            written += 1;
            if written % STEP == 0 {
                con.execute_batch("COMMIT; BEGIN")?;
            }
        }
        if let Plan::Ask(needs) = plan {
            todo.push((row, needs, 0));
        }
    }
    for (id, hash) in there.keys() {
        report.removed += 1;
        if !options.dry_run {
            con.execute("DELETE FROM lt_sync WHERE id = ?1 AND hash = ?2", params![id, hash])?;
        }
    }
    con.execute_batch("COMMIT")?;

    // The model puts the glossary's terms in.
    report.waiting = todo.len();
    if options.from_last {
        todo.reverse();
    }
    let todo = &todo[..todo.len().min(options.max_texts)];
    if let Some(ask) = ask {
        for chunk in todo.chunks(BATCH) {
            let texts: Vec<Value> = chunk
                .iter()
                .enumerate()
                .map(|(n, (row, needs, _))| {
                    let mut text = json!({
                        "n": n, "en": row.en, "it": row.it,
                        "use": needs.iter().map(|(en, it)| json!({ "en": en, "it": it })).collect::<Vec<_>>(),
                    });
                    let notes = glossary.advice_for(&row.en);
                    if !notes.is_empty() {
                        text["notes"] = json!(notes);
                    }
                    text
                })
                .collect();
            let answer = match ask(SYSTEM, &json!({ "texts": texts }).to_string()) {
                Ok(answer) => answer,
                Err(why) => {
                    report.stopped = Some(why);
                    break;
                }
            };
            report.asked += chunk.len();
            // By hand: a text with no answer stays for later, and the words are the person's judgment.
            let by_hand = answer[crate::byhand::FLAG] == true;
            let mut answered = vec![false; chunk.len()];
            let mut said: Vec<(Option<String>, Vec<String>)> = vec![(None, Vec::new()); chunk.len()];
            for t in answer["texts"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let Some(n) = t["n"].as_u64().map(|n| n as usize).filter(|&n| n < chunk.len()) else { continue };
                let plain = t["plain"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(Value::as_str).map(str::to_owned).collect();
                said[n] = (t["it"].as_str().map(markup::straight_quotes), plain);
                answered[n] = true;
            }
            for (((row, needs, tries), (it, plain)), answered) in chunk.iter().zip(said).zip(answered) {
                if by_hand && !answered {
                    continue;
                }
                let wanted: Vec<&(String, String)> = needs.iter().filter(|(en, _)| !plain.contains(en)).collect();
                let outcome = if wanted.is_empty() {
                    Some(("hybrid", row.it.clone()))
                } else {
                    it.filter(|it| usable(&row.en, &row.it, it, &wanted, !by_hand)).map(|it| (if by_hand { "term-hand" } else { "term-llm" }, it))
                };
                let (source, it, tries) = match outcome {
                    Some(("hybrid", it)) => {
                        report.plain += 1;
                        eprintln!("lt-sync: {} {:?}: plain words, ours", row.id, row.en);
                        ("hybrid", it, *tries)
                    }
                    Some((source, it)) => {
                        report.rewritten += 1;
                        eprintln!("lt-sync: {} {:?} -> {:?}", row.id, row.it, it);
                        (source, it, *tries)
                    }
                    None => {
                        report.failed += 1;
                        eprintln!("lt-sync: {} {:?}: no usable answer", row.id, row.en);
                        (if tries + 1 >= TRIES_MAX { "failed" } else { "pending" }, row.it.clone(), tries + 1)
                    }
                };
                if !options.dry_run {
                    con.execute(
                        "UPDATE lt_sync SET it = ?3, source = ?4, tries = ?5, at = ?6 WHERE id = ?1 AND hash = ?2 AND ours = ?7",
                        params![row.id, row.hash, it, source, tries, now_ms(), digest(&row.it)],
                    )?;
                }
            }
        }
    }
    let mut stmt = con.prepare("SELECT source, count(*) FROM lt_sync GROUP BY source ORDER BY source")?;
    report.sources = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(report)
}

/// The glossary from a file or an http(s) address.
fn read_glossary(from: &str) -> Result<Value, String> {
    let text = if from.starts_with("http://") || from.starts_with("https://") {
        let agent: Agent = Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).http_status_as_error(false).build().into();
        let mut response = agent.get(from).call().map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        response.body_mut().with_config().limit(GLOSSARY_MAX).read_to_string().map_err(|e| e.to_string())?
    } else {
        std::fs::read_to_string(from).map_err(|e| e.to_string())?
    };
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// `gw2it-server lt-sync`: see the top of this file. `ask`: the model, or the answers by hand.
pub fn command(db: &str, from: &str, ask: Option<&names::Ask>, options: &Options) {
    let fail = |what: String| -> ! {
        eprintln!("lt-sync: {what}");
        std::process::exit(1);
    };
    let glossary = read_glossary(from).and_then(|g| Glossary::from_json(&g)).unwrap_or_else(|e| fail(format!("glossary: {e}")));
    let con = crate::open(db).and_then(|c| c.execute_batch(SCHEMA).map(|_| c)).unwrap_or_else(|e| fail(format!("cannot open {db}: {e}")));
    match run(&con, &glossary, ask, options) {
        Ok(r) => println!(
            "lt-sync: {} rows, {} made again, {} removed; model: {} waiting, {} asked, {} rewritten, {} plain, {} failed; copy: {}{}{}",
            r.rows,
            r.made,
            r.removed,
            r.waiting,
            r.asked,
            r.rewritten,
            r.plain,
            r.failed,
            r.sources.iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", "),
            if options.dry_run { " (dry run: nothing changed)" } else { "" },
            r.stopped.map(|why| format!("; stopped: {why}")).unwrap_or_default(),
        ),
        Err(e) => fail(format!("database: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glossary() -> Glossary {
        Glossary::from_json(&json!({
            "exact": {"Junk": "Ciarpame", "Large Bone": "Large Bone", "Hard Wood Pulp": "Polpa di legno duro",
                      "Superior Rune of the Scholar": "Runa superiore dello studioso"},
            "terms": {"Lion's Arch": "Arco del Leone", "Raid God": "Dio dei raid", "Charr": "Charr", "Rune": "Runa",
                      "Greatsword": "Spadone"},
            "patterns": {"{X} Longbow": "Arco lungo {X}", "+{X}%% Might Duration": "+{X}%% Durata della Might"},
            "any_case": ["Rune"],
            "advice": {"staff": "\"Staff\" as a weapon is \"staffa\"."}
        }))
        .unwrap()
    }

    #[test]
    fn whole_texts_take_the_glossary_s_italian() {
        let g = glossary();
        assert_eq!(g.plan("Junk", "Spazzatura"), Plan::Fixed("exact", "Ciarpame".into()));
        assert_eq!(g.plan("<c=@reminder>Junk</c>", "x"), Plan::Fixed("exact", "<c=@reminder>Ciarpame</c>".into()));
        assert_eq!(g.plan("Greatsword", "Spada a due mani"), Plan::Fixed("term", "Spadone".into()));
        assert_eq!(g.plan("Pact Longbow", "Arco lungo del Patto"), Plan::Fixed("pattern", "Arco lungo Pact".into()));
        assert_eq!(g.plan("Hunter's Longbow", "x"), Plan::Fixed("pattern", "Arco lungo Hunter's".into()));
        assert_eq!(g.plan("+5%% Might Duration", "x"), Plan::Fixed("pattern", "+5%% Durata della Might".into()));
        // {X} is capitalised words or a number.
        assert_eq!(g.plan("a Longbow", "un arco lungo"), Plan::Ours);
        // Plural marks: the singular's entry.
        assert_eq!(g.plan("Large Bone[s]", "Osso[pl:\"Ossa\"] grande"), Plan::Fixed("exact", "Large Bone[s]".into()));
        assert_eq!(g.plan("Hard Wood Pulp[s]", "Polpa[pl:\"Polpe\"] di legno duro"), Plan::Ours);
        assert_eq!(
            g.plan("Hard Wood Pulp[s]", "Pasta[pl:\"Paste\"] di legno duro"),
            Plan::Ask(vec![("Hard Wood Pulp".into(), "Polpa di legno duro".into())])
        );
    }

    #[test]
    fn terms_in_a_sentence_are_found_as_the_other_module_finds_them() {
        let g = glossary();
        assert_eq!(g.plan("Meet me in Lion's Arch.", "Incontrami all'Arco del Leone."), Plan::Ours);
        assert_eq!(
            g.plan("Meet me in Lion's Arch.", "Incontrami a Lion's Arch."),
            Plan::Ask(vec![("Lion's Arch".into(), "Arco del Leone".into())])
        );
        // Two words: lowercase and sentence case too; one word: only as written, unless any_case.
        assert_eq!(g.plan("Pray to the raid god.", "Prega il dio del raid."), Plan::Ask(vec![("raid god".into(), "dio dei raid".into())]));
        assert_eq!(g.plan("Slot a rune.", "Inserisci una rune."), Plan::Ask(vec![("rune".into(), "runa".into())]));
        assert_eq!(g.plan("A charr greatsword.", "Uno spadone charr."), Plan::Ours);
        // Not inside a longer word, nor inside a tag; glued to markup it counts.
        assert_eq!(g.plan("The Runes are here.", "Le rune sono qui."), Plan::Ours);
        assert_eq!(g.plan("<c=@Charr>Hello</c>", "<c=@Charr>Ciao</c>"), Plan::Ours);
        assert_eq!(g.plan("Go to<br>Lion's Arch", "Vai a<br>Lion's Arch"), Plan::Ask(vec![("Lion's Arch".into(), "Arco del Leone".into())]));
        // The longest first: the rune's whole name, not "Rune".
        assert_eq!(
            g.find(&bare("Use a Superior Rune of the Scholar.")),
            ["Rune"],
            "only the terms: the rune's name is an exact entry"
        );
        assert_eq!(g.advice_for("Wield a staff."), ["\"Staff\" as a weapon is \"staffa\"."]);
    }

    #[test]
    fn the_model_s_answer_is_checked() {
        let w = ("Lion's Arch".to_owned(), "Arco del Leone".to_owned());
        let en = "Meet me in %str1%Lion's Arch%str2%.";
        let old = "Incontrami a %str1%Lion's Arch%str2%.";
        assert!(usable(en, old, "Incontrami all'%str1%Arco del Leone%str2%.", &[&w], true));
        // The markup lost, the term missing, a word invented.
        assert!(!usable(en, old, "Incontrami all'Arco del Leone.", &[&w], true));
        assert!(!usable(en, old, "Incontrami a %str1%Lion's Arch%str2%, presto.", &[&w], true));
        assert!(!usable(en, old, "Incontrami subito all'%str1%Arco del Leone%str2%.", &[&w], true));
        // By hand: the words around are the person's, the markup and the term are still checked.
        assert!(usable(en, old, "Incontrami subito all'%str1%Arco del Leone%str2%.", &[&w], false));
        assert!(!usable(en, old, "Incontrami all'Arco del Leone.", &[&w], false));
    }

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(include_str!("../../../server/schema.sql")).unwrap();
        con.execute_batch(SCHEMA).unwrap();
        con.execute_batch(
            "INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (1, 'a', 'Junk', 'Spazzatura', 'auto', 'x', 0, 1, 1),
               (2, 'b', 'Meet me in Lion''s Arch.', 'Incontrami a Lion''s Arch.', 'auto', 'x', 0, 2, 1),
               (3, 'c', 'Hello.', 'Ciao.', 'manual', 'x', 0, 3, 3),
               (4, 'd', 'Pray to the raid god.', 'Prega il dio del raid.', 'auto', 'x', 0, 4, 1);",
        )
        .unwrap();
        con
    }

    fn row(con: &Connection, id: i64) -> (String, String, i64) {
        con.query_row("SELECT it, source, tries FROM lt_sync WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap()
    }

    #[test]
    fn the_copy_follows_the_glossary_and_is_made_again_only_on_a_change() {
        let con = db();
        let g = glossary();
        let options = Options { dry_run: false, max_texts: 10, from_last: false };
        let ask = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains(r#""use":[{"en":"Lion's Arch","it":"Arco del Leone"}]"#), "{user}");
            Ok(json!({"texts": [{"n": 0, "it": "Incontrami all'Arco del Leone."}, {"n": 1, "it": "Prega il dio del raid."}]}))
        };
        let r = run(&con, &g, Some(&ask), &options).unwrap();
        assert_eq!((r.rows, r.made, r.waiting, r.asked, r.rewritten, r.failed), (4, 4, 2, 2, 1, 1));
        assert_eq!(row(&con, 1), ("Ciarpame".into(), "exact".into(), 0));
        assert_eq!(row(&con, 2), ("Incontrami all'Arco del Leone.".into(), "term-llm".into(), 0));
        assert_eq!(row(&con, 3), ("Ciao.".into(), "hybrid".into(), 0));
        assert_eq!(row(&con, 4), ("Prega il dio del raid.".into(), "pending".into(), 1));
        // Ours untouched.
        let it: String = con.query_row("SELECT it FROM translations WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(it, "Spazzatura");

        // Again: only the pending text is asked; after three unusable answers it keeps ours.
        let only_pending = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.contains("raid god") && !user.contains("Lion"), "{user}");
            Ok(json!({"texts": []}))
        };
        let r = run(&con, &g, Some(&only_pending), &options).unwrap();
        assert_eq!((r.made, r.asked, r.failed), (0, 1, 1));
        run(&con, &g, Some(&only_pending), &options).unwrap();
        assert_eq!(row(&con, 4), ("Prega il dio del raid.".into(), "failed".into(), 3));
        let never = |_: &str, user: &str| -> Result<Value, String> { panic!("asked again: {user}") };
        let r = run(&con, &g, Some(&never), &options).unwrap();
        assert_eq!((r.made, r.waiting), (0, 0));

        // Our Italian changed: made again; a row gone from ours: gone from the copy.
        con.execute_batch("UPDATE translations SET it = 'Ciao!' WHERE id = 3; DELETE FROM translations WHERE id = 1;").unwrap();
        let r = run(&con, &g, Some(&never), &options).unwrap();
        assert_eq!((r.made, r.removed), (1, 1));
        assert_eq!(row(&con, 3).0, "Ciao!");
        let left: i64 = con.query_row("SELECT count(*) FROM lt_sync", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 3);
    }

    #[test]
    fn plain_words_keep_ours_and_a_dry_run_writes_nothing() {
        let con = db();
        let g = glossary();
        let r = run(&con, &g, None, &Options { dry_run: true, max_texts: 10, from_last: false }).unwrap();
        assert_eq!((r.made, r.waiting), (4, 2));
        let n: i64 = con.query_row("SELECT count(*) FROM lt_sync", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let plain = |_: &str, _: &str| -> Result<Value, String> {
            Ok(json!({"texts": [{"n": 0, "it": "Incontrami a Lion's Arch.", "plain": ["Lion's Arch"]}, {"n": 1, "plain": ["raid god"]}]}))
        };
        let r = run(&con, &g, Some(&plain), &Options { dry_run: false, max_texts: 1, from_last: false }).unwrap();
        // max_texts: one text this time.
        assert_eq!((r.asked, r.plain), (1, 1));
        assert_eq!(row(&con, 2), ("Incontrami a Lion's Arch.".into(), "hybrid".into(), 0));
        assert_eq!(row(&con, 4).1, "pending");
    }

    #[test]
    fn answers_by_hand_take_the_last_texts_first_and_leave_the_unanswered_ones() {
        let con = db();
        let g = glossary();
        let options = Options { dry_run: false, max_texts: 10, from_last: true };
        // Only the raid god answered, with a word of its own: by hand, accepted.
        let hand = |_: &str, user: &str| -> Result<Value, String> {
            assert!(user.find("raid god").unwrap() < user.find("Lion").unwrap(), "{user}");
            Ok(json!({"texts": [{"n": 0, "it": "Prega il buon dio dei raid."}], crate::byhand::FLAG: true}))
        };
        let r = run(&con, &g, Some(&hand), &options).unwrap();
        assert_eq!((r.asked, r.rewritten, r.failed), (2, 1, 0));
        assert_eq!(row(&con, 4), ("Prega il buon dio dei raid.".into(), "term-hand".into(), 0));
        assert_eq!(row(&con, 2), ("Incontrami a Lion's Arch.".into(), "pending".into(), 0));
    }
}
