//! Glossary of terms that a hand correction fixed: "Seraph" is "Serafini", not "Serafi".
//!
//! A term is learned from a correction (`learn`): the words that changed between the translation
//! that was there and the corrected one, tied to the English word they translate. From then on
//! the translators are told the term (`relevant`, in the prompts of the language models) and what
//! they still get wrong is repaired in their answer (`fix`), in the texts to come and in the ones
//! already translated by a machine.
//!
//! Only proper-name-like words are learned (capitalised, at least four letters) that look like
//! their Italian form: "Seraph" / "Serafini". A term is an English word, not a phrase, and it
//! only acts on texts that contain that word.

use std::collections::HashSet;

/// What a contributor decided for an English word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Term {
    /// The English word, in lowercase.
    pub en: String,
    /// The Italian it is translated as, as the contributor wrote it.
    pub it: String,
    /// What the translators wrote instead: replaced by `it` wherever it is found.
    pub wrong: Vec<String>,
}

/// The first correction of a word, to be added to the glossary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Learned {
    pub en: String,
    pub it: String,
    pub wrong: String,
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Longest Italian form or wrong form, in characters.
const FORM_MAX: usize = 40;
const WRONG_MAX: usize = 20;

/// Letters, single spaces, apostrophes and hyphens, a few words: all that a name needs. The terms
/// go into the prompts of language models and replace text in every translation, so what comes
/// from outside (the global database) is held to this.
fn plain(form: &str) -> bool {
    let words = form.split(' ').collect::<Vec<_>>();
    form.chars().count() <= FORM_MAX
        && words.len() <= SPAN_MAX
        && words.iter().all(|w| !w.is_empty() && w.chars().all(|c| c.is_alphabetic() || matches!(c, '\'' | '’' | '-')))
}

impl Term {
    /// True if the term is safe to keep and to use: an English word of letters, plain forms.
    pub fn is_valid(&self) -> bool {
        (SHARED_MIN..=FORM_MAX).contains(&self.en.chars().count())
            && self.en.chars().all(char::is_alphabetic)
            && self.en == self.en.to_lowercase()
            && plain(&self.it)
            && self.wrong.len() <= WRONG_MAX
            && self.wrong.iter().all(|w| plain(w))
    }
}

/// `term` after a contributor of `trust` said `incoming` about the same English word, given what is
/// there already (the term and the trust of whoever wrote it). None if nothing changes. The addon
/// and the server do the same with it, so that they agree.
///
/// - A weaker voice than the one there is ignored. The same or a stronger one decides the Italian
///   (the last one wins among equals): the one it replaces becomes a wrong form, so that it is
///   still repaired.
/// - Wrong forms only add up; the Italian is never one of them.
pub fn merge(current: Option<(&Term, i64)>, incoming: &Term, trust: i64) -> Option<Term> {
    let mut term = match current {
        Some((term, current_trust)) if trust >= current_trust => term.clone(),
        Some(_) => return None,
        None => Term { en: incoming.en.clone(), it: incoming.it.clone(), wrong: Vec::new() },
    };
    if !same(&term.it, &incoming.it) {
        let old = std::mem::replace(&mut term.it, incoming.it.clone());
        term.wrong.push(old);
    }
    // The contributor's spelling, capitals included.
    term.it.clone_from(&incoming.it);
    term.wrong.extend(incoming.wrong.iter().cloned());
    let mut kept: Vec<String> = Vec::new();
    for wrong in std::mem::take(&mut term.wrong) {
        if !same(&wrong, &term.it) && !kept.iter().any(|k| same(k, &wrong)) {
            kept.push(wrong);
        }
    }
    term.wrong = kept;
    match current {
        Some((current, _)) if *current == term => None,
        _ => Some(term),
    }
}

/// A word of a text: its letters, and where they are.
pub(crate) struct Word<'a> {
    pub(crate) text: &'a str,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Runs of letters; everything else (digits, punctuation, markup syntax) separates them.
pub(crate) fn words(s: &str) -> Vec<Word<'_>> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_alphabetic(), start) {
            (true, None) => start = Some(i),
            (false, Some(from)) => {
                out.push(Word { text: &s[from..i], start: from, end: i });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        out.push(Word { text: &s[from..], start: from, end: s.len() });
    }
    out
}

pub(crate) fn lower(w: &Word) -> String {
    w.text.to_lowercase()
}

pub(crate) fn common_prefix(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count()
}

/// The longest run of changed words that is still a correction of a term, not a rewrite.
const SPAN_MAX: usize = 3;
/// Letters an English word and its Italian form share at the start.
pub(crate) const SHARED_MIN: usize = 4;

/// Articles and prepositions that join a name to the rest of the sentence.
pub(crate) const FUNCTION_WORDS: &[&str] = &[
    "il", "lo", "la", "i", "gli", "le", "un", "uno", "una", "di", "del", "dello", "della", "dei", "degli", "delle",
    "a", "al", "allo", "alla", "ai", "agli", "alle", "da", "dal", "dallo", "dalla", "dai", "dagli", "dalle", "in",
    "nel", "nello", "nella", "nei", "negli", "nelle", "su", "sul", "sullo", "sulla", "sui", "sugli", "sulle", "e",
    "ed", "o", "per", "con",
];

