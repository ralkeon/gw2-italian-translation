//! Names that stay in English (gw2it_text::names): `gw2it-server names`, run by cron.
//!
//! - `--fetch` takes the list from the official API: professions, specializations, the skills of
//!   the professions, traits, legendary weapons, armour, backs and trinkets, the runes, sigils
//!   and relics; boons and conditions are added here. A name, once known, stays (the API drops
//!   skills it retires). Into `affixes` go the stat prefixes of three attributes or more
//!   ("Berserker's") and the items named after any prefix, the minor ones too ("Malign Silk
//!   Insignia", "Berserker's Silk Insignia"): the prefix
//!   stays in English, the item's noun is Italian ("Insegna in seta Berserker").
//! - Then the machine translations (trust 1 and below) that lost a name get it back: a text that
//!   is the name becomes the name; in any other a language model of translators.json puts the
//!   English name where its Italian is, changing as little as it can (a translator given the
//!   names as placeholders scrambles the sentence around them). What it answers is checked: the
//!   names there, the markup of the English. Hand corrections are only counted. Every rewrite goes
//!   into `history` and `names_log`, and to the addons with /sync.
//! - The words of the mechanics in `gw2it_text::names::TERMS` (boon, condition, focus, rune...) are put back the
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
/// The items with stats: named after their prefix ("Berserker's Pearl Broadsword").
const STAT_TYPES: &[&str] = &["Weapon", "Armor", "Back", "Trinket"];
/// The crafting materials named after a prefix: "Berserker's Silk Insignia".
const STAT_MATERIALS: &[&str] = &["Insignia", "Inscription"];
/// Texts given to the model at once.
const BATCH: usize = 20;
const TEXT_MAX: usize = 4000;
const AUTHOR: &str = "names";

const SYSTEM: &str = "You fix the Italian translation of texts of the game Guild Wars 2. In each text, \
the names in \"keep\" (skills, traits, specializations, professions, legendary items, runes, sigils, relics, \
stat prefixes, boons, conditions) \
must stay exactly as in the English, not translated, so that players can talk about them with players \
of other languages. Rewrite the Italian changing only what is needed: put each English name where its \
Italian translation is, and fix only the words right around it (articles, prepositions: \"l'Elisir B\" \
becomes \"l'Elixir B\"). The article or preposition before an English name follows its first \
letter as if it were Italian: elided before a vowel (\"dell'Ash Legion\", \"nell'Echovald Forest\", \"all'Auric \
Basin\"), \"lo\" before s + consonant or z (\"dello Skyscale\", \"allo Zephyr Sanctum\"), \"di\" before \"The\" \
in a title (\"Manuale di The Shining Blade\"); the gender stays the one the Italian gave it (\"lo Skywatch \
Archipelago\", an arcipelago). In a sentence the \"The\" of a name gives way to the Italian article (\"La Wizard's \
Tower è diversa\", \"la Shining Blade protegge\"), never \"The Wizard's Tower è\". An English name does not change \
in the Italian plural: the singular, as for the boons (\"i Sunspear\", \"gli Elder Dragon\", \"i Seraph\", \
\"i Fractal\"). Runes and sigils by name stay whole in English, with or without Superior, Major or Minor (\"Rune \
of the Daredevil\", \"Sigil of Exposure\"), and so does Agony in item names (\"Infusione di Agony +5\"). The name of a \
skin or armor set stays in English after the Italian noun (\"Ascia Painter's Brilliance\", \"Fucile dei Lightborne\"); \
\"Slumbering\" in an item name is \"sopito\" (\"Spada d'ossidiana sopita\"). Put the English name in place of its Italian translation, never next to it (\"Provocazione \
dell'Holo-Dancer\" is \"Taunt dell'Holo-Dancer\", not \"Provocazione dell'Holo-Dancer Taunt\"), and add nothing \
the Italian did not say. Do not translate anything again, do not change the rest. Keep every %token%, \
every <tag>, every [tag] and every line break exactly as they are.

