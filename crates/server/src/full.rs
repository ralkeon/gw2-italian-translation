//! gw2it-server full --names names_it.toml --terms terms.toml [--ask-to <file> | --answers <file>] [--max-texts N] [--dry-run]
//!
//! The all-Italian variant (full/PHASE2.md): only the rows whose text differs from the hybrid one, in
//! the table `full`; the addon in full mode reads `full` first, else `translations`. A row differs when
//! the hybrid keeps in English a word the full variant translates: a name of `names_it.toml` (the names
//! the hybrid keeps in English) or an approved term of `terms.toml` (boons, attributes, stat prefixes,
//! races, places...), found in the English text and still written in English in the hybrid Italian.
//!
//! - A text that is only that name or term (inside its markup: "%str1%Godforged Greatsword%str2%") is
//!   written at once with the full variant's Italian (`source` name).
//! - The others need a rewrite: the English words out, the Italian ones in, articles and agreement fixed.
//!   `--ask-to <file>` writes them, one JSON line each ({"id", "hash", "en", "it": the hybrid Italian,
//!   "swap": [[English, Italian, kind]...], "refs": the Local Translator's Italian and GW1 names, to
//!   understand only: refs.rs}), up to --max-texts; `--answers <file>` reads {"hash", "id",
//!   "answer"} lines and writes each answer that keeps the markup and the plural marks and has none of
//!   the English words left (`source` hand). The full-Italian agent answers them (agent/AGENT-full.md).
//!
//! A row of `full` remembers the hybrid text it was made from (`base`, a digest): when the hybrid row
//! changes, the full row is dropped and made again (at once for a name, by the agent for a rewrite).
//! The words list is kept in `full_terms` (replaced on every run from the two files).

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::now_ms;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS full_terms (
    en   TEXT PRIMARY KEY,
    it   TEXT NOT NULL,
    kind TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS full (
    id     INTEGER NOT NULL,
    hash   TEXT    NOT NULL,
    it     TEXT    NOT NULL,
    base   TEXT    NOT NULL,  -- XXH3-64 of the hybrid Italian it was made from
    source TEXT    NOT NULL,  -- name (the whole text is a name or term), hand (rewritten by the agent)
    at     INTEGER NOT NULL,
    seq    INTEGER NOT NULL,
    PRIMARY KEY (id, hash)
);
CREATE INDEX IF NOT EXISTS full_seq ON full (seq);
";

pub enum ByHand {
    AskTo(String),
    Answers(String),
}

pub struct Options {
    pub max_texts: usize,
    pub dry_run: bool,
    /// Every row to the agent, also those without a listed word: the hybrid keeps in English
    /// places, titles and epithets the lists do not have yet ("Cathedral of Zephyrs", "Laranthir of
    /// the Wild"), which the all-Italian variant translates too. The agent decides each one with
    /// the game's fr/es/de (the agents' pilot).
    pub all: bool,
    pub by_hand: Option<ByHand>,
}

#[derive(Default, Debug)]
pub struct Report {
    pub words: usize,
    pub rows: usize,
    pub same: usize,
    pub kept: usize,
    pub stale: usize,
    pub names: usize,
    pub to_rewrite: usize,
    pub asked: usize,
    pub written: usize,
    pub refused: usize,
}

fn digest(s: &str) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(s.as_bytes()))
}

/// The words of the two files: English -> (Italian, kind). The names first (the more specific), the
/// approved terms after; a word whose Italian is the English, or shorter than 3 letters, is no swap.
pub fn words(names: &str, terms: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for (file, key, approved_only) in [(names, "name", false), (terms, "term", true)] {
        let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
        let table: toml::Table = text.parse().map_err(|e| format!("{file}: {e}"))?;
        for entry in table.get(key).and_then(|v| v.as_array()).into_iter().flatten() {
            let (Some(en), Some(it)) = (entry.get("en").and_then(|v| v.as_str()), entry.get("it").and_then(|v| v.as_str())) else {
                continue;
            };
            if approved_only && entry.get("status").and_then(|v| v.as_str()) != Some("approved") {
                continue;
            }
            let kind = entry.get("kind").and_then(|v| v.as_str()).unwrap_or(key);
            // The full variant keeps the English word too ("Mithril" = "di Mithril"): nothing to swap.
            if en.chars().count() < 3 || en == it || has_word(it, en) || !seen.insert(en.to_owned()) {
                continue;
            }
            out.push((en.to_owned(), it.to_owned(), kind.to_owned()));
        }
    }
    Ok(out)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}' || c == '-'
}

