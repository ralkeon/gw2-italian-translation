//! Names that stay in English: professions, specializations, skills, traits, legendary items,
//! boons and conditions, so that Italian players speak the same game language as everyone else
//! ("Firebrand", "Signet of Rage", "Binding of Ipos", "Quickness"). The server takes the list from
//! the official API (crates/server/src/names.rs); here is how a text is checked against it.
//!
//! A name is kept where it surely is one:
//! - the whole text is the name (a skill's tooltip title): the text stays as it is;
//! - a name of more words, as written, in a sentence ("the legendary short bow The Dreamer"); in a
//!   title only set apart from its other words ("Mists Research: Strife Unending", not "Rough
//!   Sharpening Stone", an item, nor "Gift of The Minstrel");
//! - a name of one word inside a sentence only with its capital, not at the start of a sentence,
//!   not in a text of capitalised words (an item title), and not next to another capitalised word
//!   ("Agony Resistance", "Gift of Might": other names that contain it).
//!
//! One-word names that are plain English words (Fire, Charge, Escape...) are left out: the same
//! text is often something else in the game.

use std::collections::HashMap;

/// One-word names that are common English: as a whole text they are often a label, a damage type,
/// a key, an achievement, not the skill.
const COMMON: &[&str] = &[
    // Core specializations named after a word the game uses for something else too: the elements
    // (attunements, damage), skill types ("Corruption."), "Chaos Aura", the Tools of a menu. The
    // other core specializations (Spite, Zeal, Marksmanship...) are names like the elite ones.
    "Air", "Arms", "Chaos", "Defense", "Earth", "Fire", "Tools", "Water", "Corruption",
    // Skills and traits.
    "Abstraction", "Anchor", "Anguish", "Awakening", "Bandage", "Banish", "Barrage", "Bite", "Blink", "Bloodlust",
    "Blowtorch", "Blunderbuss", "Bluster", "Boil", "Bomb", "Bonfire", "Bop", "Brilliance", "Brutality", "Bulwark",
    "Bumble", "Buoy", "Carnivore", "Charge", "Chomp", "Chop", "Cloudburst", "Comet", "Conjurer", "Consume",
    "Convergence", "Counterattack", "Crescendo", "Crossfire", "Cultivate", "Cyclone", "Dash", "Dazzling", "Deception",
    "Decoy", "Deluge", "Detonate", "Dissonance", "Distortion", "Distress", "Diversion", "Doom", "Dread", "Earthquake",
    "Eclipse", "Empower", "Empowered", "Energize", "Entangle", "Epidemic", "Eruption", "Escape", "Evocation", "Evolve",
    "Executioner", "Farsighted", "Feast", "Feedback", "Fireball", "Firestorm", "Fissure", "Flamethrower", "Flashbang",
    "Flourish", "Fragility", "Frenzy", "Furious", "Gale", "Galvanize", "Gash", "Germinate", "Geyser", "Gluttony",
    "Gorge", "Grenade", "Grenadier", "Gust", "Hamstring", "Harden", "Harmonize", "Harpoon", "Haste", "Haunt", "Hurl",
    "Ignite", "Impale", "Improvisation", "Ineptitude", "Inferno", "Inscription", "Isolate", "Jab", "Jet", "Jetstream",
    "Journey", "Juggernaut", "Jurisdiction", "Kick", "Kneel", "Locked", "Lunge", "Magnet", "Maul", "Mayhem",
    "Mending", "Mercy", "Meteor", "Mimic", "Mirror", "Monsoon", "Mug", "Napalm", "Nightfall", "Notoriety",
    "Opportunist", "Outrage", "Parry", "Payback", "Peacekeeper", "Pelt", "Phoenix", "Pitfall", "Pounce",
    "Premeditation", "Preparedness", "Preservation", "Pull", "Punch", "Purification", "Purify", "Quake", "Rampage",
    "Redemption", "Rejuvenate", "Rejuvenation", "Remorseless", "Rend", "Repeater", "Reverberation", "Ricochet", "Rip",
    "Riposte", "Ripple", "Riptide", "Roar", "Rocket", "Rumble", "Rush", "Sanctuary", "Sandstorm", "Sharpshooter",
    "Shackle", "Shelter", "Shredding", "Shrapnel", "Shrike", "Silencer", "Singularity", "Siphon", "Slam", "Slash",
    "Slice", "Smack", "Stab", "Steal", "Steam", "Stomp", "Strike", "Superspeed", "Swap", "Swipe", "Swoop", "Takedown",
    "Terrify", "Terror", "Thump", "Tornado", "Transfusion", "Transmute", "Tremor", "Trickster", "Twister", "Undercurrent",
    "Undertow", "Unload", "Unravel", "Updraft", "Uppercut", "Vacuum", "Vampiric", "Vault", "Veil", "Vengeance",
    "Vindication", "Volcano", "Volley", "Vortex", "Wellspring", "Whirlpool", "Whirlwind", "Wildfire", "Withdraw",
    "Wrath", "Slipstream",
    // Legendary items.
    "Bolt", "Vision",
];