The lowercase words in \"keep\" are words of the game's mechanics: \"boon\" (benedizione, vantaggio) and \"condition\" (condizione, alterazione). Use the English word wherever the Italian translated it, as an Italian loanword: it does not change in the plural (\"i boon\", \"le condition\"; \"boon\" is masculine, \"condition\" feminine), it is lowercase in a sentence and capitalised in a title or label (\"Barriera per Boon\"), and it is never elided (\"il boon\", \"della condition\", never \"l'boon\"). The attributes \"Condition Damage\", \"Condition Duration\" and \"Boon Duration\" stay whole in English (\"+100 Condition Damage\"). \"Black Lion\" in \"keep\" is the name of the company: it stays in English and does not change, with no article or preposition before it (\"forziere Black Lion\", \"statuetta Black Lion\", \"biglietto di riscatto Black Lion\", never \"del Leone Nero\"); \"Black Lion Trading Company\" stays whole in English. \"focus\" in \"keep\" is the off-hand weapon: it stays in English as an Italian loanword, masculine and the same in the plural (\"il focus\", \"i focus\", \"Focus Spindrift\", \"skin per focus\"), never \"focalizzatore\" or \"concentrazione\". The attributes in \"keep\" (Power, Precision, Toughness, Vitality, Ferocity, Healing Power, Concentration, Expertise, Agony Resistance) stay in English, capitalised, never elided (\"+100 Power\", \"in base al tuo Power\", \"della Ferocity\"), never \"potenza\", \"precisione\", \"robustezza\", \"vitalità\", \"ferocia\". The boons and conditions in \"keep\" (Might, Fury, Quickness, Bleeding, Burning, Vulnerability...) and Knockback, Knockdown stay in English, capitalised, also where the English writes them in lowercase in a sentence (\"grant might\" is \"conferisci Might\", \"inflict bleeding\" is \"infliggi Bleeding\"), never elided except the ones starting with a vowel (\"dell'Aegis\"); the verbs stay Italian (\"chill\" assiderare, \"cripple\" azzoppare, \"blind\" accecare, \"poison\" avvelenare). Nothing else goes back to English; the skill types and labels in <c=@abilitytype> (\"Glamour.\", \"Signet Passive:\") stay in English, as the English has them. \"rune\", \"sigil\" and \"relic\" in \"keep\" are the upgrades: they stay in English as Italian loanwords, the same in the plural (\"la rune\", \"le rune\", \"il sigil\", \"i sigil\", \"la relic\", \"le relic\"), never \"runa\", \"sigillo\", \"reliquia\". A stat prefix in \"keep\" (\"Berserker\", \"Carrion\", \"Knight\") stays in English without the \"'s\", after the Italian noun, with no article or preposition and the same in the plural (\"Insegna in seta Berserker\", \"Spadone di perla Carrion\", \"Cappotto esaltato Knight\"), never \"del Berserker\" or \"della Carogna\". A name before the prefix stays after it with \"di\" (\"Beigarth's Knight Insignia\" is \"Insegna Knight di Beigarth\").

The words of the mechanics in \"keep\" are also plain English words: put one back only where it is the game's mechanic. A boon or condition is the mechanic where a skill, trait or item grants, inflicts, removes, counts or is about it (\"grant might\", \"inflict bleeding\", \"chilled foes\", \"Might on Hit\", \"Swiftness When Hit\", \"Damage vs. Burning\", \"Vulnerability Duration\"); an attribute where it is the stat (\"+25 Power\", \"gain power\", \"based on your power\"); \"focus\" where it is the weapon; \"rune\", \"sigil\", \"relic\" where they are the upgrades. It is NOT the mechanic, and its Italian stays, in dialogue and story text (\"your might and wits\", \"what real power looks like\", \"the quaggans need protection\", \"under the protection of the Lionguard\", \"show any weakness\", \"follow the fear\", \"killed with precision\", \"extreme concentration\", \"the power of the gods\", \"a great boon\", \"in good condition\"); in the names of things, places and events where the word only describes them (\"Poison Cloud\" Nube di veleno, \"Poisoned Dagger\" Pugnale avvelenato, \"Burning Oil\" olio bollente, \"Power Source\" and \"Power Core\" fonte e nucleo di energia, \"Halla's Fury\" Furia di Halla, \"Electrical Distortion\" distorsione elettrica, \"Ice Protection\"); and where it is a verb or another part of speech (\"slow the spread\", \"burning everything\", \"the sweat burning your eyes\", \"bleeding with miasma\", \"they poison the quaggans\", \"power the golem\", \"stealth into the depths\", \"this might take a while\", \"focus your mind\"). List such words in \"plain\" and leave their Italian as it is: {\"n\": <n>, \"it\": \"<the Italian with the other names put back>\", \"plain\": [\"Power\"]}; when no word of \"keep\" is to be put back, answer {\"n\": <n>, \"skip\": true}. When unsure, leave the Italian: a word left in Italian is a small loss, a broken sentence is not.