/// Whether `word` is in `s` as a whole word (not inside a longer one).
pub fn has_word(s: &str, word: &str) -> bool {
    let mut from = 0;
    while let Some(at) = s[from..].find(word) {
        let start = from + at;
        let end = start + word.len();
        let before = s[..start].chars().next_back();
        let after = s[end..].chars().next();
        // An apostrophe before is an elision ("dell'Iron Legion"): the word starts there.
        let before_ok = before.is_none_or(|c| !is_word(c) || c == '\'' || c == '\u{2019}');
        let after_ok = after.is_none_or(|c| !is_word(c) || (c == '\'' && s[end..].starts_with("'s")));
        if before_ok && after_ok {
            return true;
        }
        from = start + word.len().max(1);
    }
    false
}

/// Whether the Italian still has the English word: as it is, or a stat prefix without its "'s"
/// (the hybrid writes "Zaino Apothecary" for "Apothecary's").
pub fn kept(it: &str, en: &str) -> bool {
    has_word(it, en) || en.strip_suffix("'s").is_some_and(|stem| has_word(it, stem))
}

/// The markup the game needs untouched: placeholders and tags.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                if let Some(end) = s[i + 1..].find('%') {
                    let t = &s[i..i + end + 2];
                    if t[1..t.len() - 1].chars().all(|c| c.is_ascii_alphanumeric()) && t.len() > 2 {
                        out.push(t.to_owned());
                        i += t.len();
                        continue;
                    }
                }
            }
            b'<' => {
                if let Some(end) = s[i..].find('>') {
                    out.push(s[i..i + end + 1].to_owned());
                    i += end + 1;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.sort();
    out
}

/// The English text without markup and plural marks.
fn bare(en: &str) -> String {
    let mut s = String::with_capacity(en.len());
    let mut rest = en;
    while !rest.is_empty() {
        if rest.starts_with('%') || rest.starts_with('<') || rest.starts_with('[') {
            let close = match rest.as_bytes()[0] {
                b'%' => rest[1..].find('%').map(|i| i + 2),
                b'<' => rest.find('>').map(|i| i + 1),
                _ => rest.find(']').map(|i| i + 1),
            };
            if let Some(n) = close {
                rest = &rest[n..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        s.push(c);
        rest = &rest[c.len_utf8()..];
    }
    s.trim().to_owned()
}

struct Todo {
    id: i64,
    hash: String,
    en: String,
    it: String,
    swap: Vec<(String, String, String)>,
}

/// An answer by hand that can go into `full`: the markup as in the hybrid text; plural marks when the
/// hybrid has some, and new ones only when the English has some (how many is the Italian's business:
/// "Skin[s]" is "Skin" in the hybrid, invariable, but "Aspetto[pl:\"Aspetti\"]" here; "Città" needs
/// none); every `[pl:"` closed; none of the English words left (the ones the hybrid keeps in English in
/// both variants are no swap).
pub fn usable(en: &str, hybrid: &str, answer: &str, swap: &[(String, String, String)]) -> bool {
    let plural = |s: &str| s.contains("[s]") || s.contains("[pl:");
    let closed = answer.split("[pl:").skip(1).all(|rest| rest.starts_with('"') && rest[1..].contains("\"]"));
    !answer.trim().is_empty()
        && tokens(hybrid) == tokens(answer)
        && closed
        && (!plural(hybrid) || plural(answer))
        && (!plural(answer) || plural(en) || plural(hybrid))
        && swap.iter().all(|(en, it, _)| !kept(answer, en) || kept(it, en))
}

/// What changed in `full` after `since`, oldest first, at most `max`: ([id, hash, it, seq]..., the last seq,
/// more). An empty `it` is a row taken out. For /sync (main.rs), to an addon that asks (`fsince`).
pub fn pull(con: &Connection, since: i64, max: i64) -> rusqlite::Result<(Vec<Value>, i64, bool)> {
    con.execute_batch(SCHEMA)?;
    let mut stmt = con.prepare_cached("SELECT id, hash, it, seq FROM full WHERE seq > ?1 ORDER BY seq LIMIT ?2")?;
    let rows: Vec<Value> = stmt
        .query_map(params![since, max], |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?])))?
        .collect::<rusqlite::Result<_>>()?;
    let seq = rows.last().and_then(|r| r[3].as_i64()).unwrap_or(since);
    let more = rows.len() as i64 == max;
    Ok((rows, seq, more))
}

pub fn run(con: &Connection, names: &str, terms: &str, options: &Options) -> Result<Report, String> {
    let db = |e: rusqlite::Error| format!("database: {e}");
    con.execute_batch(SCHEMA).map_err(db)?;
    let list = words(names, terms)?;
    let mut report = Report { words: list.len(), ..Report::default() };
    if !options.dry_run {
        let tx = con.unchecked_transaction().map_err(db)?;
        tx.execute("DELETE FROM full_terms", []).map_err(db)?;
        for (en, it, kind) in &list {
            tx.execute("INSERT INTO full_terms (en, it, kind) VALUES (?1, ?2, ?3)", params![en, it, kind]).map_err(db)?;
        }
        tx.commit().map_err(db)?;
    }
    let by_en: HashMap<&str, (&str, &str)> = list.iter().map(|(e, i, k)| (e.as_str(), (i.as_str(), k.as_str()))).collect();
    // Candidates by first word, longest first: a text is looked up only for the words it has.
    let mut by_first: HashMap<String, Vec<&(String, String, String)>> = HashMap::new();
    for w in &list {
        let first: String = w.0.split(|c: char| !is_word(c)).next().unwrap_or("").to_lowercase();
        by_first.entry(first).or_default().push(w);
    }
    for v in by_first.values_mut() {
        v.sort_by_key(|w| std::cmp::Reverse(w.0.len()));
    }

    let made: HashMap<(i64, String), (String, String)> = {
        // A row taken out (source gone) is kept as an empty one for the addons to delete theirs: not "made".
        let mut stmt = con.prepare("SELECT id, hash, base, source FROM full WHERE source <> 'gone'").map_err(db)?;
        stmt.query_map([], |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?)))).map_err(db)?.collect::<rusqlite::Result<_>>().map_err(db)?
    };
    let rows: Vec<(i64, String, String, String)> = {
        let mut stmt = con.prepare("SELECT id, hash, en, it FROM translations ORDER BY id, hash").map_err(db)?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(db)?.collect::<rusqlite::Result<_>>().map_err(db)?
    };
    report.rows = rows.len();
    // The agent's answers: also for rows already made (a fix of its own sent a round later).
    let mut answers: HashMap<(i64, String), String> = HashMap::new();
    if let Some(ByHand::Answers(file)) = &options.by_hand {
        for line in std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?.lines().filter(|l| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).map_err(|e| format!("{file}: {e}: {line}"))?;
            if let (Some(id), Some(hash), Some(answer)) = (v["id"].as_i64(), v["hash"].as_str(), v["answer"].as_str()) {
                answers.insert((id, hash.to_owned()), answer.to_owned());
            }
        }
    }
    let mut names_out: Vec<(i64, String, String, String)> = Vec::new();
    let mut drop: Vec<(i64, String)> = Vec::new();
    let mut todo: Vec<Todo> = Vec::new();
    let mut alive: HashSet<(i64, String)> = HashSet::new();
    for (id, hash, en, it) in rows {
        let key = (id, hash.clone());
        alive.insert(key.clone());
        let base = digest(&it);
        let answered = answers.contains_key(&key);
        if let Some((old_base, _)) = made.get(&key) {
            if *old_base == base {
                if !answered {
                    report.kept += 1;
                    continue;
                }
            } else {
                report.stale += 1;
                drop.push(key.clone());
            }
        }
        let b = bare(&en);
        if let Some((full_it, _)) = by_en.get(b.as_str())
            && !en.contains("[s]")
            && !en.contains("[pl:")
            && en.matches(b.as_str()).count() == 1
            && has_word(&it, &b)
        {
            names_out.push((id, hash, en.replacen(b.as_str(), full_it, 1), base));
            continue;
        }
        let lower = en.to_lowercase();
        let mut swap: Vec<(String, String, String)> = Vec::new();
        let mut firsts: HashSet<&str> = HashSet::new();
        for w in lower.split(|c: char| !is_word(c)).filter(|w| !w.is_empty()) {
            if !firsts.insert(w) {
                continue;
            }
            for cand in by_first.get(w).into_iter().flatten() {
                // The English may write it in lowercase ("grant might"): the hybrid kept it, so it is the term.
                if has_word(&lower, &cand.0.to_lowercase()) && kept(&it, &cand.0) && !swap.iter().any(|s| s.0.contains(cand.0.as_str())) {
                    swap.push((*cand).clone());
                }
            }
        }
        if swap.is_empty() && !options.all && !answered {
            report.same += 1;
            continue;
        }
        todo.push(Todo { id, hash, en, it, swap });
    }
    for key in made.keys() {
        if !alive.contains(key) {
            drop.push(key.clone());
        }
    }
    report.names = names_out.len();
    report.to_rewrite = todo.len();

    let now = now_ms();
    let write = |rows: &[(i64, String, String, String)], source: &str| -> Result<(), String> {
        let tx = con.unchecked_transaction().map_err(db)?;
        for (id, hash, it, base) in rows {
            tx.execute(
                "INSERT OR REPLACE INTO full (id, hash, it, base, source, at, seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, (SELECT coalesce(max(seq), 0) FROM full) + 1)",
                params![id, hash, it, base, source, now],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)
    };
    if !options.dry_run {
        let tx = con.unchecked_transaction().map_err(db)?;
        for (id, hash) in &drop {
            // Emptied, not deleted: the addons that have it take it out on their next sync.
            tx.execute(
                "UPDATE full SET it = '', base = '', source = 'gone', at = ?3, seq = (SELECT coalesce(max(seq), 0) FROM full) + 1
                 WHERE id = ?1 AND hash = ?2",
                params![id, hash, now],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)?;
        write(&names_out, "name")?;
    }

    match &options.by_hand {
        Some(ByHand::AskTo(file)) => {
            use std::io::Write;
            let mut out = std::fs::OpenOptions::new().create(true).append(true).open(file).map_err(|e| format!("{file}: {e}"))?;
            // The game's fr/es/de too with --all: they show which English names are translated.
            let refs = crate::refs::Refs::open(con.path(), options.all);
            for t in todo.iter().take(options.max_texts) {
                let swap: Vec<[&str; 3]> = t.swap.iter().map(|(e, i, k)| [e.as_str(), i.as_str(), k.as_str()]).collect();
                let mut line = json!({"id": t.id, "hash": t.hash, "en": t.en, "it": t.it, "swap": swap});
                if let Some(r) = refs.of(t.id, &t.en, |e| e.replace("<br>", "\n") == t.en.replace("<br>", "\n")) {
                    line["refs"] = r;
                }
                writeln!(out, "{line}").map_err(|e| format!("{file}: {e}"))?;
                report.asked += 1;
            }
        }
        Some(ByHand::Answers(_)) => {
            let mut rows = Vec::new();
            for t in &todo {
                let Some(answer) = answers.get(&(t.id, t.hash.clone())) else { continue };
                if usable(&t.en, &t.it, answer, &t.swap) {
                    rows.push((t.id, t.hash.clone(), answer.clone(), digest(&t.it)));
                } else {
                    report.refused += 1;
                    eprintln!("full: {} refused (markup, plural marks or an English word left): {:?} -> {:?}", t.id, t.it, answer);
                }
            }
            report.written = rows.len();
            if !options.dry_run {
                write(&rows, "hand")?;
            }
        }
        None => {}
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_words_only() {
        assert!(has_word("Conferisci Might agli alleati.", "Might"));
        assert!(!has_word("Mighty Cleave", "Might"));
        assert!(has_word("Tiratore scelto dell'Iron Legion", "Iron Legion"));
        assert!(!has_word("Skritt", "Skr"));
        assert!(has_word("Trahearne's plan", "Trahearne"));
    }

    #[test]
    fn markup_and_plural_marks_must_stay() {
        let swap = vec![("Might".to_owned(), "Forza".to_owned(), "boon".to_owned())];
        let en = "Grant %str1% might to all[y|ies].";
        let hybrid = "Conferisci %str1% Might agli alleati[s].";
        assert!(usable(en, hybrid, "Conferisci %str1% Forza agli alleati[s].", &swap));
        assert!(!usable(en, hybrid, "Conferisci Forza agli alleati[s].", &swap));
        assert!(!usable(en, hybrid, "Conferisci %str1% Might agli alleati[s].", &swap));
        assert!(!usable(en, hybrid, "Conferisci %str1% Forza agli alleati.", &swap));
    }

    #[test]
    fn plural_marks_follow_the_english() {
        let swap = vec![("Skin".to_owned(), "Aspetto".to_owned(), "item".to_owned())];
        let en = "Kaiser Snake Shield Skin[s]";
        // "Skin" is invariable in the hybrid, "Aspetto" is not: the full variant adds its own mark.
        let hybrid = "Skin per Scudo del Serpente Kaiser";
        assert!(usable(en, hybrid, "Aspetto[pl:\"Aspetti\"] per Scudo del Serpente Kaiser", &swap));
        // An invariable Italian word needs none.
        assert!(usable("City[s] of Light", "City of Light", "Città della Luce", &[]));
        // Noun and adjective: two marks for one English mark.
        assert!(usable("Large Bone[s]", "Osso grande", "Osso[pl:\"Ossa\"] grande[pl:\"grandi\"]", &[]));
        // A mark of the hybrid is not dropped.
        assert!(!usable("Large Bone[s]", "Osso[pl:\"Ossa\"] grande", "Osso grande", &[]));
        // No plural in the English: no mark.
        assert!(!usable("Kaiser Snake Shield Skin", "Skin per Scudo", "Aspetto[pl:\"Aspetti\"] per Scudo", &swap));
        // A mark left open.
        assert!(!usable(en, hybrid, "Aspetto[pl:\"Aspetti per Scudo del Serpente Kaiser", &swap));
    }

    #[test]
    fn bare_text_without_markup() {
        assert_eq!(bare("%str1%%str2%Godforged Greatsword%str3%%str4%"), "Godforged Greatsword");
        assert_eq!(bare("<c=@abilitytype>Signet</c>"), "Signet");
        assert_eq!(bare("Large Bone[s]"), "Large Bone");
    }

    #[test]
    fn names_at_once_rewrites_asked_stale_made_again() {
        let dir = std::env::temp_dir().join(format!("gw2it-full-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let names = dir.join("names.toml");
        let terms = dir.join("terms.toml");
        std::fs::write(&names, "[[name]]\nen = \"Bone Minions\"\nkind = \"skill\"\nit = \"Sgherri d'Ossa\"\nstatus = \"proposed\"\n").unwrap();
        std::fs::write(&terms, "[[term]]\nen = \"Might\"\nkind = \"boon\"\nit = \"Forza\"\nstatus = \"approved\"\n").unwrap();
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(include_str!("../../../server/schema.sql")).unwrap();
        con.execute_batch(
            "INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (1, 'a', 'Bone Minions', 'Bone Minions', 'auto', 'x', 0, 1, 1),
               (2, 'b', 'Grant might to allies.', 'Conferisci Might agli alleati.', 'auto', 'x', 0, 2, 1),
               (3, 'c', 'Hello.', 'Ciao.', 'auto', 'x', 0, 3, 1);",
        )
        .unwrap();
        let (n, t) = (names.to_str().unwrap(), terms.to_str().unwrap());
        let ask = dir.join("ask.jsonl");
        let _ = std::fs::remove_file(&ask);
        let opts = |b| Options { max_texts: 10, dry_run: false, all: false, by_hand: Some(b) };
        let r = run(&con, n, t, &opts(ByHand::AskTo(ask.to_str().unwrap().into()))).unwrap();
        assert_eq!((r.names, r.to_rewrite, r.same, r.asked), (1, 1, 1, 1));
        let it: String = con.query_row("SELECT it FROM full WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(it, "Sgherri d'Ossa");
        assert!(std::fs::read_to_string(&ask).unwrap().contains("\"swap\":[[\"Might\",\"Forza\",\"boon\"]]"));
        let answers = dir.join("answers.jsonl");
        std::fs::write(&answers, "{\"id\":2,\"hash\":\"b\",\"answer\":\"Conferisci Forza agli alleati.\"}\n").unwrap();
        let r = run(&con, n, t, &opts(ByHand::Answers(answers.to_str().unwrap().into()))).unwrap();
        assert_eq!((r.written, r.kept), (1, 1));
        // A fix of its own a round later: the row already made is written again.
        std::fs::write(&answers, "{\"id\":2,\"hash\":\"b\",\"answer\":\"Conferisci Forza a tutti gli alleati.\"}\n").unwrap();
        let r = run(&con, n, t, &opts(ByHand::Answers(answers.to_str().unwrap().into()))).unwrap();
        assert_eq!((r.written, r.kept, r.stale), (1, 1, 0));
        let it: String = con.query_row("SELECT it FROM full WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(it, "Conferisci Forza a tutti gli alleati.");
        // The hybrid row changes: the full row is dropped and asked again.
        con.execute("UPDATE translations SET it = 'Conferisci Might a tutti gli alleati.' WHERE id = 2", []).unwrap();
        let r = run(&con, n, t, &Options { max_texts: 10, dry_run: false, all: false, by_hand: None }).unwrap();
        assert_eq!((r.stale, r.to_rewrite), (1, 1));
        // Emptied, not deleted: the addons take it out; it goes to them by /sync after the name row.
        let (it, source): (String, String) = con.query_row("SELECT it, source FROM full WHERE id = 2", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((it.as_str(), source.as_str()), ("", "gone"));
        let (rows, seq, more) = pull(&con, 0, 10).unwrap();
        assert_eq!((rows.len(), more), (2, false));
        assert_eq!(rows.last().unwrap()[2], "");
        assert_eq!(pull(&con, seq, 10).unwrap().0.len(), 0);
        // Taken out once only: the next run does not count it again.
        assert_eq!(run(&con, n, t, &Options { max_texts: 10, dry_run: false, all: false, by_hand: None }).unwrap().stale, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
