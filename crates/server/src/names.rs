//! Names that stay in English (gw2it_text::names), from the tables `english_names` and `affixes`:
//! the addons get them with /sync, bulk.rs gives them to the agents ("keep"), lt_sync.rs checks its
//! rewrites with `swaps_only`.
//!
//! The job that filled the tables from the official API and put the names back into the machine
//! translations (`gw2it-server names`) was removed on 2026-11-20 with the rest of the old
//! machine-translation pipeline; its prompt is in docs/old-pipeline-prompts.md.

use gw2it_text::names::{self, Names};
use rusqlite::Connection;
use serde_json::Value;

pub const SCHEMA: &str = "
-- The names that stay in English, from the official API.
CREATE TABLE IF NOT EXISTS english_names (
    name  TEXT    PRIMARY KEY,
    kind  TEXT    NOT NULL,       -- profession, specialization, skill, trait, item, upgrade, boon, condition
    added INTEGER NOT NULL
);
-- The names taken out of english_names (by anyone: a trigger), so that the addons take them out
-- too; a name put back leaves this table.
CREATE TABLE IF NOT EXISTS english_names_removed (
    name    TEXT    PRIMARY KEY,
    removed INTEGER NOT NULL      -- unix ms
);
CREATE TRIGGER IF NOT EXISTS english_names_deleted AFTER DELETE ON english_names BEGIN
    INSERT OR REPLACE INTO english_names_removed (name, removed)
    VALUES (old.name, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER));
END;
CREATE TRIGGER IF NOT EXISTS english_names_readded AFTER INSERT ON english_names BEGIN
    DELETE FROM english_names_removed WHERE name = new.name;
