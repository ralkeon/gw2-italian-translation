//! Translators of Italian Translation and the fallback between them, shared by the addon and the
//! server: DeepL first, then Gemini, Azure Translator and other language models (any service with
//! OpenAI's API) while the ones before are out of quota or do not answer, and in the addon last
//! the project's server, for players without keys (or whose keys are used up).
//!
//! Every translator takes XML texts with `<x id="N"/>` placeholders (markup.rs in the addon) and
//! gives them back in Italian, in the same order.

pub mod azure;
pub mod deepl;
pub mod gemini;
pub mod openai;
pub mod server;

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use gw2it_text::glossary::{self, Term};
use serde_json::Value;

/// Fixed context for every request: steers the translation, not translated nor billed.
pub const CONTEXT: &str = "Guild Wars 2, a fantasy MMORPG: in-game texts such as item, skill and \
    title names, tooltips, achievements, dialogue and user interface labels.";

/// For the language models (gemini.rs, openai.rs); each adds the JSON shape of its answer.
pub(crate) const LLM_INSTRUCTIONS: &str = "You translate the texts of the game Guild Wars 2, a \
fantasy MMORPG, from English to Italian: item, skill and title names, tooltips, achievements, \
dialogue, interface labels. You get a JSON array of texts shown together on screen: use them as \
context for each other. Give one Italian translation per text, in the same order.

Rules:
- The texts are XML fragments. Keep every <x id=\"N\"/> tag exactly as it is, once, where it \
belongs in the Italian sentence: they stand for numbers, names, colours and line breaks. Keep \
&amp; &lt; &gt; escaped.
- Natural Italian, as in a video game: informal (tu) when talking to the player, short and \
clear for interface labels, buttons and item names.
- Names of characters, places, races and factions (Tyria, Lion's Arch, Inquest, Asura...) stay \
in English.
- The words of the game's mechanics stay in English, as Italian players say them: the names of \
skills, traits, specializations, professions and mounts; the skill types and labels (Glamour, \
Signet, Shatter, Mantra, Trap...); the boons and conditions by name, capitalised, also where the \
English writes them in lowercase (\"grant might\" is \"conferisci Might\", \"inflict bleeding\" is \
\"infliggi Bleeding\"; Fury, Quickness, Swiftness, Aegis, Burning, Vulnerability, Weakness, \
Stealth, Distortion...); \
Stun, Daze, Knockback, Knockdown, Fear, Taunt as names and in lists; the attributes (Power, \
Precision, Toughness, Vitality, Ferocity, Healing Power, Condition Damage...); \"boon\", \
\"condition\", \"rune\", \"sigil\", \"relic\", \"focus\" (the weapon) as loanwords, never elided \
(\"il boon\", \"della condition\"). The verbs stay Italian (\"chill\" assiderare, \"cripple\" \
azzoppare, \"stun\" stordire, \"daze\" disorientare). A stat prefix of an item (Berserker's, Carrion, \
also the minor ones: Malign, Vital, Healing, Strong) stays English and goes after the Italian noun, without \"'s\" (\"Spadone Berserker\").
- The other words of item names are translated, also in the miniatures, which keep \"Mini\" first \
(\"Mini Siamese Cat\" is \"Mini Gatto siamese\", \"Mini Raven\" is \"Mini Corvo\"). The adjectives \
of a crafting component describe the piece, not its material (\"Oiled Orichalcum Hammer Head\" is \
\"Testa di martello in oricalco oliata\").
- Translate the same term the same way across the texts.
";

/// For the language models: the terms of the glossary that are in `texts`, to add to the
/// instructions. Empty if there are none. The machine translators (DeepL, Azure) cannot be
/// told: what they get wrong is repaired in their answer (gw2it_text::glossary::fix).
pub(crate) fn glossary_note(terms: &[Term], texts: &[&str]) -> String {
    let found = glossary::relevant(terms, texts);
    if found.is_empty() {
        return String::new();
    }
    let list: Vec<String> = found.iter().map(|t| format!("{} = {}", t.en, t.it)).collect();
    format!(
        "\nGlossary, chosen by the translators of the game: translate these English words this way, in \
         whatever form the sentence needs: {}.\n",
        list.join("; ")
    )
}

