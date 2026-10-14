//! The addon's page in Nexus' options: the translators' keys, in the order they are used.

use std::sync::Mutex;

use gw2it_translate::openai::PRESETS;
use nexus::imgui::Ui;

use crate::config::{self, Config, Llm};

/// The fields as typed, until saved.
static EDIT: Mutex<Option<Config>> = Mutex::new(None);

pub fn render(ui: &Ui) {
    let Ok(mut edit) = EDIT.lock() else {
        return;
    };
    let c = edit.get_or_insert_with(config::get);

    ui.text_wrapped(
        "Traduttori, nell'ordine in cui vengono usati: quando uno finisce la quota o non risponde \
         si passa al successivo, e si torna al primo appena è di nuovo disponibile. Tutte le chiavi \
         sono facoltative: senza, o quando sono esaurite, traduce il server del progetto.",
    );
    ui.separator();

    ui.text("DeepL (piano API Free: 500.000 caratteri al mese)");
    ui.input_text("Chiave##deepl", &mut c.deepl.api_key).password(true).build();

    ui.text("Google Gemini (aistudio.google.com)");
    ui.input_text("Chiave##gemini", &mut c.gemini.api_key).password(true).build();
    ui.input_text("Modelli, separati da virgole##gemini", &mut c.gemini.model).build();

    ui.text("Microsoft Azure Translator (piano F0: 2 milioni di caratteri al mese)");
    ui.input_text("Chiave##azure", &mut c.azure.api_key).password(true).build();
    ui.input_text("Regione (es. italynorth)##azure", &mut c.azure.region).build();

    ui.text("Altri modelli linguistici (API compatibile con OpenAI)");
    let mut remove = None;
    for (i, llm) in c.llm.iter_mut().enumerate() {
        ui.input_text(format!("Nome##llm-name-{i}"), &mut llm.name).build();
        ui.input_text(format!("Indirizzo API##llm-url-{i}"), &mut llm.url).build();
        ui.input_text(format!("Chiave##llm-key-{i}"), &mut llm.api_key).password(true).build();
        ui.input_text(format!("Modello##llm-model-{i}"), &mut llm.model).build();
        if ui.button(format!("Rimuovi {}##llm-remove-{i}", llm.name)) {
            remove = Some(i);
        }
        ui.separator();
    }
    if let Some(i) = remove {
        c.llm.remove(i);
    }
    ui.text_disabled("Aggiungi:");
    for (name, url, model) in PRESETS {
        ui.same_line();
        if ui.button(format!("{name}##add")) {
            c.llm.push(Llm { name: (*name).into(), url: (*url).into(), api_key: String::new(), model: (*model).into() });
        }
    }
    ui.same_line();
    if ui.button("Altro##add") {
        c.llm.push(Llm::default());
    }

    ui.separator();
    if ui.button("Salva") {
        // Only the keys come from here: the rest of the config may have changed meanwhile
        // (the sync token, the install flag).
        let mut saved = config::get();
        saved.deepl.api_key = c.deepl.api_key.trim().to_owned();
        saved.azure = c.azure.clone();
        saved.gemini = c.gemini.clone();
        saved.gemini.api_key = saved.gemini.api_key.trim().to_owned();
        saved.llm = c.llm.clone();
        config::set(saved);
        *edit = None;
        return;
    }
    ui.same_line();
    if ui.button("Annulla") {
        *edit = None;
    }
}
