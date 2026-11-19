//! The addon's page in Nexus' options: the variant (hybrid or all-Italian). The translations come
//! from the project's database: there is no translator or key to set here.

use nexus::imgui::Ui;

use crate::{config, install, store};

pub fn render(ui: &Ui) {
    ui.text_disabled(concat!("Italian Translation v", env!("CARGO_PKG_VERSION")));
    if install::refreshes_paused() {
        ui.text_colored([0.95, 0.75, 0.3, 1.0], "Attenzione");
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
    ui.text_wrapped(
        "Le traduzioni arrivano dal database del progetto, aggiornato ogni giorno. Un testo non \
         ancora tradotto resta in inglese finché la sua traduzione non arriva.",
    );
}