END;
-- The stat prefixes and the items named after one, with the prefix (gw2it_text::names::Names::affix).
CREATE TABLE IF NOT EXISTS affixes (
    text  TEXT    PRIMARY KEY,
    affix TEXT    NOT NULL,
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

/// The names, without the plain English words (gw2it_text::names), and the stat prefixes.
pub fn load(con: &Connection) -> rusqlite::Result<Names> {
    let mut stmt = con.prepare_cached("SELECT name FROM english_names")?;
    let list: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    let mut stmt = con.prepare_cached("SELECT text, affix FROM affixes")?;
    let affixes: Vec<(String, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut stmt = con.prepare_cached("SELECT name FROM english_names WHERE kind IN ('imported', 'item', 'profession', 'specialization')")?;
    let proper: Vec<String> = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(Names::new(list).with_affixes(affixes).with_proper(proper))
}

/// The names added after `since` (unix ms), and the time of the newest (`since` if none), for /sync.
pub fn pull(con: &Connection, since: i64) -> rusqlite::Result<(Vec<String>, i64)> {
    changed(con, "SELECT name, added FROM english_names WHERE added > ?1", since)
}

/// The names taken out after `since` (unix ms), and the time of the newest (`since` if none), for
/// /sync: on their own mark, so that an addon that does not know them yet gets them all once it does.
pub fn removed(con: &Connection, since: i64) -> rusqlite::Result<(Vec<String>, i64)> {
    changed(con, "SELECT name, removed FROM english_names_removed WHERE removed > ?1", since)
}

fn changed(con: &Connection, sql: &str, since: i64) -> rusqlite::Result<(Vec<String>, i64)> {
    let mut stmt = con.prepare_cached(sql)?;
    let mut newest = since;
    let mut list = Vec::new();
    for row in stmt.query_map([since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (name, at) = row?;
        newest = newest.max(at);
        list.push(name);
    }
    Ok((list, newest))
}

/// The language model: system prompt and user message in, JSON out; an Err stops the run.
pub type Ask<'a> = dyn Fn(&str, &str) -> Result<Value, String> + 'a;

/// The words of `s` in lowercase, split at apostrophes and hyphens too ("dell'Holo-Dancer": "dell",
/// "holo", "dancer"), outside `<tags>`, `%tokens%` and `[marks]`.
fn plain_words(s: &str) -> Vec<String> {
    let mut bare = String::with_capacity(s.len());
    let mut until: Option<char> = None;
    for c in s.chars() {
        match (until, c) {
            (Some(end), _) if c == end => until = None,
            (Some(_), _) => {}
            (None, '<') => until = Some('>'),
            (None, '[') => until = Some(']'),
            (None, '%') => until = Some('%'),
            (None, _) => bare.push(c),
        }
    }
    bare.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// The articles and prepositions a rewrite may change around a name ("nell'Arcipelago di Skywatch"
/// becomes "nello Skywatch Archipelago").
const AROUND: &[&str] = &[
    "il", "lo", "la", "i", "gli", "le", "l", "un", "uno", "una", "di", "del", "dello", "della", "dei", "degli", "delle",
    "dell", "a", "al", "allo", "alla", "ai", "agli", "alle", "all", "da", "dal", "dallo", "dalla", "dai", "dagli",
    "dalle", "dall", "in", "nel", "nello", "nella", "nei", "negli", "nelle", "nell", "con", "su", "sul", "sullo",
    "sulla", "sui", "sugli", "sulle", "sull", "per", "tra", "fra", "e", "ed", "d",
];

/// The Italian verbs that come with a mechanic put back ("Tormenta i nemici" becomes "Infliggi
/// Torment ai nemici"), and the ones that stay Italian (cripple azzoppare, chill assiderare).
const VERBS: &[&str] = &[
    "conferisci", "conferisce", "conferiscono", "conferire", "conferendo", "conferito", "conferita", "infliggi", "infligge",
    "infliggono", "infliggere", "infliggendo", "inflitto", "inflitta", "applica", "applicano", "applicare", "applicando",
    "rimuovi", "rimuove", "rimuovono", "rimuovere", "rimuovendo", "ottieni", "ottiene", "ottengono", "ottenere",
    "ottenendo", "azzoppa", "azzoppare", "assidera", "assiderare", "acceca", "accecare",
];

/// The stems of the verbs of conditions and disables that stay Italian (cripple azzoppare, daze
/// disorientare, slow rallentare): a rewrite never swaps them for the term.
const KEPT_VERBS: &[&str] = &["azzopp", "assider", "accec", "avvelen", "stord", "disorient", "rallent", "immobilizz", "indebol"];

/// Whether the rewrite `new` of `old` only swaps Italian for the English of `en`: no Italian word
/// `old` did not have but articles and prepositions (", conferendo Might." added at the end), and
/// something of `old` taken out for what came in (a name put next to its translation, not in its
/// place: "Provocazione dell'Holo-Dancer Taunt"), and no verb swapped for a term ("indebolendo e
/// bruciando" become "indebolendo e Burning": the verb stays, "infliggendo Burning" would do).
pub(crate) fn swaps_only(en: &str, old: &str, new: &str, keep: &[&str]) -> bool {
    let (en, old_words, new_words) = (plain_words(en), plain_words(old), plain_words(new));
    let gerunds = |ws: &[String]| {
        ws.iter().filter(|w| w.chars().count() > 5 && (w.ends_with("ando") || w.ends_with("endo")) && !["quando", "comando"].contains(&w.as_str())).count()
    };
    if gerunds(&new_words) < gerunds(&old_words) {
        return false;
    }
    // "disorienta i nemici" become "Daze ai nemici", "rallentare" become "Slow": these verbs stay.
    // Not the nouns ("stordimento" may become "Stun" in a list of disables).
    let noun = |w: &str| ["mento", "menti", "zione", "zioni"].iter().any(|e| w.ends_with(e));
    let kept = |ws: &[String]| ws.iter().filter(|w| !noun(w) && KEPT_VERBS.iter().any(|v| w.starts_with(v))).count();
    if kept(&new_words) < kept(&old_words) {
        return false;
    }
    // The names and terms in any of their forms ("conditions" in the English, "condition" here).
    let mut allowed: Vec<String> = keep.iter().flat_map(|k| plain_words(k)).collect();
    for (term, forms) in names::TERMS {
        allowed.extend(std::iter::once(term).chain(forms.iter()).flat_map(|f| plain_words(f)));
    }
    for attribute in names::TERM_ATTRIBUTES {
        allowed.extend(plain_words(attribute));
    }
    let invented = new_words.iter().any(|w| {
        !old_words.contains(w)
            && !en.contains(w)
            && !allowed.contains(w)
            && !AROUND.contains(&w.as_str())
            && !VERBS.contains(&w.as_str())
            && w.chars().any(char::is_alphabetic)
    });
    let mut left = new_words.clone();
    let removed = old_words.iter().any(|w| match left.iter().position(|n| n == w) {
        Some(at) => {
            left.swap_remove(at);
            false
        }
        None => true,
    });
    !invented && removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let con = Connection::open_in_memory().unwrap();
        con.execute_batch(include_str!("../schema.sql")).unwrap();
        con.execute_batch(SCHEMA).unwrap();
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

    #[test]
    fn a_rewrite_only_swaps_italian_for_english() {
        // Rewrites of the night of 2026-10-24: the name in place of its translation.
        assert!(swaps_only(
            "Raccogli all of Zojja's journal pages in Skywatch Archipelago.",
            "Raccogli tutte le pagine del diario di Zojja nell'Arcipelago di Skywatch.",
            "Raccogli tutte le pagine del diario di Zojja nello Skywatch Archipelago.",
            &["Skywatch Archipelago"]
        ));
        assert!(swaps_only("Harrowshot—Fear", "Harrowshot — Paura", "Harrowshot — Fear", &["Fear"]));
        assert!(swaps_only("Inflict torment.", "Tormenta i nemici.", "Infliggi Torment ai nemici.", &["Torment"]));
        assert!(swaps_only("Removes conditions.", "Rimuove le condizioni.", "Rimuove le condition.", &["condition"]));
        assert!(swaps_only(
            "The Wizard's Tower is unlike anywhere.",
            "La Torre del Mago è diversa.",
            "La Wizard's Tower è diversa.",
            &["The Wizard's Tower"]
        ));
        assert!(swaps_only("+%num1% Power", "+%num1% Potenza", "+%num1% Power", &["Power"]));
        // Words the Italian did not say, and a name next to its translation.
        assert!(!swaps_only(
            "Send them to whatever god might still accept them.",
            "Mandali da qualunque dio possa ancora accoglierli.",
            "Mandali da qualunque dio possa ancora accoglierli, conferendo Might.",
            &["Might"]
        ));
        assert!(!swaps_only("Power Skritt Suit", "Tuta da combattimento Skritt", "Tuta da combattimento Power Skritt", &["Power"]));
        assert!(!swaps_only("The sweat burning your eyes.", "Il sudore che ti brucia gli occhi.", "Il sudore che ti causa Burning gli occhi.", &["Burning"]));
        assert!(!swaps_only("Holo-Dancer Taunt", "Provocazione dell'Holo-Dancer", "Provocazione dell'Holo-Dancer Taunt", &["Taunt"]));
        // 2026-11-14: the verb swapped for the condition; with its verb it is right.
        let en = "Lunge forward, weakening and burning any foes left in your wake.";
        assert!(!swaps_only(en, "Scatta, indebolendo e bruciando tutti i nemici.", "Scatta, indebolendo e Burning tutti i nemici.", &["Burning"]));
        assert!(swaps_only(en, "Scatta, indebolendo e bruciando tutti i nemici.", "Scatta, indebolendo e infliggendo Burning a tutti i nemici.", &["Burning"]));
        // 2026-11-17: the verbs of daze and slow stay; a noun in a list of disables may go.
        let en = "Damage and Daze enemies with a powerful screech.";
        assert!(!swaps_only(en, "Infliggi danni e disorienta i nemici con un potente stridio.", "Infliggi danni e Daze ai nemici con un potente stridio.", &["Daze"]));
        let en = "<c=@abilitytype>Survival.</c> Use mud to cripple, slow, and immobilize foes.";
        let old = "<c=@abilitytype>Survival.</c> Usa il fango per azzoppare, rallentare e immobilizzare i nemici.";
        assert!(!swaps_only(en, old, "<c=@abilitytype>Survival.</c> Usa il fango per azzoppare, Slow e immobilizzare i nemici.", &["Slow"]));
        assert!(swaps_only("Stuns • Causes Vulnerability", "Stordimento • Provoca vulnerabilità", "Stun • Provoca Vulnerability", &["Stun", "Vulnerability"]));
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
    fn the_addons_get_the_names_taken_out_since_their_last_sync() {
        let con = db();
        con.execute("DELETE FROM english_names WHERE name = 'The Dreamer'", []).unwrap();
        let (list, newest) = removed(&con, 0).unwrap();
        assert_eq!(list, ["The Dreamer"]);
        assert!(newest > 5);
        assert_eq!(removed(&con, newest).unwrap(), (vec![], newest));
        // Taking it out does not move the mark of the names added; a name put back is no longer out.
        assert_eq!(pull(&con, 1).unwrap().1, 1);
        con.execute("INSERT INTO english_names VALUES ('The Dreamer', 'item', ?1)", [newest + 1]).unwrap();
        assert_eq!(removed(&con, 0).unwrap().0.len(), 0);
    }
}
