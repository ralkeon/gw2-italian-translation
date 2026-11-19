//! Italian Translation: Nexus addon.
//!
//! Text Translator (Ideka) hooks the game text and hands it to a "module" process. Our module is
//! the shim (gw2it_shim.exe), which only relays the requests to this DLL over named pipes. All the
//! logic and the in-game UI live here.
//!
//! Every source text is saved in strings.db. The translations come from the project's database on
//! ifalna (sync.rs), made there by the translation agents: a text that has one gets it at once, a
//! text that has none yet stays English until it arrives (no translator runs here).

pub(crate) use gw2it_text::{markup, plural};
mod cache;
mod config;
mod install;
mod options;
mod pipe;
mod state;
mod store;
mod sync;

use nexus::{
    AddonFlags, UpdateProvider,
    gui::{RenderType, register_render, render},
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
    let tt_dir = match get_addon_dir("text_translator") {
        Ok(tt_dir) => {
            install::module(&tt_dir);
            Some(tt_dir)
        }
        Err(e) => {
            log::error!("no Text Translator folder: {e}");
            None
        }
    };
    let db = dir.join("strings.db");
    if let Err(e) = store::start(&db) {
        log::error!("cannot open strings.db: {e}");
        return;
    }
    if let Some(tt_dir) = &tt_dir {
        cache::start(tt_dir, &db);
    }
    pipe::start();
    sync::start(db);

    register_render(RenderType::OptionsRender, render!(options::render)).revert_on_unload();

    log::info!("loaded, data in {}", dir.display());
}

fn unload() {
    // Our threads must be gone before Nexus frees the DLL code they run.
    cache::stop();
    pipe::stop();
    sync::stop();
    store::stop();
    log::info!("unloaded");
}