Answer with JSON: {\"texts\": [{\"n\": <the text's n>, \"it\": \"<the fixed Italian>\"}]}";

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

/// The names with their kind, and the affixes (text, prefix).
type FromApi = (Vec<(String, &'static str)>, Vec<(String, String)>);

/// The names of the official API, by kind, and the affixes.
fn from_api() -> Result<FromApi, String> {
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
    // All the prefixes name items ("Malign Silk Insignia"), but alone only those of three attributes
    // or more: the minor ones alone ("Healing", "Vital") are plain words.
    let mut stats = std::collections::HashMap::new();
    let mut affixes: Vec<(String, String)> = Vec::new();
    for v in all(&agent, "itemstats")? {
        let affix = name(&v);
        if affix.contains(" and ") || !usable(&affix) {
            continue;
        }
        if v["attributes"].as_array().is_some_and(|a| a.len() >= 3) && !affixes.iter().any(|(a, _)| *a == affix) {
            affixes.push((affix.clone(), affix.clone()));
        }
        stats.insert(v["id"].as_i64().unwrap_or(-1), affix);
    }
    for v in all(&agent, "items")? {
        let kind = v["type"].as_str().unwrap_or("");
        let item = name(&v);
        if v["rarity"] == "Legendary" && ITEM_TYPES.contains(&kind) || kind == "Relic" {
            out.push((item.clone(), "item"));
        }
        if kind == "UpgradeComponent" && matches!(v["details"]["type"].as_str(), Some("Rune" | "Sigil")) {
            out.push((item.clone(), "upgrade"));
        }
        // Named after its own prefix, or a material of a prefix ("Berserker's Silk Insignia").
        let own = STAT_TYPES
            .contains(&kind)
            .then(|| v["details"]["infix_upgrade"]["id"].as_i64().and_then(|id| stats.get(&id)))
            .flatten()
            .filter(|a| item.starts_with(&format!("{a} ")));
        let material = (kind == "CraftingMaterial" && STAT_MATERIALS.iter().any(|m| item.ends_with(m)))
            .then(|| stats.values().find(|a| item.starts_with(&format!("{a} "))))
            .flatten();
        if let Some(affix) = own.or(material) {
            affixes.push((item.clone(), affix.clone()));
        }
    }
    out.extend(BOONS.iter().map(|b| ((*b).to_owned(), "boon")));
    out.extend(CONDITIONS.iter().map(|c| ((*c).to_owned(), "condition")));
    out.retain(|(n, _)| usable(n));
    Ok((out, affixes))
}

/// Adds the names of the official API. Returns how many are new.
pub fn fetch(con: &Connection) -> Result<usize, String> {
    let (list, affixes) = from_api()?;
    let now = now_ms();
    let tx = con.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut added = 0;
    for (name, kind) in list {
        added += tx
            .execute("INSERT OR IGNORE INTO english_names (name, kind, added) VALUES (?1, ?2, ?3)", params![name, kind, now])
            .map_err(|e| e.to_string())?;
    }
    for (text, affix) in affixes {
        added += tx
            .execute("INSERT OR IGNORE INTO affixes (text, affix, added) VALUES (?1, ?2, ?3)", params![text, affix, now])
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
    // A boon is a name and a term too ("Might"): once.
    for term in names::terms_missing(en, it) {
        if !out.contains(&term) {
            out.push(term);
        }
    }
    out
}

/// The text, if the whole of `en` is a name or a term ("Boons"): it stays as it is; a stat
/// prefix alone ("Berserker's") is the prefix as the Italian keeps it ("Berserker").
fn whole(names: &Names, en: &str) -> Option<String> {
    let en = en.trim();
    let term = names::TERMS.iter().any(|(_, forms)| forms.iter().any(|f| en.eq_ignore_ascii_case(f)));
    let bare = en.strip_suffix("'s").or_else(|| en.strip_suffix("’s")).unwrap_or(en);
    let affix = names.affix(en).filter(|a| *a == bare);
    names.whole(en).or_else(|| term.then(|| en.to_owned())).or_else(|| affix.map(str::to_owned))
}

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

/// Whether the rewrite `new` of `old` only swaps Italian for the English of `en`: no Italian word
/// `old` did not have but articles and prepositions (", conferendo Might." added at the end), and
/// something of `old` taken out for what came in (a name put next to its translation, not in its
/// place: "Provocazione dell'Holo-Dancer Taunt").
pub(crate) fn swaps_only(en: &str, old: &str, new: &str, keep: &[&str]) -> bool {
    let (en, old_words, new_words) = (plain_words(en), plain_words(old), plain_words(new));
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
            // The words of TERMS the model said are not the mechanic in that text: they stay Italian.
            let mut plain: Vec<Vec<&str>> = vec![Vec::new(); chunk.len()];
            for t in answer["texts"].as_array().map(Vec::as_slice).unwrap_or_default() {
                let Some(n) = t["n"].as_u64().map(|n| n as usize).filter(|&n| n < chunk.len()) else { continue };
                let said: Vec<&str> = t["plain"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(Value::as_str).collect();
                let all = t["it"].as_str().is_none() && t["skip"] == true;
                plain[n] = names::terms_missing(&chunk[n].en, &chunk[n].it).into_iter().filter(|term| all || said.contains(term)).collect();
                if let Some(it) = t["it"].as_str() {
                    fixed[n] = Some(it.to_owned());
                } else if all {
                    skipped[n] = true;
                }
            }
            for (((row, it), skipped), plain) in chunk.iter().zip(fixed).zip(skipped).zip(plain) {
                let wanted = |it: &str| -> Vec<&str> { keep(&names, &row.en, it).into_iter().filter(|k| !plain.contains(k)).collect() };
                // All of its terms plain: the text stays, as a skip.
                let skipped = skipped || (!plain.is_empty() && wanted(&row.it).is_empty() && it.as_deref().is_none_or(|it| it == row.it));
                let it = if skipped { None } else { it };
                // Only a text whose names are fine may be skipped: the names are no guess.
                if skipped && names.missing(&row.en, &row.it).iter().all(|name| plain.contains(name)) {
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
                        && wanted(it).is_empty()
                        && !names::term_elided(it)
                        && names::english_brought_in(&row.en, &row.it, it, &wanted(&row.it)).is_empty()
                        && markup::token_mismatch(&row.en, it).is_empty()
                        && elided_well(&names, &row.en, it)
                        && swaps_only(&row.en, &row.it, it, &keep(&names, &row.en, &row.it))
                        && row.it.contains("[pl:") == it.contains("[pl:")
                });
                match usable {
                    Some(it) => {
                        // Its plain words are not asked again while the text stays the same.
                        if !plain.is_empty() && !options.dry_run {
                            con.execute(
                                "INSERT OR REPLACE INTO names_skip (id, hash, it) VALUES (?1, ?2, ?3)",
                                params![row.id, row.hash, markup::straight_quotes(&it)],
                            )?;
                        }
                        done.push((row, it))
                    }
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
            params![row.id, row.hash, markup::straight_quotes(new), AUTHOR, now, row.it],
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
    fn a_word_that_is_not_the_mechanic_stays_italian_next_to_one_that_is() {
        let con = db();
        con.execute_batch(
            "DELETE FROM translations;
             INSERT INTO translations (id, hash, en, it, status, author, at, seq, trust) VALUES
               (9, 'i', 'Grant might to allies. Slow the spread.', 'Concedi potenza agli alleati. Rallenta la diffusione.', 'auto', 'x', 0, 9, 1),
               (10, 'j', 'Halla''s Fury', 'Furia di Halla', 'auto', 'x', 0, 10, 1);
             INSERT INTO english_names VALUES ('Fury', 'boon', 1);",
        )
        .unwrap();
        let ask = |_: &str, _: &str| -> Result<Value, String> {
            Ok(json!({"texts": [
                {"n": 0, "it": "Concedi Might agli alleati. Rallenta la diffusione.", "plain": ["Slow"]},
                {"n": 1, "it": "Furia di Halla", "plain": ["Fury"]},
            ]}))
        };
        let report = run(&con, Some(&ask), &Options { dry_run: false, max_texts: 10 }).unwrap();
        assert_eq!((report.inside, report.rewritten, report.skipped, report.failed), (2, 1, 1, 0));
        assert_eq!(it(&con, 9), "Concedi Might agli alleati. Rallenta la diffusione.");
        assert_eq!(it(&con, 10), "Furia di Halla");
        // Neither is asked again.
        let never = |_: &str, user: &str| -> Result<Value, String> { panic!("asked again: {user}") };
        let report = run(&con, Some(&never), &Options { dry_run: false, max_texts: 10 }).unwrap();
        assert_eq!(report.inside, 0);
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
