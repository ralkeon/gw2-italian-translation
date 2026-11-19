//! The addon's page in Nexus' options: the variant (hybrid or all-Italian) and the status (Text
//! Translator, the project's server). There is no window of its own.

use std::sync::atomic::Ordering;

use nexus::imgui::Ui;

use crate::{config, install, state::SHARED, store};

const GREEN: [f32; 4] = [0.4, 0.9, 0.4, 1.0];
const ORANGE: [f32; 4] = [0.95, 0.75, 0.3, 1.0];

pub fn render(ui: &Ui) {
    ui.text_disabled(concat!("Italian Translation v", env!("CARGO_PKG_VERSION")));
    if install::refreshes_paused() {
        ui.text_colored(ORANGE, "Attenzione");
        ui.text_wrapped(
            "Pause Refreshes di Text Translator è attivo: i testi già a schermo non si aggiornano \
             (traduzioni nuove, tasto di Show Translated Text). Spegnilo nelle opzioni di Text Translator in Nexus.",
        );
    }

    // Applied at once.
    let mut full = config::get().full_italian;
    if ui.checkbox("Tutto in italiano", &mut full) {
        let mut saved = config::get();
        saved.full_italian = full;
        config::set(saved);
        store::set_full_mode(full);
    }
    ui.text_wrapped(
        "Spenta (traduzione ibrida): i nomi di abilità, tratti, specializzazioni, luoghi, boon e \
         condition restano in inglese, come li dicono i giocatori italiani. Accesa: anche quelli \
         in italiano (Berserker Fantasma, Arco del Leone, Forza), dove la traduzione completa è \
         già pronta; il resto resta com'è.",
    );
    ui.text_disabled(format!("Testi tutti in italiano già pronti: {}", store::full_count()));
    ui.separator();
    status(ui);
}

fn status(ui: &Ui) {
    if SHARED.shim_connected.load(Ordering::Relaxed) {
        ui.text_colored(GREEN, "Text Translator: collegato");
    } else {
        ui.text_colored(ORANGE, "Text Translator: in attesa");
        ui.text_wrapped(
            "Serve l'addon Text Translator (libreria di Nexus), con il modulo Italian Translation scelto nelle \
             sue impostazioni. Se l'hai appena installato, riavvia il gioco.",
        );
    }
    let Ok(sync) = SHARED.sync.lock() else { return };
    let when = match sync.at {
        Some(at) if at.elapsed().as_secs() < 60 => ", ultima sincronizzazione adesso".to_owned(),
        Some(at) => format!(", ultima sincronizzazione {} min fa", at.elapsed().as_secs() / 60),
        None => String::new(),
    };
    let text = format!("Server: {}{when}", if sync.status.is_empty() { "in attesa" } else { &sync.status });
    if sync.status == "ok" {
        ui.text_colored(GREEN, text);
    } else {
        ui.text_wrapped(text);
    }
}
