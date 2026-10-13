//! Italian plurals for item names. English texts mark the plural with `[s]` or `[pl:"..."]`
//! and the game picks the singular or the plural by the stack size; markup.rs drops those marks
//! before translating, so the translation is the singular and this puts the plural back, in the
//! game's own syntax: `Medaglia precisa in juta` -> `Medaglia[pl:"Medaglie"] precisa[pl:"precise"] in juta`.
//! `[pl:"..."]` replaces the word it follows.
//!
//! Only the first words are changed (the noun and its adjectives), up to the first preposition,
//! colon, comma or parenthesis. Words that are also in the English text (proper names, terms left
//! in English) are kept. When a plural is not known the word is left alone: the singular is a
//! smaller mistake than a wrong plural.
//!
//! Texts with `%num%` are left alone: there the plural depends on a number inside the sentence and
//! the word order changes.

/// The part to pluralize ends at one of these.
const STOP: &[&str] = &[
    "di", "del", "dello", "della", "dei", "degli", "delle", "da", "dal", "dallo", "dalla", "dai",
    "dagli", "dalle", "a", "al", "allo", "alla", "ai", "agli", "alle", "in", "nel", "nello",
    "nella", "nei", "negli", "nelle", "su", "sul", "sullo", "sulla", "sui", "sugli", "sulle",
    "per", "con", "tra", "fra", "e", "ed", "o", "contro", "senza", "sotto", "sopra", "verso",
    "come", "che", "dopo", "prima", "presso",
];
const ARTICLES: &[(&str, &str)] = &[("il", "i"), ("lo", "gli"), ("la", "le")];
/// "un Elmo" has no plural a single mark can give.
const NO_PLURAL: &[&str] = &["un", "uno", "una"];
const IRREGULAR: &[(&str, &str)] = &[
    ("uomo", "uomini"), ("uovo", "uova"), ("dio", "dei"), ("bue", "buoi"), ("ala", "ali"),
    ("arma", "armi"), ("mano", "mani"), ("paio", "paia"), ("eco", "echi"), ("amico", "amici"),
    ("nemico", "nemici"), ("greco", "greci"), ("porco", "porci"), ("antico", "antichi"),
    ("fico", "fichi"), ("mago", "maghi"), ("re", "re"), ("tè", "tè"), ("gnu", "gnu"),
];
/// Masculine nouns in -a: plural in -i.
const MASC_A: &[&str] = &[
    "problema", "sistema", "tema", "schema", "programma", "diagramma", "emblema", "dilemma",
    "enigma", "stemma", "poema", "dramma", "clima", "pianeta", "poeta", "profeta", "pilota",
    "atleta", "teorema", "fantasma", "panorama", "aroma", "telegramma", "prisma", "magma",
    "sintoma", "idioma", "carisma", "cinema",
];
const INVARIABLE: &[&str] = &[
    "lilla", "blu", "beige", "ocra", "mini", "maxi", "super", "extra",
    "standard", "pari", "dispari", "serie", "specie", "radio", "foto", "moto", "auto", "bici",
    "euro", "vaglia", "boa", "gorilla", "panda", "koala", "yak", "alpaca",
    // colours that do not change ("Tinte borgogna")
    "borgogna", "salmone", "indaco", "lavanda",
    "malva", "avorio", "porpora", "fucsia", "cobalto", "magenta", "ciano", "ruggine",
];
/// Nouns that are also colours: invariable as a colour after the noun ("Tinta rosa"), regular
/// as the noun itself ("Rosa" -> "Rose").
const COLOUR_NOUNS: &[&str] = &["rosa", "viola", "crema", "oliva", "ambra", "prugna", "lavanda", "corallo", "senape"];
/// After the noun these are appositions, not adjectives: they stay as they are ("Tinture Vino").
const APPOSITION: &[&str] = &[
    "smeraldo", "rubino", "zaffiro", "oro", "argento", "bronzo", "rame", "ferro", "acciaio",
    "base", "ombra", "vino", "notte", "fuoco", "ghiaccio", "sole", "luna", "cielo", "mare",
    "tempesta", "drago", "fantasma", "spirito", "mini", "extra", "standard", "premium",
];

