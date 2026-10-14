//! The DLL installs its own Text Translator module: the shim (built into it by build.rs) and
//! module.toml go to text_translator/modules/italian_translation/, refreshed when they change.
//! Until the module has worked once, it is also made Text Translator's active module, so that
//! the player has nothing to choose. After that the choice is the player's.

use std::{
    fs, io,
    path::Path,
};

use crate::config;

const SHIM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/gw2it_shim.exe"));
const MODULE_TOML: &str = include_str!("../../../module/module.toml");
const MODULE_DIR: &str = "italian_translation";

/// `tt_dir`: Text Translator's data folder (addons/text_translator).
pub fn module(tt_dir: &Path) {
    let dir = tt_dir.join("modules").join(MODULE_DIR);
    if let Err(e) = fs::create_dir_all(&dir) {
        log::error!("cannot create {}: {e}", dir.display());
        return;
    }
    if let Err(e) = write_shim(&dir.join("gw2it_shim.exe")) {
        log::error!("cannot install the shim in {}: {e}", dir.display());
    }
    if let Err(e) = write_if_changed(&dir.join("module.toml"), MODULE_TOML.as_bytes()) {
        log::error!("cannot install module.toml in {}: {e}", dir.display());
    }
    if !config::get().install.module_worked {
        choose(&tt_dir.join("settings.toml"));
    }
}

/// The shim connected: the module works, Text Translator's choice is left alone from now on.
pub fn module_works() {
    let mut c = config::get();
    if !c.install.module_worked {
        c.install.module_worked = true;
        config::set(c);
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    fs::write(path, bytes)?;
    Ok(true)
}

/// Text Translator may be running the old shim: a running exe cannot be written, but it can be
/// renamed, and it goes away at the next load.
fn write_shim(path: &Path) -> io::Result<()> {
    let old = path.with_extension("exe.old");
    let _ = fs::remove_file(&old);
    match write_if_changed(path, SHIM) {
        Ok(true) => log::info!("shim installed in {}", path.display()),
        Ok(false) => {}
        Err(_) if path.exists() => {
            fs::rename(path, &old)?;
            fs::write(path, SHIM)?;
            log::info!("shim updated in {} (the old one was running)", path.display());
        }
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Makes our module the active one in Text Translator's settings.toml, where it is kept as the
/// folder name in UTF-16 (`[active_module] Windows = [...]`). Only an existing file is changed:
/// Text Translator writes it the first time it runs.
fn choose(settings: &Path) {
    let Ok(text) = fs::read_to_string(settings) else {
        log::info!("{} not there yet: the module is chosen at the next load", settings.display());
        return;
    };
    let mut table: toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => {
            log::warn!("{}: {e}", settings.display());
            return;
        }
    };
    let ours: toml::value::Array = MODULE_DIR.encode_utf16().map(|c| toml::Value::Integer(c.into())).collect();
    let current = table.get("active_module").and_then(|m| m.get("Windows")).and_then(toml::Value::as_array);
    if current == Some(&ours) {
        return;
    }
    let mut active = toml::Table::new();
    active.insert("Windows".into(), toml::Value::Array(ours));
    table.insert("active_module".into(), toml::Value::Table(active));
    match toml::to_string(&table).map_err(io::Error::other).and_then(|text| fs::write(settings, text)) {
        Ok(()) => log::info!("Italian Translation made Text Translator's active module"),
        Err(e) => log::warn!("cannot write {}: {e}", settings.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_the_module() {
        let path = std::env::temp_dir().join(format!("gw2it-settings-{}.toml", std::process::id()));
        fs::write(&path, "show_translated = true\n\n[active_module]\nWindows = [108, 116]\n\n[refresh_debounce]\nsecs = 0\nnanos = 500000000\n").unwrap();
        choose(&path);
        let table: toml::Table = fs::read_to_string(&path).unwrap().parse().unwrap();
        let _ = fs::remove_file(&path);
        let name: Vec<u16> = table["active_module"]["Windows"].as_array().unwrap().iter().map(|v| v.as_integer().unwrap() as u16).collect();
        assert_eq!(String::from_utf16(&name).unwrap(), "italian_translation");
        assert_eq!(table["show_translated"].as_bool(), Some(true));
        assert_eq!(table["refresh_debounce"]["nanos"].as_integer(), Some(500_000_000));
    }
}
