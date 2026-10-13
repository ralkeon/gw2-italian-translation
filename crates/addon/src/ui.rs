//! In-game window (ALT+SHIFT+I): status, API keys and, for contributors, the translation editor.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

use nexus::imgui::{ChildWindow, Selectable, TreeNodeFlags, Ui, Window};

use crate::{
    config, markup,
    state::SHARED,
    store::{self, Row},
    translate::{self, Job},
};

static OPEN: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// While the search box is empty the list follows what the game shows, refreshed this often.
const RECENT_REFRESH: Duration = Duration::from_secs(2);

struct State {
    deepl_key: String,
    gemini_key: String,
    query: String,
    rows: Vec<Row>,
    pending: Option<Receiver<Vec<Row>>>,
    searched_at: Option<Instant>,
    /// The text being edited and its Italian as typed.
    selected: Option<Row>,
    edit: String,
}

impl State {
    fn new() -> Self {
        let c = config::get();
        Self {
            deepl_key: c.deepl.api_key,
            gemini_key: c.gemini.api_key,
            query: String::new(),
            rows: Vec::new(),
            pending: None,
            searched_at: None,
            selected: None,
            edit: String::new(),
        }
    }

    fn search(&mut self) {
        self.pending = Some(store::search(self.query.clone()));
        self.searched_at = Some(Instant::now());
    }

    fn select(&mut self, row: Row) {
        self.edit = row.it.clone().unwrap_or_default();
        self.selected = Some(row);
    }
}

pub fn toggle() {
    OPEN.fetch_xor(true, Ordering::Relaxed);
}

pub fn render(ui: &Ui) {
    let mut open = OPEN.load(Ordering::Relaxed);
    if !open {
        return;
    }
    let Ok(mut state) = STATE.lock() else {
        return;
    };
    let state = state.get_or_insert_with(State::new);
    Window::new("Italian Translation").opened(&mut open).size([700.0, 600.0], nexus::imgui::Condition::FirstUseEver).build(ui, || {
        status(ui);
        if ui.collapsing_header("Chiavi API", TreeNodeFlags::empty()) {
            keys(ui, state);
        }
        // The editor only for contributors (contributor.json in the addon folder).
        if let Some(user) = config::contributor() {
            ui.separator();
            editor(ui, state, &user);
        }
    });
    OPEN.store(open, Ordering::Relaxed);
}

fn status(ui: &Ui) {
    if SHARED.shim_connected.load(Ordering::Relaxed) {
        ui.text_colored([0.4, 0.9, 0.4, 1.0], "Text Translator collegato");
    } else {
        ui.text_colored([0.9, 0.6, 0.3, 1.0], "In attesa di Text Translator (modulo Italian Translation avviato?)");
    }
    ui.text(format!(
        "Richieste in questa sessione: {}   Testi in archivio: {}",
        SHARED.received.load(Ordering::Relaxed),
        SHARED.stored.load(Ordering::Relaxed),
    ));
    let status = SHARED.status.lock().map(|s| s.clone()).unwrap_or_default();
    ui.text(format!("Traduzione: {status}"));
    ui.text(format!(
        "Tradotti: {}   In coda: {}   Scartati: {}",
        SHARED.translated.load(Ordering::Relaxed),
        SHARED.queued.load(Ordering::Relaxed),
        SHARED.rejected.load(Ordering::Relaxed),
    ));
    if let Some((used, limit)) = SHARED.usage.lock().ok().and_then(|u| *u) {
        ui.text(format!("DeepL questo mese: {used} / {limit} caratteri"));
    }
}

fn keys(ui: &Ui, state: &mut State) {
    ui.input_text("Chiave DeepL", &mut state.deepl_key).password(true).build();
    ui.input_text("Chiave Gemini", &mut state.gemini_key).password(true).build();
    if ui.button("Salva chiavi") {
        let mut c = config::get();
        c.deepl.api_key = state.deepl_key.trim().to_owned();
        c.gemini.api_key = state.gemini_key.trim().to_owned();
        config::set(c);
    }
}