/// `it` (the singular translation of `en`) with the plural marks the game needs. Unchanged if `en`
/// has no plural mark, has a `%num%`, or `it` already has marks.
pub fn pluralize(en: &str, it: &str) -> String {
    if !(en.contains("[s]") || en.contains("[pl:")) || en.contains("%num") || it.contains("[pl:") {
        return it.to_owned();
    }
    let english: Vec<String> = words_of(en).map(str::to_lowercase).collect();

    let toks = split_keeping_spaces(it.trim());
    let mut out: Vec<String> = toks.iter().map(|t| (*t).to_owned()).collect();
    let mut changed = false;
    let mut head = true;
    let mut first = true;
    for (i, tok) in toks.iter().enumerate() {
        if tok.chars().all(char::is_whitespace) {
            continue;
        }
        let n = if std::mem::take(&mut first) { 0 } else { 1 };
        if !head {
            break;
        }
        let (lead, core, trail) = edges(tok);
        let punct_lead = !lead.is_empty();
        let (tag_l, core, tag_r) = peel_tags(core);
        let (lead, trail) = (format!("{lead}{tag_l}"), format!("{tag_r}{trail}"));
        let (lead, trail) = (lead.as_str(), trail.as_str());
        let low = core.to_lowercase();
        if STOP.contains(&low.as_str()) || punct_lead {
            break;
        }
        if n == 0 && (NO_PLURAL.contains(&low.as_str()) || core.contains(['\'', '’'])) {
            break;
        }
        if let Some((_, pl)) = ARTICLES.iter().find(|(s, _)| *s == low) {
            out[i] = mark(lead, core, &case_like(core, pl), trail);
            changed = true;
        } else if is_word(core) && !english.contains(&low) {
            let pl = if low == "lama" && english.iter().any(|w| w == "llama") {
                Some(core.to_owned())
            } else {
                plural_word(core, n == 0)
            };
            if changed && APPOSITION.contains(&low.as_str()) {
                // stays as it is
            } else if let Some(pl) = pl {
                if pl != core {
                    out[i] = mark(lead, core, &pl, trail);
                    changed = true;
                }
            } else {
                // unknown plural: leave the word, and do not guess at the ones after it
                break;
            }
        }
        head = !trail.contains([':', ',', ';', '(', '–', '—']);
        if !head {
            break;
        }
    }
    if changed { out.concat() } else { it.to_owned() }
}

fn mark(lead: &str, core: &str, plural: &str, trail: &str) -> String {
    format!("{lead}{core}[pl:\"{plural}\"]{trail}")
}

/// Plural of an Italian noun or adjective; None if unknown.
fn plural_word(word: &str, noun: bool) -> Option<String> {
    if !is_word(word) {
        return None;
    }
    let w = word.to_lowercase();
    if let Some((_, pl)) = IRREGULAR.iter().find(|(s, _)| *s == w) {
        return Some(case_like(word, pl));
    }
    let chars: Vec<char> = w.chars().collect();
    let last = *chars.last()?;
    if INVARIABLE.contains(&w.as_str())
        || (!noun && COLOUR_NOUNS.contains(&w.as_str()))
        || "àèéìòóùiy".contains(last)
        || !"aeo".contains(last)
        || chars.len() < 2
        || w.ends_with("che")
        || w.ends_with("ghe")
    {
        return Some(word.to_owned()); // accented, in -i, foreign in a consonant, invariable
    }
    let stem = |cut: usize, add: &str| -> String { chars[..chars.len() - cut].iter().collect::<String>() + add };
    if w.ends_with("ista") {
        return Some(case_like(word, &stem(1, "i")));
    }
    if MASC_A.contains(&w.as_str()) {
        return Some(case_like(word, &stem(1, "i")));
    }
    if w.ends_with("ico") && chars.len() >= 6 {
        return Some(case_like(word, &stem(2, "ci"))); // magico, mistico, tecnico
    }
    for (end, pl) in [("co", "chi"), ("go", "ghi"), ("ca", "che"), ("ga", "ghe"), ("io", "i")] {
        if w.ends_with(end) {
            return Some(case_like(word, &stem(end.len(), pl)));
        }
    }
    if w.ends_with("cia") || w.ends_with("gia") {
        let before = chars.len().checked_sub(4).map(|i| chars[i]);
        return Some(case_like(
            word,
            &if before.is_some_and(|c| !"aeiou".contains(c)) { stem(2, "e") } else { stem(1, "e") },
        ));
    }
    match last {
        'o' | 'e' => Some(case_like(word, &stem(1, "i"))),
        'a' => Some(case_like(word, &stem(1, "e"))),
        _ => None,
    }
}

fn case_like(model: &str, word: &str) -> String {
    if model.chars().count() > 1 && model.chars().all(|c| !c.is_lowercase()) {
        return word.to_uppercase();
    }
    if model.chars().next().is_some_and(char::is_uppercase) {
        let mut cs = word.chars();
        return cs.next().map(|c| c.to_uppercase().chain(cs).collect()).unwrap_or_default();
    }
    word.to_owned()
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphabetic())
}

fn words_of(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !c.is_alphabetic()).filter(|w| !w.is_empty())
}

