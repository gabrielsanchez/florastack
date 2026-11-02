//! Installs Khronos' official validator as a test fixture, outside the source tree.

use std::process::Command;

pub fn run() -> Result<(), String> {
    let folder = std::env::temp_dir().join("florastack-gltf-validator-2.0.0-dev.3.10");
    let package = folder.join("node_modules/gltf-validator/package.json");
    if let Ok(bytes) = std::fs::read(&package)
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
        && value["version"] == "2.0.0-dev.3.10"
        && folder.join("node_modules/gltf-validator/index.js").is_file()
        && folder.join("node_modules/gltf-validator/gltf_validator.dart.js").is_file()
    {
        println!("Khronos glTF validator fixture is ready");
        return Ok(());
    }
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/c", "npm"]);
        c
    } else {
        Command::new("npm")
    };
    let status = command
        .args(["install", "--prefix"])
        .arg(&folder)
        .arg("--cache")
        .arg(folder.join("npm-cache"))
        .args(["--ignore-scripts", "--no-audit", "--no-fund", "--save-exact", "--user-agent=FloraStack-tests", "gltf-validator@2.0.0-dev.3.10"])
        .status()
        .map_err(|e| format!("cannot run npm (Node.js is required for the official glTF validator): {e}"))?;
    if !status.success() {
        return Err(format!("installing Khronos' glTF validator failed: {status}"));
    }
    Ok(())
}