#[derive(Debug)]
pub enum Error {
    /// 456: monthly character quota used up (DeepL), or today's requests (Gemini).
    QuotaExceeded,
    /// 429: too many requests, retry later.
    TooManyRequests,
    /// 403: wrong key.
    Forbidden,
    /// 400: these texts were refused; retrying them with the same translator would fail again.
    BadRequest(String),
    /// Anything else: network, 5xx, unexpected answer.
    Other(String),
    /// Every translator is paused after errors: nothing was sent.
    Paused,
    /// The translator is not a language model: it cannot be asked questions.
    Unsupported,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QuotaExceeded => f.write_str("quota esaurita"),
            Self::TooManyRequests => f.write_str("troppe richieste, riprovo tra poco"),
            Self::Forbidden => f.write_str("chiave API non valida"),
            Self::BadRequest(e) => write!(f, "richiesta rifiutata: {e}"),
            Self::Other(e) => f.write_str(e),
            Self::Paused => f.write_str("in pausa dopo un errore"),
            Self::Unsupported => f.write_str("non è un modello linguistico"),
        }
    }
}

/// DeepL's status codes, which the server passes on as they are.
pub(crate) fn check(response: &mut ureq::http::Response<ureq::Body>) -> Result<(), Error> {
    let status = response.status().as_u16();
    if status == 200 {
        return Ok(());
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["message"].as_str().or(v["error"].as_str()).map(str::to_owned))
        .unwrap_or(body);
    Err(match status {
        456 => Error::QuotaExceeded,
        429 => Error::TooManyRequests,
        401 | 403 => Error::Forbidden,
        400 => Error::BadRequest(message),
        s => Error::Other(format!("HTTP {s} {message}")),
    })
}

#[derive(Clone, PartialEq, Eq)]
pub enum Engine {
    Deepl { key: String },
    Azure { key: String, region: String },
    Gemini { key: String, model: String },
    /// A service with OpenAI's API (openai.rs).
    OpenAi { name: String, url: String, key: String, model: String },
    /// The project's server, with the installation's sync token.
    Server { url: String, token: String },
}

impl Engine {
    /// As saved with the translation: "deepl", "gemini", "groq"...
    pub fn name(&self) -> String {
        match self {
            Self::Deepl { .. } => "deepl".into(),
            Self::Azure { .. } => "azure".into(),
            Self::Gemini { .. } => "gemini".into(),
            Self::OpenAi { name, .. } => name.to_lowercase(),
            Self::Server { .. } => "server".into(),
        }
    }

    /// One entry of the server's translators.json:
    /// {"type": "deepl" | "azure" | "gemini" | "openai", "key", "region", "model", "name", "url"}.
    /// Gemini's "model" may list several, separated by commas: one engine each.
    pub fn from_json(v: &Value) -> Result<Vec<Self>, String> {
        let field = |name: &str| v[name].as_str().map(str::trim).unwrap_or("").to_owned();
        let key = field("key");
        if key.is_empty() {
            return Err(format!("no key in {v}"));
        }
        Ok(match field("type").as_str() {
            "deepl" => vec![Self::Deepl { key }],
            "azure" => vec![Self::Azure { key, region: field("region") }],
            "gemini" => field("model")
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(|model| Self::Gemini { key: key.clone(), model: model.to_owned() })
                .collect(),
            "openai" if !field("url").is_empty() && !field("model").is_empty() => {
                vec![Self::OpenAi { name: field("name"), url: field("url"), key, model: field("model") }]
            }
            _ => return Err(format!("unknown or incomplete translator {v}")),
        })
    }

