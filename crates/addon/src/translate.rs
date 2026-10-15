//! Translation queue: texts with no saved translation are translated in batches, in the
//! background, by the first translator available (gw2it-translate): the player's own (DeepL,
//! Gemini, Azure, other language models, each while the ones before are out of quota or do not
//! answer), and last the project's server, which does the same with its own keys.
//!
//! The game never waits: it shows the English until the translation is ready, then it is pushed
//! into the game and saved, so it is never asked for again.

use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use gw2it_text::glossary;
use gw2it_translate::{Chain, Engine, Error, deepl};

use crate::{
    config,
    markup::{self, Prepared},
    pipe, plural,
    state::SHARED,
    store::{self, Translated},
};

pub struct Job {
    pub id: u32,
    pub hash: u64,
    pub en: String,
    /// Translate again even if it was tried (and rejected) in this session.
    pub force: bool,
}

struct Queued {
    job: Job,
    prepared: Prepared,
}

/// Texts beyond this wait for the next time the game asks for them.
const QUEUE_MAX: usize = 5000;
/// Context given to the translator with each batch: the texts of the batch itself, up to this size.
const NEIGHBOURS_MAX: usize = 3000;
const USAGE_EVERY: Duration = Duration::from_secs(300);

static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<(Sender<Job>, JoinHandle<()>)>> = Mutex::new(None);

/// `backlog`: texts seen in earlier sessions and never translated.
pub fn start(backlog: Vec<Job>) {
    STOP.store(false, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    for job in backlog {
        let _ = tx.send(job);
    }
    let handle = thread::Builder::new()
        .name("gw2it-translate".into())
        .spawn(move || Worker::default().run(&rx))
        .expect("spawn translate thread");
    *WORKER.lock().unwrap() = Some((tx, handle));
}

/// Asks for a translation of a text that has none yet.
pub fn request(job: Job) {
    if let Ok(worker) = WORKER.lock() {
        if let Some((tx, _)) = worker.as_ref() {
            let _ = tx.send(job);
        }
    }
}

pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    let worker = WORKER.lock().ok().and_then(|mut w| w.take());
    if let Some((tx, handle)) = worker {
        drop(tx);
        let _ = handle.join(); // at worst a request in flight: bounded by the HTTP timeout
    }
}

#[derive(Default)]
struct Worker {
    queue: VecDeque<Queued>,
    /// Queued, in flight, or rejected in this session: not queued again.
    known: HashSet<(u32, u64)>,
    chain: Chain,
    last_usage: Option<Instant>,
}

