//! Translation queue: texts with no saved translation go to DeepL in batches, in the background:
//! with the player's key, or through the project's server (which has its own) when there is none.
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

use crate::{
    config,
    deepl::{self, Error},
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

/// Who calls DeepL.
enum Via {
    Own(String),
    Server { url: String, token: String },
}

struct Queued {
    job: Job,
    prepared: Prepared,
}

/// Texts beyond this wait for the next time the game asks for them.
const QUEUE_MAX: usize = 5000;
/// Context given to DeepL with each batch: the texts of the batch itself, up to this size.
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
    paused_until: Option<Instant>,
    /// The key DeepL refused: nothing is sent until it changes.
    refused_key: Option<String>,
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
            let (config, sync) = {
                let c = config::get();
                (c.deepl, c.sync)
            };
            if !self.queue.is_empty() {
                // Let the rest of the burst arrive: it goes in the same batch, as context.
                let until = Instant::now() + Duration::from_millis(config.batch_wait_ms);
                while let Some(left) = until.checked_duration_since(Instant::now()) {
                    match rx.recv_timeout(left) {
                        Ok(job) => self.add(job),
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
            SHARED.queued.store(self.queue.len() as u64, Ordering::Relaxed);

            let via = if config.api_key.is_empty() {
                if !sync.enabled {
                    set_status("Manca la chiave DeepL (e il DB globale è spento)");
                    continue;
                }
                // The token comes with the first sync.
                if sync.token.is_empty() {
                    set_status("In attesa del server del progetto");
                    continue;
                }
                Via::Server { url: sync.server().to_owned(), token: sync.token }
            } else {
                if self.refused_key.as_deref() == Some(config.api_key.as_str()) {
                    continue;
                }
                if self.last_usage.is_none_or(|t| t.elapsed() >= USAGE_EVERY) {
                    self.update_usage(&config.api_key);
                }
                Via::Own(config.api_key)
            };
            if self.queue.is_empty() || self.paused_until.is_some_and(|t| Instant::now() < t) {
                continue;
            }
            self.paused_until = None;
            self.translate_batch(&via, config.batch_max.clamp(1, 50));
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

    fn translate_batch(&mut self, via: &Via, max: usize) {
        let n = self.queue.len().min(max);
        let batch: Vec<Queued> = self.queue.drain(..n).collect();
        let texts: Vec<&str> = batch.iter().map(|q| q.prepared.xml.as_str()).collect();
        let mut neighbours = String::new();
        for q in &batch {
            if neighbours.len() + q.prepared.xml.len() > NEIGHBOURS_MAX {
                break;
            }
            neighbours.push_str(&q.prepared.xml);
            neighbours.push('\n');
        }

        let (result, who) = match via {
            Via::Own(key) => {
                set_status(&format!("Traduco {n} testi..."));
                (deepl::translate(key, &texts, &neighbours), "DeepL")
            }
            Via::Server { url, token } => {
                set_status(&format!("Traduco {n} testi (server del progetto)..."));
                (deepl::translate_on_server(url, token, &texts, &neighbours), "Server")
            }
        };
        match result {
            Ok(translations) => {
                let mut rejected = 0;
                for (q, xml) in batch.iter().zip(&translations) {
                    let Job { id, hash, en, .. } = &q.job;
                    match q.prepared.restore(xml) {
                        Some(it) => {
                            let it = plural::pluralize(en, &markup::fix_spacing(en, &it));
                            pipe::send_text(*id, *hash, &it);
                            store::save_translation(Translated { id: *id, hash: *hash, it, source: "deepl" });
                            self.known.remove(&(*id, *hash));
                        }
                        None => {
                            // Kept in `known`: not retried in this session.
                            log::warn!("translation of {id} rejected (markup changed): {en:?} -> {xml:?}");
                            rejected += 1;
                        }
                    }
                }
                SHARED.rejected.fetch_add(rejected, Ordering::Relaxed);
                set_status("Pronto");
            }
            Err(Error::BadRequest(e)) => {
                // Kept in `known`: not retried in this session.
                log::warn!("DeepL refused {n} texts: {e}");
                for q in &batch {
                    log::warn!("  {}: {:?}", q.job.id, q.prepared.xml);
                }
                SHARED.rejected.fetch_add(n as u64, Ordering::Relaxed);
                set_status(&format!("{who}: richiesta rifiutata ({n} testi scartati)"));
            }
            Err(e) => {
                let pause = match (&e, via) {
                    (Error::QuotaExceeded, _) => Duration::from_secs(3600),
                    (Error::TooManyRequests, _) => Duration::from_secs(10),
                    (Error::Forbidden, Via::Own(key)) => {
                        self.refused_key = Some(key.clone());
                        Duration::ZERO
                    }
                    // Our sync token: the sync thread gets a new one.
                    (Error::Forbidden, Via::Server { .. }) => Duration::from_secs(60),
                    (Error::Other(_) | Error::BadRequest(_), _) => Duration::from_secs(30),
                };
                log::warn!("{who}: {e}");
                match (&e, via) {
                    (Error::QuotaExceeded, Via::Server { .. }) => {
                        set_status("Server: limite di traduzioni raggiunto, riprovo tra un'ora")
                    }
                    _ => set_status(&format!("{who}: {e}")),
                }
                self.paused_until = Some(Instant::now() + pause);
                // Back at the front, in the same order.
                for q in batch.into_iter().rev() {
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
            Err(Error::Forbidden) => {
                self.refused_key = Some(key.to_owned());
                set_status("DeepL: chiave API non valida");
            }
            Err(e) => log::warn!("DeepL usage: {e}"),
        }
    }
}

fn set_status(s: &str) {
    if let Ok(mut status) = SHARED.status.lock() {
        if *status != s {
            *status = s.to_owned();
        }
    }
}