    /// How long to leave it alone after an error.
    fn pause(&self, e: &Error) -> Duration {
        match (e, self) {
            (Error::QuotaExceeded, _) => Duration::from_secs(3600),
            (Error::TooManyRequests, Self::Gemini { .. } | Self::OpenAi { .. }) => Duration::from_secs(60),
            (Error::TooManyRequests, _) => Duration::from_secs(10),
            // The server's 401: the addon's sync gets a new token.
            (Error::Forbidden, Self::Server { .. }) => Duration::from_secs(60),
            // Until the key changes, which makes it another engine.
            (Error::Forbidden, _) => Duration::from_secs(24 * 3600),
            (Error::BadRequest(_) | Error::Paused | Error::Unsupported, _) => Duration::ZERO,
            (Error::Other(_), _) => Duration::from_secs(60),
        }
    }

    /// The texts, who made them and the server's signatures (only from the server).
    fn translate(
        &self,
        texts: &[&str],
        neighbours: &str,
        terms: &[Term],
    ) -> Result<(Vec<String>, String, Vec<String>), Error> {
        match self {
            Self::Deepl { key } => deepl::translate(key, texts, neighbours).map(|t| (t, "deepl".into(), Vec::new())),
            Self::Azure { key, region } => azure::translate(key, region, texts).map(|t| (t, "azure".into(), Vec::new())),
            Self::Gemini { key, model } => gemini::translate(key, model, texts, &glossary_note(terms, texts)).map(|t| (t, "gemini".into(), Vec::new())),
            Self::OpenAi { url, key, model, .. } => {
                openai::translate(url, key, model, texts, &glossary_note(terms, texts)).map(|t| (t, self.name(), Vec::new()))
            }
            Self::Server { url, token } => {
                server::translate(url, token, texts, neighbours).map(|(t, by, s)| (t, format!("server:{by}"), s))
            }
        }
    }
}

impl Engine {
    /// A question for the language models (Gemini, other services with OpenAI's API) with a JSON
    /// answer. The machine translators cannot answer it: Unsupported.
    fn ask(&self, system: &str, user: &str) -> Result<Value, Error> {
        match self {
            Self::Gemini { key, model } => gemini::ask(key, model, system, user),
            Self::OpenAi { url, key, model, .. } => openai::ask(url, key, model, system, user),
            Self::Deepl { .. } | Self::Azure { .. } | Self::Server { .. } => Err(Error::Unsupported),
        }
    }
}

struct Slot {
    engine: Engine,
    paused_until: Mutex<Option<Instant>>,
}

/// Translators in order of preference. Each one that fails is paused for a while (an hour when
/// out of quota) and the next one takes over; when its pause ends, it is tried first again.
/// Usable from several threads at once (the server): no lock is held during a request.
#[derive(Default)]
pub struct Chain {
    slots: Vec<Slot>,
}

pub struct Translated {
    pub texts: Vec<String>,
    /// "deepl", "gemini", "server:deepl"...
    pub by: String,
    /// The server's signature of each text, when it made them; else empty.
    pub signatures: Vec<String>,
    /// The position of the translator in the chain, to ask the next ones when a text is no good.
    pub slot: usize,
}