fn trim_function_words<'a, 'b>(words: &'b [Word<'a>]) -> &'b [Word<'a>] {
    let is_function = |w: &Word| FUNCTION_WORDS.contains(&lower(w).as_str());
    let start = words.iter().take_while(|w| is_function(w)).count();
    let end = words.len() - words[start..].iter().rev().take_while(|w| is_function(w)).count();
    &words[start..end]
}

/// The term a correction fixed: `old` was the Italian text, `new` the contributor's, `en` the
/// English. None if the correction is not about one word that can be tied to the English (a
/// rewrite, a word that is not a name, a translation that does not look like the English).
pub fn learn(en: &str, old: &str, new: &str) -> Option<Learned> {
    let (o, n) = (words(old), words(new));
    let same = |a: &Word, b: &Word| lower(a) == lower(b);
    let prefix = o.iter().zip(&n).take_while(|(a, b)| same(a, b)).count();
    let suffix = o[prefix..].iter().rev().zip(n[prefix..].iter().rev()).take_while(|(a, b)| same(a, b)).count();
    let (o, n) = (&o[prefix..o.len() - suffix], &n[prefix..n.len() - suffix]);
    // "Soldato Serafino" -> "Soldato dei Serafini": the term is Serafini; the "dei" is the
    // sentence's (and would be doubled by `fix`).
    let (o, n) = (trim_function_words(o), trim_function_words(n));
    if o.is_empty() || n.is_empty() || o.len() > SPAN_MAX || n.len() > SPAN_MAX {
        return None;
    }
    let span = |s: &str, w: &[Word]| s[w[0].start..w[w.len() - 1].end].to_owned();
    let (wrong, it) = (span(old, o), span(new, n));

    let (_, english) = words(en)
        .into_iter()
        .filter(|w| w.text.chars().next().is_some_and(char::is_uppercase) && w.text.chars().count() >= SHARED_MIN)
        .filter_map(|w| {
            let shared = n.iter().map(|m| common_prefix(w.text, m.text)).max()?;
            (shared >= SHARED_MIN).then_some((shared, w))
        })
        .max_by_key(|(shared, _)| *shared)?;
    let learned = Learned { en: english.text.to_lowercase(), it, wrong };
    // What the global database would refuse is not learned.
    Term { en: learned.en.clone(), it: learned.it.clone(), wrong: vec![learned.wrong.clone()] }.is_valid().then_some(learned)
}

/// The lowercase words of English texts, and the singular of the ones in -s ("Seraphs").
fn english(texts: &[&str]) -> HashSet<String> {
    let mut present = HashSet::new();
    for word in texts.iter().flat_map(|t| words(t)) {
        let word = lower(&word);
        if let Some(singular) = word.strip_suffix('s') {
            present.insert(singular.to_owned());
        }
        present.insert(word);
    }
    present
}

/// The terms of the glossary that appear in `texts` (English).
pub fn relevant<'a>(terms: &'a [Term], texts: &[&str]) -> Vec<&'a Term> {
    let present = english(texts);
    terms.iter().filter(|t| present.contains(&t.en)).collect()
}

/// `it` with the wrong forms of the terms in `en` replaced. Idempotent.
pub fn fix(en: &str, it: &str, terms: &[Term]) -> String {
    let in_en = english(&[en]);
    let mut out = it.to_owned();
    for term in terms.iter().filter(|t| in_en.contains(&t.en)) {
        for wrong in &term.wrong {
            out = replace_words(&out, wrong, &term.it);
        }
    }
    out
}

