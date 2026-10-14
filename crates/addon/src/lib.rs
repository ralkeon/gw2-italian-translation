//! Italian Translation: Nexus addon.
//!
//! Text Translator (Ideka) hooks the game text and hands it to a "module" process. Our module is
//! the shim (gw2it_shim.exe), which only relays the requests to this DLL over named pipes. All the
//! logic and the in-game UI live here.
//!
//! Every source text is saved in strings.db. A text with a saved translation gets it at once;
//! the others are translated in the background (DeepL, Gemini when DeepL is used up, or the
//! project's server for players without keys: translate.rs) and pushed into the game when ready.
//! Translations are shared with the other players through a global database (sync.rs).

pub(crate) use gw2it_text::{markup, plural};
mod config;
mod install;
mod options;
mod pipe;
mod state;
mod store;
mod sync;
mod translate;
mod ui;

use nexus::{
    AddonFlags, UpdateProvider,
    gui::{RenderType, register_render, render},
    keybind::{keybind_handler, register_keybind_with_string},
    paths::get_addon_dir,
};

pub(crate) const ADDON_DIR: &str = "italian_translation";

nexus::export! {
    name: "Italian Translation",
    signature: -0x4732_4954, // "G2IT", negative: not on Raidcore
    load,
    unload,
    flags: AddonFlags::None,
    // New versions from the GitHub releases (the repository in Cargo.toml).
    provider: UpdateProvider::GitHub,
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
    match get_addon_dir("text_translator") {
        Ok(tt_dir) => install::module(&tt_dir),
        Err(e) => log::error!("no Text Translator folder: {e}"),
    }
    let db = dir.join("strings.db");
    let backlog = match store::start(&db) {
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
    sync::start(db);

    register_render(RenderType::Render, render!(ui::render)).revert_on_unload();
    register_render(RenderType::OptionsRender, render!(options::render)).revert_on_unload();
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
    sync::stop();
    translate::stop();
    store::stop();
    log::info!("unloaded");
}