/// Words and the white space between them, in order.
fn split_keeping_spaces(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut in_space = None;
    for (i, c) in s.char_indices() {
        let sp = c.is_whitespace();
        match in_space {
            Some(prev) if prev != sp => {
                parts.push(&s[start..i]);
                start = i;
            }
            _ => {}
        }
        in_space = Some(sp);
    }
    if start < s.len() {
        parts.push(&s[start..]);
    }
    parts
}

/// `<c=@flavor>Word</c>` -> (`<c=@flavor>`, `Word`, `</c>`).
fn peel_tags(mut core: &str) -> (&str, &str, &str) {
    let whole = core;
    while core.starts_with('<') {
        match core.find('>') {
            Some(i) => core = &core[i + 1..],
            None => break,
        }
    }
    let head = &whole[..whole.len() - core.len()];
    let mut end = core.len();
    while core[..end].ends_with('>') {
        match core[..end].rfind('<') {
            Some(i) => end = i,
            None => break,
        }
    }
    (head, &core[..end], &core[end..])
}

/// (leading punctuation, the word, trailing punctuation). `%`, `<`, `>`, `[` and `]` belong to
/// the word: they are markup.
fn edges(tok: &str) -> (&str, &str, &str) {
    let in_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = tok.find(|c: char| in_word(c) || "%<[".contains(c)).unwrap_or(tok.len());
    let end = tok[start..]
        .rfind(|c: char| in_word(c) || "%>]".contains(c))
        .map_or(start, |i| start + i + tok[start + i..].chars().next().map_or(0, char::len_utf8));
    (&tok[..start], &tok[start..end], &tok[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_names() {
        assert_eq!(
            pluralize("Decade Enhancement Station[s]", "Stazione di potenziamento del decennio"),
            "Stazione[pl:\"Stazioni\"] di potenziamento del decennio"
        );
        assert_eq!(pluralize("Piece[s] of Scrap Metal", "Pezzo di metallo di scarto"), "Pezzo[pl:\"Pezzi\"] di metallo di scarto");
        assert_eq!(pluralize("Broken Bottle[s]", "Flacone rotto"), "Flacone[pl:\"Flaconi\"] rotto[pl:\"rotti\"]");
        assert_eq!(pluralize("Wurm Egg[s]", "Uovo Wurm"), "Uovo[pl:\"Uova\"] Wurm");
        assert_eq!(pluralize("Glass[pl:\"Glasses\"] of Melon Juice", "Bicchiere di succo di melone"), "Bicchiere[pl:\"Bicchieri\"] di succo di melone");
    }

    #[test]
    fn spelling() {
        assert_eq!(plural_word("Amico", true).as_deref(), Some("Amici"));
        assert_eq!(plural_word("Magico", true).as_deref(), Some("Magici"));
        assert_eq!(plural_word("Arancia", true).as_deref(), Some("Arance"));
        assert_eq!(plural_word("Valigia", true).as_deref(), Some("Valigie"));
        assert_eq!(plural_word("Pozione", true).as_deref(), Some("Pozioni"));
        assert_eq!(plural_word("Borsa", true).as_deref(), Some("Borse"));
        assert_eq!(plural_word("Tè", true).as_deref(), Some("Tè"));
        assert_eq!(plural_word("Skritt", true).as_deref(), Some("Skritt"));
        assert_eq!(plural_word("Pilota", true).as_deref(), Some("Piloti"));
    }

    #[test]
    fn untouched() {
        // no mark in English, %num% in the text, or already marked
        assert_eq!(pluralize("Decade Station", "Stazione del decennio"), "Stazione del decennio");
        assert_eq!(pluralize("%num1% Item[s]", "Oggetto %num1%"), "Oggetto %num1%");
        assert_eq!(pluralize("Truffle[s]", "Tartufo[pl:\"Tartufi\"]"), "Tartufo[pl:\"Tartufi\"]");
        // a word that is also English is kept
        assert_eq!(pluralize("Unconscious Skritt[s]", "Skritt inconscio"), "Skritt inconscio[pl:\"inconsci\"]");
    }

    #[test]
    fn markup_is_kept() {
        assert_eq!(pluralize("<c=@flavor>Truffle[s]</c>", "<c=@flavor>Tartufo</c>"), "<c=@flavor>Tartufo[pl:\"Tartufi\"]</c>");
        assert!(crate::markup::token_mismatch("Truffle[s]", &pluralize("Truffle[s]", "Tartufo")).is_empty());
    }

    #[test]
    fn stops_at_comma_and_unknown() {
        assert_eq!(pluralize("Flask[s], Large", "Fiasca, grande"), "Fiasca[pl:\"Fiasche\"], grande");
        assert_eq!(pluralize("Thing[s]", "Skritt"), "Skritt");
    }
}
