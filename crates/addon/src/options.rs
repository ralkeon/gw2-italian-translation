//! The addon's page in Nexus' options: the variant (hybrid or all-Italian) and the status (Text
//! Translator, the project's server). There is no window of its own.

use std::sync::atomic::Ordering;

use nexus::imgui::{StyleColor, Ui};

use crate::{config, install, state::SHARED, store};

const GREEN: [f32; 4] = [0.4, 0.9, 0.4, 1.0];
const ORANGE: [f32; 4] = [0.95, 0.75, 0.3, 1.0];
/// Ko-fi's button colour (#8B40B9) and the page for tips.
const KOFI: [f32; 4] = [0.545, 0.251, 0.725, 1.0];
const KOFI_HOVER: [f32; 4] = [0.62, 0.33, 0.8, 1.0];
const KOFI_URL: &str = "https://ko-fi.com/P6V52AYPG7";

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
    let before = full;
    ui.radio_button("Traduzione completa", &mut full, true);
    ui.same_line();
    ui.radio_button("Traduzione ibrida", &mut full, false);
    if full != before {
        let mut saved = config::get();
        saved.full_italian = full;
        config::set(saved);
        store::set_full_mode(full);
    }
    ui.text_wrapped(
        "Completa: anche i nomi di abilità, tratti, specializzazioni, luoghi, boon e condition in \
         italiano (Berserker Fantasma, Arco del Leone, Forza), dove sono già pronti. Ibrida: quei \
         nomi restano in inglese, come li dicono i giocatori italiani.",
    );
    ui.text_disabled(format!("Testi tutti in italiano già pronti: {}", store::full_count()));
    ui.separator();
    status(ui);
    ui.separator();
    tips(ui);
}

/// A button to the project's Ko-fi page, opened in the default browser.
fn tips(ui: &Ui) {
    let clicked = {
        let _button = ui.push_style_color(StyleColor::Button, KOFI);
        let _hovered = ui.push_style_color(StyleColor::ButtonHovered, KOFI_HOVER);
        let _active = ui.push_style_color(StyleColor::ButtonActive, KOFI);
        ui.button("Supportami su Ko-fi")
    };
    ui.same_line();
    ui.text_disabled("Se l'addon ti piace, puoi offrirmi un caffè.");
    if clicked {
        // Off the render thread: ShellExecute may take a moment to start the browser.
        std::thread::spawn(|| open_url(KOFI_URL));
    }
}

fn open_url(url: &str) {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (verb, url) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call; the other pointers may be null.
    let code = unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), url.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
    if code as usize <= 32 {
        log::warn!("cannot open {KOFI_URL} (ShellExecute {})", code as usize);
    }
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
