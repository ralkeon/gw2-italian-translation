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

/// Ko-fi's cup (from its own button), drawn inside ours.
const KOFI_ICON: &[u8] = include_bytes!("../assets/kofi-cup.png");

/// On the right, as other addons do: "Se l'addon ti piace" and a button to the project's Ko-fi page with
/// Ko-fi's cup, opened in the default browser.
fn tips(ui: &Ui) {
    const NOTE: &str = "Se l'addon ti piace";
    const LABEL: &str = "Supportami su Ko-fi";
    let pad = ui.clone_style().frame_padding[0];
    let height = ui.frame_height();
    let icon = [height * 0.8 * 64.0 / 56.0, height * 0.8];
    let text = ui.calc_text_size(LABEL);
    let width = pad + icon[0] + pad * 0.75 + text[0] + pad;
    let note = ui.calc_text_size(NOTE)[0];
    let spacing = ui.clone_style().item_spacing[0];
    let [x, y] = ui.cursor_pos();
    let free = ui.content_region_avail()[0] - (note + spacing + width);
    ui.set_cursor_pos([x + free.max(0.0), y]);
    ui.align_text_to_frame_padding();
    ui.text_disabled(NOTE);
    ui.same_line();
    let clicked = {
        let _button = ui.push_style_color(StyleColor::Button, KOFI);
        let _hovered = ui.push_style_color(StyleColor::ButtonHovered, KOFI_HOVER);
        let _active = ui.push_style_color(StyleColor::ButtonActive, KOFI);
        ui.button_with_size("##kofi", [width, height])
    };
    let [left, top] = ui.item_rect_min();
    let draw = ui.get_window_draw_list();
    if let Some(cup) = nexus::texture::get_texture_or_create_from_memory("GW2IT_KOFI_CUP", KOFI_ICON) {
        let at = [left + pad, top + (height - icon[1]) / 2.0];
        draw.add_image(cup.id(), at, [at[0] + icon[0], at[1] + icon[1]]).build();
    }
    draw.add_text([left + pad + icon[0] + pad * 0.75, top + (height - text[1]) / 2.0], [1.0, 1.0, 1.0, 1.0], LABEL);
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