/// Words that join the words of a title: "Gift of Might", "Fury of the Lich".
const JOINERS: &[&str] = &["of", "the", "de", "a", "an", "to", "for", "in", "on"];

/// Lowercase words a title has too: "Gift of Fire and Ice".
const TITLE_WORDS: &[&str] = &["and", "or", "with"];

/// The names, by their first word.
#[derive(Default, Clone)]
pub struct Names {
    by_first: HashMap<String, Vec<String>>,
    /// The texts that are a stat prefix ("Berserker's") or an item named after one ("Berserker's
    /// Silk Insignia"), with the prefix: it stays in English, without the "'s" and the same in the
    /// plural, after the Italian noun ("Insegna in seta Berserker").
    affixed: HashMap<String, String>,
}

/// A word of a text: letters, digits and the apostrophes, hyphens and dots inside a name.
#[derive(Clone, Copy)]
struct Word<'a> {
    text: &'a str,
    start: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// The words of `s`, outside `<...>` tags and `&...;` entities (the XML of markup.rs).
fn words(s: &str) -> Vec<Word<'_>> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut skip_until: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(end) = skip_until {
            if c == end {
                skip_until = None;
            }
            continue;
        }
        let inner = start.is_some() && matches!(c, '\'' | '’' | '-' | '.') && s[i + c.len_utf8()..].chars().next().is_some_and(is_word_char);
        if is_word_char(c) || inner {
            start.get_or_insert(i);
            continue;
        }
        if let Some(from) = start.take() {
            out.push(Word { text: &s[from..i], start: from });
        }
        match c {
            '<' => skip_until = Some('>'),
            '&' => skip_until = Some(';'),
            _ => {}
        }
    }
    if let Some(from) = start {
        out.push(Word { text: &s[from..], start: from });
    }
    out
}

fn capitalised(w: &str) -> bool {
    w.chars().next().is_some_and(char::is_uppercase)
}

/// Whether what comes before byte `at` of `s` starts a sentence: nothing, or a full stop, a colon,
/// a line break or a tag (a placeholder of markup.rs, often a line break or a colour).
fn sentence_start(s: &str, at: usize) -> bool {
    s[..at].trim_end_matches([' ', '"', '“', '(', '«']).chars().next_back().is_none_or(|c| matches!(c, '.' | '!' | '?' | ':' | '\n' | '>'))
}

impl Names {
    /// The list, without the ones that cannot be told from common English (`COMMON`).
    pub fn new<I: IntoIterator<Item = S>, S: AsRef<str>>(list: I) -> Self {
        let mut by_first: HashMap<String, Vec<String>> = HashMap::new();
        for name in list {
            let name = name.as_ref().trim();
            let found = words(name);
            let Some(first) = found.first() else { continue };
            if found.len() == 1 && COMMON.contains(&name) || !capitalised(name) {
                continue;
            }
            let list = by_first.entry(first.text.to_owned()).or_default();
            if !list.iter().any(|n| n == name) {
                list.push(name.to_owned());
            }
        }
        // The longest first: "Signet of Rage" before "Signet".
        for list in by_first.values_mut() {
            list.sort_by_key(|n| std::cmp::Reverse(n.len()));
        }
        Self { by_first, affixed: HashMap::new() }
    }

