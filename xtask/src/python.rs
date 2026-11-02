//! Build and exercise the Python extension in an isolated virtual environment, using maturin as users do.

use std::path::Path;
use std::process::Command;

pub fn run(root: &Path) -> Result<(), String> {
    let environment = root.join("target/python-tests");
    let python = environment.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" });
    if !python.is_file() {
        let interpreter = std::env::var_os("PYO3_PYTHON").unwrap_or_else(|| if cfg!(windows) { "python".into() } else { "python3".into() });
        checked(Command::new(interpreter).args(["-m", "venv"]).arg(&environment), "create a Python virtual environment (Python 3.9+ required)")?;
    }
    checked(
        Command::new(&python).args(["-m", "pip", "--disable-pip-version-check", "install", "maturin>=1.12,<2", "pytest>=8,<10", "numpy>=1.23,<3"]),
        "install Python test tools",
    )?;
    checked(
        Command::new(&python)
            .args(["-m", "maturin", "develop", "--locked", "--manifest-path"])
            .arg(root.join("python/florastack/Cargo.toml"))
            .env("VIRTUAL_ENV", &environment)
            .env("PYO3_PYTHON", &python),
        "build and install the Python extension",
    )?;
    checked(
        Command::new(&python)
            .args(["-m", "pytest", "-q", "-p", "no:cacheprovider"])
            .arg(root.join("python/florastack/tests"))
            .env("PYTHONDONTWRITEBYTECODE", "1"),
        "run Python acceptance tests",
    )
}

fn checked(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command.status().map_err(|e| format!("cannot {what}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("cannot {what}: {status}")) }
}