fn editor(ui: &Ui, state: &mut State, user: &str) {
    // Results of the last search.
    if let Some(rx) = &state.pending {
        match rx.try_recv() {
            Ok(rows) => {
                state.rows = rows;
                state.pending = None;
            }
            Err(TryRecvError::Disconnected) => state.pending = None,
            Err(TryRecvError::Empty) => {}
        }
    }
    let changed = ui.input_text("Cerca (inglese, italiano o ID)", &mut state.query).build();
    let stale = state.query.trim().is_empty() && state.searched_at.is_none_or(|t| t.elapsed() >= RECENT_REFRESH);
    if (changed || stale || state.searched_at.is_none()) && state.pending.is_none() {
        state.search();
    }
    if state.query.trim().is_empty() {
        ui.text_disabled("Ultimi testi visti in gioco: clicca per correggere. [M] = corretto a mano.");
    }

    let list_height = if state.selected.is_some() { 220.0 } else { 0.0 };
    let mut clicked = None;
    ChildWindow::new("results").size([0.0, list_height]).border(true).build(ui, || {
        for row in &state.rows {
            let it = match (&row.it, row.status.as_deref()) {
                (Some(it), Some("manual")) => format!("[M] {it}"),
                (Some(it), _) => it.clone(),
                (None, _) => "(non tradotto)".into(),
            };
            let label = format!("{:>7}  {}  ->  {}##{}-{}", row.id, one_line(&row.en), one_line(&it), row.id, row.hash);
            let selected = state.selected.as_ref().is_some_and(|s| s.id == row.id && s.hash == row.hash);
            if Selectable::new(label).selected(selected).build(ui) {
                clicked = Some(row.clone());
            }
            if ui.is_item_hovered() && (row.en.len() > 60 || it.len() > 60) {
                ui.tooltip_text(format!("{}\n\n{it}", row.en));
            }
        }
    });
    if let Some(row) = clicked {
        state.select(row);
    }

    let Some(row) = state.selected.clone() else {
        return;
    };
    ui.separator();
    ui.text(format!("ID {}", row.id));
    ui.text_wrapped(&row.en);
    ui.input_text_multiline("##it", &mut state.edit, [-1.0, 90.0]).build();
    let mismatch = markup::token_mismatch(&row.en, &state.edit);
    let empty = state.edit.trim().is_empty();

    if mismatch.is_empty() && !empty {
        if ui.button("Salva") {
            let it = state.edit.clone();
            store::save_manual(row.id, row.hash, it.clone(), user);
            update_row(state, row.id, row.hash, Some(it), Some("manual"));
        }
    } else {
        ui.text_disabled("Salva");
    }
    ui.same_line();
    if ui.button("Ritraduci con DeepL") {
        store::forget(row.id, row.hash);
        translate::request(Job { id: row.id, hash: row.hash, en: row.en.clone(), force: true });
        update_row(state, row.id, row.hash, None, None);
        state.edit.clear();
    }
    ui.same_line();
    if ui.button("Chiudi") {
        state.selected = None;
    }
    if !mismatch.is_empty() {
        ui.text_colored(
            [0.95, 0.45, 0.4, 1.0],
            format!("Il markup deve restare uguale all'inglese: {}", mismatch.join("  ")),
        );
    }
}

fn update_row(state: &mut State, id: u32, hash: u64, it: Option<String>, status: Option<&str>) {
    for row in state.rows.iter_mut().chain(state.selected.as_mut()) {
        if row.id == id && row.hash == hash {
            row.it = it.clone();
            row.status = status.map(str::to_owned);
        }
    }
}

/// Line breaks shown as " / ", to keep one text per line in the list.
fn one_line(s: &str) -> String {
    s.replace('\n', " ⏎ ")
}