    /// With the stat prefixes: `list` has (a prefix or an item named after one, the prefix).
    pub fn with_affixes<I: IntoIterator<Item = (S, S)>, S: AsRef<str>>(mut self, list: I) -> Self {
        self.affixed = list.into_iter().map(|(text, affix)| (text.as_ref().trim().to_owned(), affix.as_ref().trim().to_owned())).collect();
        self
    }

    /// The prefix `en` is or is named after, as the Italian keeps it ("Berserker"), as part of `en`:
    /// the whole text but its placeholders ("%str1%") and plural marks.
    pub fn affix<'a>(&self, en: &'a str) -> Option<&'a str> {
        let bare = crate::markup::strip_grammar(en);
        let bare = strip_tokens(bare.trim());
        let affix = self.affixed.get(bare)?;
        let base = affix.strip_suffix("'s").or_else(|| affix.strip_suffix("’s")).unwrap_or(affix);
        let at = en.find(base)?;
        Some(&en[at..at + base.len()])
    }

    /// The longest name at word `w` of `s`, and where it ends.
    fn at<'a>(&'a self, s: &str, w: Word) -> Option<(&'a str, usize)> {
        self.by_first.get(w.text)?.iter().find_map(|name| {
            let end = w.start + name.len();
            // `get`: `end` may fall inside a character of the text ("•").
            let after = s.get(end..)?.chars().next();
            (s.get(w.start..end) == Some(name.as_str()) && after.is_none_or(|c| !is_word_char(c))).then_some((name.as_str(), end))
        })
    }

    /// Whether the text is a title ("Rough Sharpening Stone", "Gift of Might"): outside the names,
    /// no lowercase word but the ones that join the words of a title. A sentence has some.
    fn is_title(&self, s: &str, found: &[Word]) -> bool {
        let mut i = 0;
        while i < found.len() {
            if let Some((_, end)) = self.at(s, found[i]) {
                i = found.iter().position(|x| x.start >= end).unwrap_or(found.len());
                continue;
            }
            let w = found[i].text;
            if !capitalised(w) && w.chars().count() >= 3 && !JOINERS.contains(&w) && !TITLE_WORDS.contains(&w) && w.chars().any(char::is_alphabetic) {
                return false;
            }
            i += 1;
        }
        true
    }

    pub fn len(&self) -> usize {
        self.by_first.values().map(Vec::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.by_first.is_empty()
    }

    /// The name the whole of `en` is, if it is one (spaces around it apart).
    ///
    /// A name that is also marked for the plural ("Silver Ingot[s]") is the whole text too: it
    /// stays as it is, marks included, so that the game still has its plural.
    pub fn whole(&self, en: &str) -> Option<String> {
        let en = en.trim();
        let bare = crate::markup::strip_grammar(en);
        let bare = bare.trim();
        let first = words(bare).into_iter().next()?;
        let name = self.by_first.get(first.text)?.iter().find(|n| *n == bare)?;
        Some(if bare == en { name.clone() } else { en.to_owned() })
    }

    /// The names in `s` (plain English or the XML of markup.rs) to keep, as byte ranges, in order,
    /// not overlapping. The whole text being a name counts too.
    pub fn find(&self, s: &str) -> Vec<(usize, usize)> {
        let found = words(s);
        let title = self.is_title(s, &found);
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < found.len() {
            let w = found[i];
            let Some((name, end)) = self.at(s, w) else {
                i += 1;
                continue;
            };
            let next = found.iter().position(|x| x.start >= end).unwrap_or(found.len());
            let alone = found.len() == 1 && s.trim() == name;
            // Nothing but spaces between it and the punctuation or the ends of the text.
            let isolated = s[..w.start].trim_end().chars().next_back().is_none_or(|c| !is_word_char(c))
                && s[end..].trim_start().chars().next().is_none_or(|c| !is_word_char(c));
            let keep = alone || name.contains(' ') && (!title || isolated) || {
                let neighbour = |k: Option<usize>, step: isize| -> bool {
                    // The next word on that side, past one joiner: capitalised means a longer title
                    // ("Gift of Might"), but right next to it the first word of a sentence does not
                    // ("Grants Might").
                    let mut k = k;
                    for joined in [false, true] {
                        let Some(at) = k else { return false };
                        let x = found[at].text;
                        if !joined && JOINERS.contains(&x) {
                            k = at.checked_add_signed(step).filter(|&n| n < found.len());
                            continue;
                        }
                        return capitalised(x) && (joined || !sentence_start(s, found[at].start));
                    }
                    false
                };
                !title && !sentence_start(s, w.start) && !neighbour(i.checked_sub(1), -1) && !neighbour((next < found.len()).then_some(next), 1)
            };
            if keep {
                out.push((w.start, end));
                i = next;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The names `en` keeps that `it` does not have, as written in `en`.
    pub fn missing<'a>(&self, en: &'a str, it: &str) -> Vec<&'a str> {
        let mut out: Vec<&str> = Vec::new();
        for (from, to) in self.find(en) {
            let name = &en[from..to];
            let there = it.match_indices(name).any(|(at, _)| {
                let (before, after) = (it[..at].chars().next_back(), it[at + name.len()..].chars().next());
                before.is_none_or(|c| !is_word_char(c)) && after.is_none_or(|c| !is_word_char(c))
            });
            if !there && !out.contains(&name) {
                out.push(name);
            }
        }
        if let Some(affix) = self.affix(en)
            && !has_word(it, &[affix])
            && !out.contains(&affix)
        {
            out.push(affix);
        }
        // A name marked for the plural inside ("Superior Rune[s] of the Dragonhunter") is no name
        // to `find`: the whole text is.
        let en_whole = en.trim();
        if out.is_empty() && self.whole(en).is_some_and(|w| w == en_whole && w != crate::markup::strip_grammar(en_whole)) && it.trim() != en_whole {
            out.push(en_whole);
        }
        out
    }
}

/// `s` without the placeholders around it: "%str1%%str2%Berserker's Axe%str3%%str4%".
fn strip_tokens(s: &str) -> &str {
    let mut s = s;
    loop {
        let before = s;
        if let Some(rest) = s.strip_prefix('%').and_then(|r| r.find('%').map(|i| &r[i + 1..])) {
            s = rest;
        }
        if let Some(rest) = s.strip_suffix('%').and_then(|r| r.rfind('%').map(|i| &r[..i])) {
            s = rest;
        }
        if s == before {
            return s.trim();
        }
    }
}

/// Words of the game's mechanics that stay in English too, in any case and in sentences, with the
/// forms they take: Italian players say "boon" and "condi", "benedizione" is no game word.
/// "Black Lion", the company, too: players say "forziere Black Lion", never "Leone Nero". And
/// "focus", the off-hand weapon: "focalizzatore" and "concentrazione" are no game words. And the
/// upgrades "rune", "sigil" and "relic", the words of the wiki, the builds and the Trading Post,
/// and the attributes ("+100 Power"): "potenza" was the boon Might too.
pub const TERMS: &[(&str, &[&str])] = &[
    ("boon", &["boon", "boons"]),
    ("condition", &["condition", "conditions"]),
    ("Black Lion", &["black lion"]),
    ("focus", &["focus", "foci"]),
    ("rune", &["rune", "runes"]),
    ("sigil", &["sigil", "sigils"]),
    ("relic", &["relic", "relics"]),
    ("Power", &["power"]),
    ("Precision", &["precision"]),
    ("Toughness", &["toughness"]),
    ("Vitality", &["vitality"]),
    ("Ferocity", &["ferocity"]),
    ("Healing Power", &["healing power"]),
    ("Concentration", &["concentration"]),
    ("Expertise", &["expertise"]),
    ("Agony Resistance", &["agony resistance"]),
    // The boons and conditions by name, also where the English writes them in lowercase ("grant
    // might" is "conferisci Might"): the verbs stay Italian (chill, cripple, blind, poison).
    ("Aegis", &["aegis"]),
    ("Alacrity", &["alacrity"]),
    ("Fury", &["fury"]),
    ("Might", &["might"]),
    ("Protection", &["protection"]),
    ("Quickness", &["quickness"]),
    ("Regeneration", &["regeneration"]),
    ("Resistance", &["resistance"]),
    ("Resolution", &["resolution"]),
    ("Stability", &["stability"]),
    ("Swiftness", &["swiftness"]),
    ("Vigor", &["vigor"]),
    ("Superspeed", &["superspeed"]),
    ("Stealth", &["stealth"]),
    ("Distortion", &["distortion"]),
    ("Bleeding", &["bleeding"]),
    ("Blinded", &["blinded", "blindness"]),
    ("Burning", &["burning"]),
    ("Chilled", &["chilled"]),
    ("Confusion", &["confusion"]),
    ("Crippled", &["crippled"]),
    ("Fear", &["fear"]),
    ("Immobile", &["immobile", "immobilized"]),
    ("Poisoned", &["poisoned", "poison"]),
    ("Slow", &["slow"]),
    ("Taunt", &["taunt"]),
    ("Torment", &["torment"]),
    ("Vulnerability", &["vulnerability"]),
    ("Weakness", &["weakness"]),
    ("Knockback", &["knockback", "knockbacks"]),
    ("Knockdown", &["knockdown", "knockdowns"]),
];

/// Whether `s` has one of `forms` (one word or more, in a row) as words, in any case.
fn has_word(s: &str, forms: &[&str]) -> bool {
    let found = words(s);
    forms.iter().any(|f| {
        let want: Vec<&str> = f.split(' ').collect();
        found.windows(want.len()).any(|w| w.iter().zip(&want).all(|(w, f)| w.text.eq_ignore_ascii_case(f)))
    })
}

/// The terms (`TERMS`) `en` has that `it` lost. The prose is left out: the flavour text, the
/// words in quotes and the texts someone says or writes ("I", "my", "we"), where "power",
/// "weakness" or "a boon" are plain words, not the mechanic; but for the upgrades and Black Lion.
/// "the power of", "power to", "the wizard's power" are never the attribute.
pub fn terms_missing(en: &str, it: &str) -> Vec<&'static str> {
    let mechanics = mechanics_part(en);
    let spoken = words(&mechanics).iter().any(|w| SPOKEN.contains(&w.text));
    TERMS
        .iter()
        .filter(|(term, _)| !spoken || ALWAYS_TERMS.contains(term))
        .filter(|(term, forms)| {
            let found = if *term == "Power" { has_word(&generic_power(&mechanics), forms) } else { has_word(&mechanics, forms) };
            found && !has_word(it, forms)
        })
        .map(|(term, _)| *term)
        .collect()
}