impl Worker {
    fn run(mut self, rx: &Receiver<Job>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(job) => self.add(job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if STOP.load(Ordering::SeqCst) {
                return;
            }
            let config = config::get();
            if !self.queue.is_empty() {
                // Let the rest of the burst arrive: it goes in the same batch, as context.
                let until = Instant::now() + Duration::from_millis(config.deepl.batch_wait_ms);
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    match rx.recv_timeout(left) {
                        Ok(job) => self.add(job),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
            SHARED.queued.store(self.queue.len() as u64, Ordering::Relaxed);

            self.chain.set(engines(&config));
            if self.chain.is_empty() {
                set_status(if !config.sync.enabled {
                    "Nessuna chiave API e DB globale spento: niente traduzioni"
                } else {
                    // The token comes with the first sync.
                    "In attesa del server del progetto"
                });
                continue;
            }
            let key = &config.deepl.api_key;
            if !key.is_empty() && self.last_usage.is_none_or(|t| t.elapsed() >= USAGE_EVERY) {
                self.update_usage(key);
            }
            if self.queue.is_empty() || !self.chain.ready() {
                continue;
            }
            self.translate_batch(config.deepl.batch_max.clamp(1, 50));
        }
    }

    fn add(&mut self, job: Job) {
        let key = (job.id, job.hash);
        if job.force {
            self.known.remove(&key);
        }
        if self.known.contains(&key) || store::lookup(job.id, job.hash).is_some() {
            return;
        }
        // A name that stays in English (a skill, a trait...): the name, without asking for one.
        if let Some(name) = store::english_name(&job.en) {
            copy(&job, name, "names".into(), None);
            return;
        }
        // The same English under another ID: its translation, without asking for one.
        if !job.force
            && let Some(it) = store::lookup_same(job.hash)
        {
            copy(&job, it, "copy".into(), None);
            return;
        }
        self.known.insert(key);
        let Some(prepared) = markup::prepare(&job.en) else {
            return; // nothing to translate: the game shows it as it is
        };
        if self.queue.len() >= QUEUE_MAX {
            self.known.remove(&key);
            return;
        }
        self.queue.push_back(Queued { job, prepared });
    }

    fn translate_batch(&mut self, max: usize) {
        let n = self.queue.len().min(max);
        let drained: Vec<Queued> = self.queue.drain(..n).collect();
        // The same English twice in the batch is translated once (`same`: the other one, and the
        // index of the first in `batch`); one translated since it was queued is copied.
        let (mut batch, mut same) = (Vec::with_capacity(n), Vec::new());
        for q in drained {
            if !q.job.force
                && let Some(it) = store::lookup_same(q.job.hash)
            {
                self.known.remove(&(q.job.id, q.job.hash));
                copy(&q.job, it, "copy".into(), None);
            } else if let Some(i) = batch.iter().position(|b: &Queued| b.job.hash == q.job.hash) {
                same.push((q, i));
            } else {
                batch.push(q);
            }
        }
        if batch.is_empty() {
            SHARED.queued.store(self.queue.len() as u64, Ordering::Relaxed);
            return;
        }
        let n = batch.len();
        let texts: Vec<&str> = batch.iter().map(|q| q.prepared.xml.as_str()).collect();
        let mut neighbours = String::new();
        for q in &batch {
            if neighbours.len() + q.prepared.xml.len() > NEIGHBOURS_MAX {
                break;
            }
            neighbours.push_str(&q.prepared.xml);
            neighbours.push('\n');
        }

        set_status(&format!("Traduco {n} testi..."));
        let terms = store::glossary();
        match self.chain.translate(&texts, &neighbours, &terms) {
            Ok(translated) => {
                let mut rejected = 0;
                for (i, (q, xml)) in batch.iter().zip(&translated.texts).enumerate() {
                    let Job { id, hash, en, .. } = &q.job;
                    match q.prepared.restore(xml) {
                        Some(it) => {
                            // Before the plural marks: they are made from the corrected text.
                            let it = glossary::fix(en, &markup::fix_spacing(en, &it), &terms);
                            let it = plural::pluralize(en, &it);
                            pipe::send_text(*id, *hash, &it);
                            let proof = translated.signatures.get(i).map(|s| (s.clone(), xml.clone()));
                            for (other, _) in same.iter().filter(|(_, first)| *first == i) {
                                self.known.remove(&(other.job.id, other.job.hash));
                                copy(&other.job, it.clone(), translated.by.clone(), proof.clone());
                            }
                            store::save_translation(Translated { id: *id, hash: *hash, it, source: translated.by.clone(), proof });
                            self.known.remove(&(*id, *hash));
                        }
                        None => {
                            // Kept in `known`: not retried in this session.
                            log::warn!("translation of {id} by {} rejected (markup changed): {en:?} -> {xml:?}", translated.by);
                            rejected += 1 + same.iter().filter(|(_, first)| *first == i).count() as u64;
                        }
                    }
                }
                SHARED.rejected.fetch_add(rejected, Ordering::Relaxed);
                set_status(&format!("Pronto (ultima traduzione: {})", translated.by));
            }
            Err(Error::BadRequest(e)) => {
                // Refused by every translator. Kept in `known`: not retried in this session.
                log::warn!("{n} texts refused: {e}");
                for q in &batch {
                    log::warn!("  {}: {:?}", q.job.id, q.prepared.xml);
                }
                SHARED.rejected.fetch_add((n + same.len()) as u64, Ordering::Relaxed);
                set_status(&format!("Richiesta rifiutata ({n} testi scartati): {e}"));
            }
            Err(e) => {
                // Every translator failed or is paused: they are tried again when a pause ends.
                set_status(&format!("Traduttori non disponibili: {e}"));
                // Back at the front, in the same order.
                for q in batch.into_iter().chain(same.into_iter().map(|(q, _)| q)).rev() {
                    self.queue.push_front(q);
                }
            }
        }
        SHARED.queued.store(self.queue.len() as u64, Ordering::Relaxed);
    }

    fn update_usage(&mut self, key: &str) {
        self.last_usage = Some(Instant::now());
        match deepl::usage(key) {
            Ok(u) => {
                if let Ok(mut usage) = SHARED.usage.lock() {
                    *usage = Some((u.used, u.limit));
                }
            }
            Err(e) => log::warn!("DeepL usage: {e}"),
        }
    }
}

/// The translators in order: the player's own keys, then the project's server.
fn engines(config: &config::Config) -> Vec<Engine> {
    let mut engines = Vec::new();
    if !config.deepl.api_key.is_empty() {
        engines.push(Engine::Deepl { key: config.deepl.api_key.clone() });
    }
    if !config.gemini.api_key.is_empty() {
        for model in config.gemini.model.split(',').map(str::trim).filter(|m| !m.is_empty()) {
            engines.push(Engine::Gemini { key: config.gemini.api_key.clone(), model: model.to_owned() });
        }
    }
    // After Gemini: a wide quota, but plainer Italian.
    let azure = &config.azure;
    if !azure.api_key.is_empty() {
        engines.push(Engine::Azure { key: azure.api_key.clone(), region: azure.region.trim().to_owned() });
    }
    for llm in &config.llm {
        if !llm.api_key.is_empty() && !llm.url.trim().is_empty() && !llm.model.trim().is_empty() {
            engines.push(Engine::OpenAi {
                name: llm.name.trim().to_owned(),
                url: llm.url.trim().to_owned(),
                key: llm.api_key.clone(),
                model: llm.model.trim().to_owned(),
            });
        }
    }
    let sync = &config.sync;
    if sync.enabled && !sync.token.is_empty() {
        engines.push(Engine::Server { url: sync.server().to_owned(), token: sync.token.clone() });
    }
    engines
}

/// Gives `job` a translation made for the same English.
fn copy(job: &Job, it: String, source: String, proof: Option<(String, String)>) {
    pipe::send_text(job.id, job.hash, &it);
    store::save_translation(Translated { id: job.id, hash: job.hash, it, source, proof });
}

fn set_status(s: &str) {
    if let Ok(mut status) = SHARED.status.lock() {
        if *status != s {
            *status = s.to_owned();
        }
    }
}
