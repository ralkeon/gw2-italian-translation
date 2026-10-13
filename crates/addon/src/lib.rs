//! GW2 Italian Translation: Nexus addon.
//!
//! Text Translator (Ideka) hooks the game text and hands it to a "module" process. Our module is
//! the shim (gw2it_shim.exe), which only relays the requests to this DLL over named pipes. All the
//! logic and the in-game UI live here.
//!
//! Every source text is saved in strings.db. A text with a saved translation gets it at once;
//! the others are translated by DeepL in the background and pushed into the game when ready.

mod config;
mod deepl;
mod markup;
mod pipe;
mod plural;
mod state;
mod store;
mod translate;
mod ui;

use nexus::{
    AddonFlags, UpdateProvider,
    gui::{RenderType, register_render, render},
    keybind::{keybind_handler, register_keybind_with_string},
    paths::get_addon_dir,
};

pub(crate) const ADDON_DIR: &str = "gw2_italian_translation";

nexus::export! {
    name: "GW2 Italian Translation",
    signature: -0x4732_4954, // "G2IT", negative: not on Raidcore
    load,
    unload,
    flags: AddonFlags::None,
    provider: UpdateProvider::None,
    // ureq/rustls trace every HTTP exchange byte by byte: only their warnings.
    log_filter: "debug,ureq=warn,ureq_proto=warn,rustls=warn",
}

fn load() {
    let dir = match get_addon_dir(ADDON_DIR) {
        Ok(dir) => dir,
        Err(e) => {
            log::error!("no addon folder: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error!("cannot create {}: {e}", dir.display());
        return;
    }

    config::load(&dir.join("config.toml"));
    config::load_contributor(&dir.join("contributor.json"));
    let backlog = match store::start(&dir.join("strings.db")) {
        Ok(backlog) => backlog,
        Err(e) => {
            log::error!("cannot open strings.db: {e}");
            return;
        }
    };
    if !backlog.is_empty() {
        log::info!("{} texts without a translation from earlier sessions: queued", backlog.len());
    }
    translate::start(backlog);
    pipe::start();

    register_render(RenderType::Render, render!(ui::render)).revert_on_unload();
    register_keybind_with_string(
        "GW2IT_TOGGLE_WINDOW",
        keybind_handler!(|_, is_release| {
            if !is_release {
                ui::toggle();
            }
        }),
        "ALT+SHIFT+I",
    )
    .revert_on_unload();

    log::info!("loaded, data in {}", dir.display());
}

fn unload() {
    // Our threads must be gone before Nexus frees the DLL code they run.
    pipe::stop();
    translate::stop();
    store::stop();
    log::info!("unloaded");
}