/// The terms that are the game's even in prose.
const ALWAYS_TERMS: &[&str] = &["Black Lion", "rune", "sigil", "relic", "Agony Resistance", "Healing Power"];
/// The words of someone speaking or writing.
const SPOKEN: &[&str] = &["I", "I'm", "I'll", "I've", "I'd", "me", "my", "mine", "we", "we're", "we'll", "we've", "our", "us", "I’m", "I’ll", "I’ve", "I’d", "we’re"];

/// `en` without the flavour text (`<c=@flavor>...</c>`) and the words in quotes.
fn mechanics_part(en: &str) -> String {
    let mut out = String::with_capacity(en.len());
    let mut rest = en;
    while let Some(at) = rest.find("<c=@flavor>") {
        out.push_str(&rest[..at]);
        rest = rest[at..].find("</c>").map_or("", |end| &rest[at + end + 4..]);
    }
    out.push_str(rest);
    let mut bare = String::with_capacity(out.len());
    let mut quoted = false;
    for c in out.chars() {
        match c {
            '"' | '“' | '”' => quoted = !quoted,
            _ if !quoted => bare.push(c),
            _ => {}
        }
    }
    bare
}

/// `s` without "power" where it is not the attribute: "the power of", "power to", "power in".
fn generic_power(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = s.to_owned();
    for (at, _) in lower.match_indices("power").collect::<Vec<_>>().into_iter().rev() {
        let (before, after) = (lower[..at].trim_end(), lower[at + 5..].trim_start());
        let generic = before.ends_with(" the") || before == "the" || before.ends_with(" of")
            || ["'s", "’s", " his", " her", " its", " their"].iter().any(|w| before.ends_with(w))
            || ["of ", "to ", "in ", "into ", "from "].iter().any(|w| after.starts_with(w));
        if generic && !before.ends_with("healing") {
            out.replace_range(at..at + 5, "_____");
        }
    }
    out
}

