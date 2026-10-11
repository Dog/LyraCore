use serde::Deserialize;
use std::path::PathBuf;
use std::{env, fs, process::Command};

#[derive(Deserialize)]
struct Prepared {
    directory: PathBuf,
    watches: Vec<PathBuf>,
}

fn prepare() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?)
        .parent()
        .ok_or("Gateway has no workspace directory")?
        .to_path_buf();
    let pin_path = root.join(".lyracore-cli-rev");
    println!("cargo:rerun-if-changed={}", pin_path.display());
    let pin = fs::read_to_string(&pin_path)?;
    let pin = pin.trim();
    if pin.len() != 40 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid CLI revision".into());
    }
    // Call the installed binary directly. The shim may install it with Cargo, which would wait
    // for this build's lock. Preparation installs it before any Gateway build starts.
    let cli = root.join(".lyracore/cli").join(pin).join("bin/lyracore");
    println!("cargo:rerun-if-changed={}", cli.display());
    let output = Command::new(cli)
        .args(["bindings", "--check"])
        .current_dir(&root)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let prepared: Prepared = serde_json::from_slice(&output.stdout)?;
    for path in prepared.watches {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let wrapper = format!(
        "#[allow(clippy::too_many_lines)]\n#[path = {:?}]\npub mod bindings;\n",
        prepared.directory.join("mod.rs")
    );
    let destination = PathBuf::from(env::var("OUT_DIR")?).join("bindings.rs");
    if fs::read_to_string(&destination).ok().as_deref() != Some(&wrapper) {
        fs::write(destination, wrapper)?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = prepare() {
        eprintln!("Gateway bindings are missing or stale: {error}\nRun `./lyracore bindings` before Cargo.");
        std::process::exit(1);
    }
}