/// Replaces every run of words equal (ignoring case) to `wrong` with `right`, keeping the case
/// of the first letter of what was there.
fn replace_words(text: &str, wrong: &str, right: &str) -> String {
    let target: Vec<String> = words(wrong).iter().map(lower).collect();
    let found = words(text);
    if target.is_empty() || target.len() > found.len() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let (mut copied, mut i) = (0, 0);
    while i + target.len() <= found.len() {
        if target.iter().zip(&found[i..]).all(|(t, w)| *t == lower(w)) {
            let last = &found[i + target.len() - 1];
            out.push_str(&text[copied..found[i].start]);
            out.push_str(&match_case(right, found[i].text));
            (copied, i) = (last.end, i + target.len());
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// `word` starting with a capital or not, as `like` does.
fn match_case(word: &str, like: &str) -> String {
    let mut chars = word.chars();
    let (Some(first), Some(like)) = (chars.next(), like.chars().next()) else {
        return word.to_owned();
    };
    let first: String = if like.is_uppercase() { first.to_uppercase().collect() } else { first.to_lowercase().collect() };
    first + chars.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(en: &str, it: &str, wrong: &[&str]) -> Term {
        Term { en: en.into(), it: it.into(), wrong: wrong.iter().map(|w| (*w).into()).collect() }
    }

    #[test]
    fn learns_the_word_that_changed() {
        let learned = learn("Seraph Guard", "Guardia dei Serafi", "Guardia dei Serafini").unwrap();
        assert_eq!(learned, Learned { en: "seraph".into(), it: "Serafini".into(), wrong: "Serafi".into() });
    }

    #[test]
    fn the_sentences_own_words_are_not_the_term() {
        // The same name, written three ways: the word is learned, not the "dei".
        let learned = learn("Seraph soldier", "Soldato Serafino", "Soldato dei Serafini").unwrap();
        assert_eq!(learned, Learned { en: "seraph".into(), it: "Serafini".into(), wrong: "Serafino".into() });
        let terms = [term("seraph", "Serafini", &["Serafi", "Serafino"])];
        for text in ["Soldato dei Serafini", "Soldato Serafino", "Guardia dei Serafi"] {
            let once = fix("Seraph soldier", text, &terms);
            assert!(once.ends_with("Serafini") && !once.contains("dei dei"), "{once}");
            assert_eq!(fix("Seraph soldier", &once, &terms), once);
        }
    }

    #[test]
    fn does_not_learn_what_is_not_a_term() {
        // A rewrite, an addition, a deletion, no English word it looks like, a lowercase one.
        assert_eq!(learn("Seraph Guard", "Guardia dei Serafi", "La guardia scelta dalla Fiamma"), None);
        assert_eq!(learn("Seraph Guard", "Guardia dei Serafi", "Guardia dei Serafi veri"), None);
        assert_eq!(learn("Seraph Guard", "Guardia dei Serafi", "Guardia dei"), None);
        assert_eq!(learn("Seraph Guard", "Guardia dei Serafi", "Guardia dei Angeli"), None);
        assert_eq!(learn("seraph guard", "guardia dei serafi", "guardia dei serafini"), None);
        // Nothing changed but the case.
        assert_eq!(learn("Seraph Guard", "Guardia dei serafini", "Guardia dei Serafini"), None);
    }

    #[test]
    fn fixes_the_wrong_form_only_where_the_term_is() {
        let terms = [term("seraph", "Serafini", &["Serafi"])];
        assert_eq!(fix("Seraph Vanguard", "Avanguardia dei Serafi", &terms), "Avanguardia dei Serafini");
        // Case follows the text; whole words only; the English must have the term.
        assert_eq!(fix("Seraphs", "i serafi", &terms), "i serafini");
        assert_eq!(fix("Seraphim", "i serafi", &terms), "i serafi");
        assert_eq!(fix("The Seraph", "i serafi, i Serafi e i Serafini", &terms), "i serafini, i Serafini e i Serafini");
        assert_eq!(fix("Axe", "Serafi", &terms), "Serafi");
        // Idempotent.
        let once = fix("Seraph Guard", "Guardia dei Serafi", &terms);
        assert_eq!(fix("Seraph Guard", &once, &terms), once);
    }

    #[test]
    fn markup_is_left_alone() {
        let terms = [term("seraph", "Serafini", &["Serafi"])];
        assert_eq!(fix("Seraph %str1%", "Serafi <c=@flavor>%str1%</c>", &terms), "Serafini <c=@flavor>%str1%</c>");
    }

    #[test]
    fn merging_what_contributors_say() {
        let said = |it: &str, wrong: &str| term("seraph", it, &[wrong]);
        let first = merge(None, &said("Serafini", "Serafi"), 2).unwrap();
        assert_eq!(first, term("seraph", "Serafini", &["Serafi"]));
        // Nothing new: no change. A new wrong form is added.
        assert_eq!(merge(Some((&first, 2)), &said("Serafini", "serafi"), 2), None);
        let more = merge(Some((&first, 2)), &said("Serafini", "Serafino"), 2).unwrap();
        assert_eq!(more.wrong, ["Serafi", "Serafino"]);
        // The last of equals decides; the one it replaces is now wrong; the Italian is never wrong.
        let changed = merge(Some((&more, 2)), &said("Serafim", "Serafini"), 2).unwrap();
        assert_eq!(changed, term("seraph", "Serafim", &["Serafi", "Serafino", "Serafini"]));
        let back = merge(Some((&changed, 2)), &said("Serafini", "Serafim"), 2).unwrap();
        assert_eq!(back, term("seraph", "Serafini", &["Serafi", "Serafino", "Serafim"]));
        // A weaker voice is ignored, a stronger one replaces.
        assert_eq!(merge(Some((&more, 3)), &said("Angeli", "Serafini"), 2), None);
        assert_eq!(merge(Some((&more, 2)), &said("Angeli", "Serafini"), 3).unwrap().it, "Angeli");
    }

    #[test]
    fn the_terms_a_text_has() {
        let terms = [term("seraph", "Serafini", &["Serafi"]), term("vigil", "Vigilia", &["Veglia"])];
        let found = relevant(&terms, &["Join the Seraph.", "Nothing"]);
        assert_eq!(found, [&terms[0]]);
    }
}