impl Chain {
    /// Sets the translators, keeping the pauses of those that did not change.
    pub fn set(&mut self, engines: Vec<Engine>) {
        let mut old = std::mem::take(&mut self.slots);
        self.slots = engines
            .into_iter()
            .map(|engine| match old.iter().position(|s| s.engine == engine) {
                Some(i) => old.swap_remove(i),
                None => Slot { engine, paused_until: Mutex::new(None) },
            })
            .collect();
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// True if a translator can be tried now.
    pub fn ready(&self) -> bool {
        let now = Instant::now();
        self.slots.iter().any(|s| s.paused_until.lock().unwrap().is_none_or(|t| now >= t))
    }

    /// The first translator that is not paused and answers. A translator that refuses the texts
    /// (BadRequest) is not paused, but the next one gets them. The error is the last one met.
    pub fn translate(&self, texts: &[&str], neighbours: &str, terms: &[Term]) -> Result<Translated, Error> {
        self.translate_without(&[], texts, neighbours, terms)
    }

    /// As `translate`, but not by the translators at the positions in `skip` (the ones that already
    /// answered and whose answer was no good: markup the game would show broken).
    pub fn translate_without(&self, skip: &[usize], texts: &[&str], neighbours: &str, terms: &[Term]) -> Result<Translated, Error> {
        let mut last = Error::Paused;
        for (position, slot) in self.slots.iter().enumerate() {
            if skip.contains(&position) {
                continue;
            }
            let now = Instant::now();
            if slot.paused_until.lock().unwrap().is_some_and(|t| now < t) {
                continue;
            }
            match slot.engine.translate(texts, neighbours, terms) {
                Ok((texts, by, signatures)) => {
                    *slot.paused_until.lock().unwrap() = None;
                    return Ok(Translated { texts, by, signatures, slot: position });
                }
                Err(e) => {
                    let pause = slot.engine.pause(&e);
                    log::warn!("{}: {e}; paused for {}s", slot.engine.name(), pause.as_secs());
                    if !pause.is_zero() {
                        *slot.paused_until.lock().unwrap() = Some(Instant::now() + pause);
                    }
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// Asks the language models in order (same pauses as `translate`) and gives the first JSON
    /// answer. Paused, out of quota or refused: the next one. Err(Unsupported) if there is none.
    pub fn ask(&self, system: &str, user: &str) -> Result<Value, Error> {
        let mut last = Error::Unsupported;
        for slot in &self.slots {
            if matches!(slot.engine, Engine::Deepl { .. } | Engine::Azure { .. } | Engine::Server { .. }) {
                continue;
            }
            let now = Instant::now();
            if slot.paused_until.lock().unwrap().is_some_and(|t| now < t) {
                last = Error::Paused;
                continue;
            }
            match slot.engine.ask(system, user) {
                Ok(answer) => {
                    *slot.paused_until.lock().unwrap() = None;
                    return Ok(answer);
                }
                Err(e) => {
                    let pause = slot.engine.pause(&e);
                    log::warn!("{}: {e}; paused for {}s", slot.engine.name(), pause.as_secs());
                    if !pause.is_zero() {
                        *slot.paused_until.lock().unwrap() = Some(Instant::now() + pause);
                    }
                    last = e;
                }
            }
        }
        Err(last)
    }

    /// As `ask`, for the jobs that run alone (cron): when the models are only busy (HTTP 503, too
    /// many requests, network) it waits and asks again, longer each time (1, 2, 5, 10, then 15
    /// minutes), until they answer or `patience` of waiting in a row is spent. Out of quota, a
    /// wrong key or a refusal end it at once, as does a wait longer than what patience is left.
    pub fn ask_patiently(&self, system: &str, user: &str, patience: Duration) -> Result<Value, Error> {
        const WAITS: [u64; 5] = [60, 120, 300, 600, 900];
        let mut waited = Duration::ZERO;
        let mut tries = 0;
        loop {
            let e = match self.ask(system, user) {
                Ok(answer) => return Ok(answer),
                Err(e @ (Error::QuotaExceeded | Error::Forbidden | Error::BadRequest(_) | Error::Unsupported)) => return Err(e),
                Err(e) => e,
            };
            // Not before a model is out of its pause; a pause of half an hour or more is a quota used up.
            let ready = self.ready_in();
            let wait = ready.max(Duration::from_secs(WAITS[tries.min(WAITS.len() - 1)]));
            if ready >= Duration::from_secs(1800) || waited + wait > patience {
                return Err(e);
            }
            // These jobs write to their log file: no logger there.
            eprintln!("{e}; asking again in {}s", wait.as_secs());
            std::thread::sleep(wait);
            waited += wait;
            tries += 1;
        }
    }

    /// How long until a language model can be tried: zero if one can now.
    fn ready_in(&self) -> Duration {
        let now = Instant::now();
        self.slots
            .iter()
            .filter(|s| !matches!(s.engine, Engine::Deepl { .. } | Engine::Azure { .. } | Engine::Server { .. }))
            .map(|s| s.paused_until.lock().unwrap().map_or(Duration::ZERO, |t| t.saturating_duration_since(now)))
            .min()
            .unwrap_or(Duration::ZERO)
    }
}