/// Whether `it` elides into a term ("l'boon", "dell'condition"). The boons that start with a vowel
/// elide as Italian words do ("dell'Aegis").
pub fn term_elided(it: &str) -> bool {
    let lower = it.to_lowercase();
    TERMS.iter().filter(|(term, _)| !["Aegis", "Alacrity", "Immobile"].contains(term)).any(|(term, _)| {
        let term = term.to_lowercase();
        lower.contains(&format!("'{term}")) || lower.contains(&format!("’{term}"))
    })
}

/// Attributes of the terms that stay whole in English: "+100 Condition Damage".
pub const TERM_ATTRIBUTES: &[&str] = &["Condition Damage", "Condition Duration", "Boon Duration", "Black Lion Trading Company"];

/// The words of the English `en` that a rewrite `new` of the Italian `old` brought in while not
/// part of `keep` (the names and terms to put back), of `TERM_ATTRIBUTES` nor of the skill labels
/// (`markup::skill_labels`): "+5 Armatura"
/// become "+5 Armor" is a word translated back, not a name put back.
pub fn english_brought_in<'a>(en: &str, old: &str, new: &'a str, keep: &[&str]) -> Vec<&'a str> {
    let lower = |s: &str| words(s).iter().map(|w| w.text.to_lowercase()).collect::<std::collections::HashSet<_>>();
    let labels = crate::markup::skill_labels(en);
    let (en, old) = (lower(en), lower(old));
    let mut allowed = std::collections::HashSet::new();
    for k in keep.iter().chain(TERM_ATTRIBUTES).chain(&labels) {
        allowed.extend(lower(k));
    }
    for (_, forms) in TERMS {
        for f in *forms {
            allowed.extend(lower(f));
        }
    }
    words(new)
        .into_iter()
        .map(|w| w.text)
        .filter(|w| {
            let l = w.to_lowercase();
            w.chars().any(char::is_alphabetic) && en.contains(&l) && !old.contains(&l) && !allowed.contains(&l)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_names_come_back_in_english() {
        let en = "+7 Condition Damage\n+5 Armor";
        assert_eq!(english_brought_in(en, "+7 Danni da condizione +5 Armatura", "+7 Condition Damage\n+5 Armor", &["condition"]), ["Armor"]);
        assert!(english_brought_in(en, "+7 Danni da condizione +5 Armatura", "+7 Condition Damage +5 Armatura", &["condition"]).is_empty());
        let en = "<c=@abilitytype>Cantrip.</c> Gain a barrier for each boon.";
        let old = "<c=@abilitytype>Incantesimo minore.</c> Ottieni una barriera per ogni benedizione.";
        // The skill labels stay in English (markup::ability_labels): "Cantrip" is no word brought back.
        assert!(english_brought_in(en, old, "<c=@abilitytype>Cantrip.</c> Ottieni una barriera per ogni boon.", &["boon"]).is_empty());
        assert!(english_brought_in(en, old, "<c=@abilitytype>Incantesimo minore.</c> Ottieni una barriera per ogni boon.", &["boon"]).is_empty());
    }

    #[test]
    fn terms_in_any_case() {
        assert_eq!(terms_missing("Barrier per Boon", "Barriera per benedizione"), ["boon"]);
        assert!(terms_missing("Barrier per Boon", "Barriera per boon").is_empty());
        assert_eq!(terms_missing("Removes boons and conditions.", "Rimuove i boon e le condizioni."), ["condition"]);
        assert_eq!(terms_missing("Gain Might.", "Ottieni potenza."), ["Might"]);
        // A word that only starts like one is another word.
        assert!(terms_missing("Boonless foes", "Nemici").is_empty());
        assert!(term_elided("Rimuove l'boon"));
        assert_eq!(terms_missing("Open a Black Lion Chest.", "Apri un forziere del Leone Nero."), ["Black Lion"]);
        assert!(terms_missing("Open a Black Lion Chest.", "Apri un forziere Black Lion.").is_empty());
        assert!(terms_missing("A black cat and a lion.", "Un gatto nero e un leone.").is_empty());
        assert!(term_elided("Statuetta dell'Black Lion"));
        assert!(!term_elided("Rimuove il boon"));
        assert_eq!(terms_missing("Spindrift Focus", "Focalizzatore Spindrift"), ["focus"]);
        assert!(terms_missing("Spindrift Focus", "Focus Spindrift").is_empty());
        assert!(terms_missing("Focused Strikes", "Colpi concentrati").is_empty());
    }

    fn names() -> Names {
        Names::new([
            "Signet of Rage", "Signet", "The Dreamer", "Might", "Resistance", "Firebrand", "Fire", "Quickness", "Aurene's Fang",
            "Master-at-Arms", "Binding of Ipos", "Eternity", "Burning",
        ])
    }

    fn kept(en: &str) -> Vec<&str> {
        names().find(en).into_iter().map(|(a, b)| &en[a..b]).collect()
    }

    #[test]
    fn the_whole_text() {
        let n = names();
        assert_eq!(n.whole("Signet of Rage").as_deref(), Some("Signet of Rage"));
        assert_eq!(n.whole(" Firebrand ").as_deref(), Some("Firebrand"));
        assert_eq!(n.whole("Might").as_deref(), Some("Might"));
        assert_eq!(n.whole("Signet of Rage[s]").as_deref(), Some("Signet of Rage[s]"));
        assert_eq!(n.whole("Signet of Rage Recharge"), None);
        // A plain word is not a name, even alone.
        assert_eq!(n.whole("Fire"), None);
        assert_eq!(kept("Aurene's Fang"), ["Aurene's Fang"]);
        assert_eq!(kept("Master-at-Arms"), ["Master-at-Arms"]);
    }

    #[test]
    fn names_of_more_words_anywhere() {
        assert_eq!(kept("This weapon is used to craft the legendary short bow The Dreamer."), ["The Dreamer"]);
        // The longest name, not the shorter one inside it; whole words only.
        assert_eq!(kept("Use Signet of Rage now."), ["Signet of Rage"]);
        assert!(kept("Use Signet of Rageful now.").is_empty());
        // In a title, only set apart from its other words.
        assert_eq!(kept("Mists Research: The Dreamer"), ["The Dreamer"]);
        assert!(kept("Rough Signet of Rage Stone").is_empty());
        assert!(kept("Gift of The Dreamer").is_empty());
    }

    #[test]
    fn names_of_one_word_only_where_they_surely_are() {
        assert_eq!(kept("Gain 5 stacks of Might when you dodge."), ["Might"]);
        assert_eq!(kept("You grant Quickness and Might to allies."), ["Quickness", "Might"]);
        assert_eq!(kept("the legendary greatsword Eternity by combining it"), ["Eternity"]);
        // Part of another name, at the start of a sentence, in a title, lowercase.
        assert!(kept("+9 Agony Resistance").is_empty());
        assert!(kept("Choose a Gift of Might or a Gift of Magic.").is_empty());
        assert!(kept("Might is granted to allies.").is_empty());
        assert!(kept("Nexus of Eternity Power Unleashed").is_empty());
        assert!(kept("Inflict burning on foes.").is_empty());
        assert!(kept("Signet of Might").is_empty());
    }

    #[test]
    fn markup_is_not_a_word() {
        assert_eq!(kept("<x id=\"0\"/>Grants Might to you.<x id=\"1\"/>"), ["Might"]);
        assert!(kept("<c=@Might>text</c>").is_empty());
        // A name that would end inside a character of the text.
        assert!(kept("Signet of Ra•").is_empty());
    }

    #[test]
    fn stat_prefixes() {
        let n = names().with_affixes([
            ("Berserker's", "Berserker's"),
            ("Berserker's Silk Insignia", "Berserker's"),
            ("Carrion Pearl Broadsword", "Carrion"),
        ]);
        assert_eq!(n.affix("Berserker's Silk Insignia[s]"), Some("Berserker"));
        assert_eq!(n.affix("%str1%%str2%Carrion Pearl Broadsword%str3%%str4%"), Some("Carrion"));
        assert_eq!(n.affix("Berserker's"), Some("Berserker"));
        assert_eq!(n.affix("Berserker's Stance"), None);
        assert_eq!(n.missing("Berserker's Silk Insignia[s]", "Distintivo[pl:\"Distintivi\"] in seta del Berserker"), Vec::<&str>::new());
        assert_eq!(n.missing("Carrion Pearl Broadsword", "Spadone di perla della carogna"), ["Carrion"]);
        assert!(n.missing("Carrion Pearl Broadsword", "Spadone di perla Carrion").is_empty());
        assert_eq!(strip_tokens("%str1%%str2%Axe%str3%%str4%"), "Axe");
        assert_eq!(strip_tokens("100%"), "100%");
    }

    #[test]
    fn attributes_are_terms() {
        assert_eq!(terms_missing("+100 Power\n+70 Precision", "+100 Potenza\n+70 Precisione"), ["Power", "Precision"]);
        assert!(terms_missing("+100 Healing Power", "+100 Healing Power").is_empty());
        assert_eq!(terms_missing("Gain ferocity based on your power.", "Ottieni ferocia in base al tuo Power."), ["Ferocity"]);
        assert!(term_elided("Aumenta l'Expertise"));
    }

    #[test]
    fn prose_has_no_terms() {
        let kuda = "I, Director Kuda, will lead you forward and turn this moment of weakness into one of might.";
        assert!(terms_missing(kuda, "Io vi guiderò e trasformerò questo momento di debolezza in uno di forza.").is_empty());
        assert!(terms_missing("Prepare to accept the wizard's power.", "Preparati ad accettare il potere del mago.").is_empty());
        assert!(terms_missing("Channel power to your Spirit of Wanderlust.", "Incanala potere nel tuo Spirit of Wanderlust.").is_empty());
        assert_eq!(terms_missing("Gain power while in fire attunement.", "Ottieni potenza."), ["Power"]);
        assert!(terms_missing("<c=@flavor>Seal away your fear.</c>", "<c=@flavor>Sigilla la tua paura.</c>").is_empty());
        let spec = "\"My power shapes the world.\"\n\nGain might.";
        assert_eq!(terms_missing(spec, "«Il mio potere plasma il mondo.»\n\nOttieni potenza."), ["Might"]);
        assert_eq!(terms_missing("I found a superior rune.", "Ho trovato una runa superiore."), ["rune"]);
    }

    #[test]
    fn upgrades_are_terms() {
        assert_eq!(terms_missing("Equip a sigil.", "Equipaggia un sigillo."), ["sigil"]);
        assert!(terms_missing("Salvage the runes.", "Ricicla le rune.").is_empty());
        assert_eq!(terms_missing("Relics grant bonuses.", "Le reliquie conferiscono bonus."), ["relic"]);
        // Boons and conditions by name, also in lowercase; the verbs stay Italian.
        assert_eq!(terms_missing("Grant might and inflict bleeding.", "Conferisci potenza e infliggi sanguinamento."), ["Might", "Bleeding"]);
        assert!(terms_missing("Grant might and inflict bleeding.", "Conferisci Might e infliggi Bleeding.").is_empty());
        assert!(terms_missing("Chill and cripple foes.", "Assidera e azzoppa i nemici.").is_empty());
        assert!(!term_elided("Conferisci 3 secondi dell'Aegis."));
    }

    #[test]
    fn missing_in_the_italian() {
        let n = names();
        assert_eq!(n.missing("Signet of Rage", "Sigillo della rabbia"), ["Signet of Rage"]);
        assert!(n.missing("Signet of Rage", "Signet of Rage").is_empty());
        assert_eq!(n.missing("You grant Quickness and Might.", "Conferisci Quickness e Potenza."), ["Might"]);
        let n = Names::new(["Superior Rune of the Dragonhunter"]);
        let en = "Superior Rune[s] of the Dragonhunter";
        assert_eq!(n.missing(en, "Runa[pl:\"Rune\"] superiore[pl:\"superiori\"] del Cacciatore di Draghi"), [en]);
        assert!(n.missing(en, en).is_empty());
    }
}
