//! Builds the shim (crates/shim) so that the DLL carries it: install.rs writes it into Text
//! Translator's modules folder, and the player copies one file only.
//!
//! A cargo of its own, with its own target folder: the outer one holds the lock on target/.

use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let workspace = manifest_dir.join("../..");
    let profile = env::var("PROFILE").unwrap();
    let (target, host) = (env::var("TARGET").unwrap(), env::var("HOST").unwrap());
    let target_dir = out.join("shim-target");

    let mut cargo = Command::new(env::var_os("CARGO").unwrap());
    cargo
        .args(["build", "--package", "gw2it-shim", "--manifest-path"])
        .arg(workspace.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .env_remove("CARGO_TARGET_DIR");
    if profile == "release" {
        cargo.arg("--release");
    }
    let mut built = target_dir.clone();
    if target != host {
        cargo.args(["--target", &target]);
        built.push(&target);
    }
    built.push(&profile);
    built.push("gw2it_shim.exe");

    let status = cargo.status().expect("run cargo for the shim");
    assert!(status.success(), "building the shim failed");
    fs::copy(&built, out.join("gw2it_shim.exe")).expect("copy the shim");

    for path in ["crates/shim", "crates/proto", "module/module.toml", "Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", workspace.join(path).display());
    }
}
