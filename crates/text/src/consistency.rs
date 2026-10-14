//! Finds the English words that the translations render in different ways: "Seraph guard" is
//! "Guardia dei Serafini" and "Seraph soldier" is "Soldato Serafino". The server's consistency job
//! (crates/server/src/consistency.rs) asks a language model to settle each group found here.
//!
//! A group is an English word (a capitalised one, as names are) in at least `MIN_TEXTS` texts whose
//! Italian renders it with more than one form: the Italian word that looks like it ("Serafino",
//! "Serafini"), with the preposition before it if there is one ("dei Serafini"), or no such word.

use std::collections::{BTreeMap, BTreeSet};

use crate::glossary::{FUNCTION_WORDS, SHARED_MIN, common_prefix, lower, words};

/// Texts a word must be in to be worth settling.
pub const MIN_TEXTS: usize = 3;
/// What a text with no Italian word like the English one renders it as.
const OTHER: &str = "-";

/// A translation, as the job reads it from the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: i64,
    pub hash: String,
    pub en: String,
    pub it: String,
    /// The trust of the row (schema.sql): up to 1 are machine translations, which the job may
    /// rewrite; from 2 up, hand corrections, which it only reads.
    pub trust: i64,
}

/// An English word and the texts it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Lowercase, in the singular ("seraph").
    pub word: String,
    /// Indexes into the entries given to `groups`.
    pub entries: Vec<usize>,
    /// How the texts render it, sorted: ["dei serafini", "serafino"].
    pub variants: Vec<String>,
}

impl Group {
    /// Changes when the group does: the job does not look again at one it has settled.
    pub fn signature(&self) -> String {
        format!("{}|{}", self.entries.len(), self.variants.join(","))
    }
}

/// The word as the key of a group: lowercase, without the plural -s ("Seraphs" -> "seraph").
fn key(word: &str) -> String {
    let word = word.to_lowercase();
    match word.strip_suffix('s') {
        Some(singular) if singular.chars().count() >= SHARED_MIN && !singular.ends_with('s') => singular.to_owned(),
        _ => word,
    }
}

/// How `it` renders the English `word`: its Italian word that looks like it, and the preposition
/// or article before that word.
fn rendering(word: &str, it: &str) -> String {
    let found = words(it);
    let best = found
        .iter()
        .enumerate()
        .map(|(i, w)| (common_prefix(word, w.text), i))
        .filter(|(shared, _)| *shared >= SHARED_MIN)
        .max_by_key(|(shared, i)| (*shared, std::cmp::Reverse(*i)));
    let Some((_, i)) = best else {
        return OTHER.into();
    };
    let before = i.checked_sub(1).map(|b| lower(&found[b])).filter(|w| FUNCTION_WORDS.contains(&w.as_str()));
    match before {
        Some(function) => format!("{function} {}", lower(&found[i])),
        None => lower(&found[i]),
    }
}

/// The English words in more than `MIN_TEXTS - 1` texts that are rendered in more than one way,
/// the biggest groups first.
pub fn groups(entries: &[Entry]) -> Vec<Group> {
    let mut by_word: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, entry) in entries.iter().enumerate() {
        let names: BTreeSet<String> = words(&entry.en)
            .iter()
            .filter(|w| w.text.chars().next().is_some_and(char::is_uppercase) && w.text.chars().count() >= SHARED_MIN)
            .map(|w| key(w.text))
            .collect();
        for name in names {
            by_word.entry(name).or_default().push(i);
        }
    }
    let mut found: Vec<Group> = by_word
        .into_iter()
        .filter(|(_, texts)| texts.len() >= MIN_TEXTS)
        .filter_map(|(word, texts)| {
            let variants: BTreeSet<String> = texts.iter().map(|&i| rendering(&word, &entries[i].it)).collect();
            (variants.len() > 1).then(|| Group { word, entries: texts, variants: variants.into_iter().collect() })
        })
        .collect();
    found.sort_by(|a, b| b.entries.len().cmp(&a.entries.len()).then_with(|| a.word.cmp(&b.word)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i64, en: &str, it: &str) -> Entry {
        Entry { id, hash: format!("{id:016x}"), en: en.into(), it: it.into(), trust: 1 }
    }

    #[test]
    fn renderings() {
        assert_eq!(rendering("seraph", "Guardia dei Serafini"), "dei serafini");
        assert_eq!(rendering("seraph", "Soldato Serafino"), "serafino");
        assert_eq!(rendering("seraph", "Soldato Serafini"), "serafini");
        assert_eq!(rendering("seraph", "Soldato Angelico"), "-");
    }

    #[test]
    fn finds_the_word_rendered_in_different_ways() {
        let entries = [
            entry(1, "Seraph Guard", "Guardia dei Serafini"),
            entry(2, "Seraph Soldier", "Soldato Serafino"),
            entry(3, "Seraph Soldier's Sword", "Spada del Soldato Serafino"),
            entry(4, "Seraphs", "Serafini"),
            // Always the same way: not a group. Too few texts: not a group.
            entry(5, "Vigil Guard", "Guardia della Vigilia"),
            entry(6, "Vigil Soldier", "Soldato della Vigilia"),
            entry(7, "Vigil Mage", "Mago della Vigilia"),
            entry(8, "Pact Mage", "Mago Patto"),
            entry(9, "Pact Soldier", "Soldato Alleanza"),
        ];
        let found = groups(&entries);
        // "Soldier" is a candidate too ("del Soldato" in the possessive): the model dismisses it.
        let words: Vec<&str> = found.iter().map(|g| g.word.as_str()).collect();
        assert_eq!(words, ["seraph", "soldier"], "{found:?}");
        assert_eq!(found[0].entries, [0, 1, 2, 3]);
        assert_eq!(found[0].variants, ["dei serafini", "serafini", "serafino"]);
        assert_eq!(found[0].signature(), "4|dei serafini,serafini,serafino");
    }
}
