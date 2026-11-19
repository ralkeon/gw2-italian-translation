//! In-game window (ALT+SHIFT+I): the addon's status. The variant is in the addon's options in Nexus
//! (options.rs).

use std::sync::atomic::{AtomicBool, Ordering};

use nexus::imgui::{Ui, Window};

use crate::state::SHARED;

static OPEN: AtomicBool = AtomicBool::new(false);

pub fn toggle() {
    OPEN.fetch_xor(true, Ordering::Relaxed);
}

pub fn render(ui: &Ui) {
    let mut open = OPEN.load(Ordering::Relaxed);
    if !open {
        return;
    }
    Window::new("Italian Translation")
        .opened(&mut open)
        .size([520.0, 180.0], nexus::imgui::Condition::FirstUseEver)
        .build(ui, || status(ui));
    OPEN.store(open, Ordering::Relaxed);
}

fn status(ui: &Ui) {
    if SHARED.shim_connected.load(Ordering::Relaxed) {
        ui.text_colored([0.4, 0.9, 0.4, 1.0], "Text Translator collegato");
        if crate::install::refreshes_paused() {
            ui.text_colored([0.95, 0.75, 0.3, 1.0], "Attenzione: Pause Refreshes è attivo");
            ui.text_wrapped(
                "Pause Refreshes di Text Translator è attivo: i testi già a schermo non si aggiornano \
                 (traduzioni nuove, tasto di Show Translated Text). Spegnilo nelle opzioni di Text Translator in Nexus.",
            );
        }
    } else {
        ui.text_colored([0.9, 0.6, 0.3, 1.0], "In attesa di Text Translator");
        ui.text_wrapped(
            "Serve l'addon Text Translator (libreria di Nexus), con il modulo Italian Translation scelto nelle \
             sue impostazioni. Se l'hai appena installato, riavvia il gioco.",
        );
    }
    ui.text(format!(
        "Richieste in questa sessione: {}   Testi visti: {}   Traduzioni: {}",
        SHARED.received.load(Ordering::Relaxed),
        SHARED.stored.load(Ordering::Relaxed),
        SHARED.translated.load(Ordering::Relaxed),
    ));
    if let Ok(sync) = SHARED.sync.lock() {
        let when = match sync.at {
            Some(at) if at.elapsed().as_secs() < 60 => ", ultima adesso".to_owned(),
            Some(at) => format!(", ultima {} min fa", at.elapsed().as_secs() / 60),
            None => String::new(),
        };
        ui.text(format!(
            "Database del progetto: {}{when}   Traduzioni ricevute: {}",
            if sync.status.is_empty() { "in attesa" } else { &sync.status },
            sync.received,
        ));
    }
}
