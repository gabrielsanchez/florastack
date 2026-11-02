//! Build and exercise the Python extension in an isolated virtual environment, using maturin as users do.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn run(root: &Path) -> Result<(), String> {
    let environment = root.join("target/python-tests");
    let python = prepare_environment(&environment)?;
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

/// Recreate the disposable environment: CI's Rust cache can restore an interpreter while omitting `pyvenv.cfg` or
/// pip's Python files. An executable alone does not establish a usable venv, and venvs also contain machine-specific
/// paths. `--clear` removes partial packages before venv bootstraps pip again, without touching the base interpreter.
fn prepare_environment(environment: &Path) -> Result<PathBuf, String> {
    let interpreter = std::env::var_os("PYO3_PYTHON").unwrap_or_else(|| if cfg!(windows) { "python".into() } else { "python3".into() });
    checked(
        Command::new(interpreter).args(["-m", "venv", "--clear"]).arg(environment),
        "create a Python virtual environment (Python 3.9+ required)",
    )?;
    Ok(environment.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" }))
}

fn checked(command: &mut Command, what: &str) -> Result<(), String> {
    let status = command.status().map_err(|e| format!("cannot {what}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("cannot {what}: {status}")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Check both isolation and pip's entry point without downloading any packages.
    fn assert_ready(python: &Path, environment: &Path) {
        checked(
            Command::new(python)
                .args([
                    "-I",
                    "-c",
                    "import pathlib, sys, pip.__main__; assert sys.prefix != sys.base_prefix; \
                     assert pathlib.Path(sys.prefix).resolve() == pathlib.Path(sys.argv[1]).resolve()",
                ])
                .arg(environment),
            "verify the test environment's isolation and pip",
        )
        .unwrap();
    }

    #[test]
    fn partial_cached_environments_are_recreated() {
        let environment = std::env::temp_dir().join(format!("florastack-python-tests-{}", std::process::id()));
        let python = prepare_environment(&environment).unwrap();
        assert_ready(&python, &environment);

        // A cached executable without the venv marker can run the externally managed base Python on macOS.
        std::fs::remove_file(environment.join("pyvenv.cfg")).unwrap();
        assert!(python.is_file());
        let python = prepare_environment(&environment).unwrap();
        assert_ready(&python, &environment);

        // A cached pip directory without its entry point produces Windows CI's "No module named pip.__main__".
        let output = Command::new(&python).args(["-I", "-c", "import pip.__main__; print(pip.__main__.__file__)"]).output().unwrap();
        assert!(output.status.success());
        let entry = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert!(entry.starts_with(&environment));
        std::fs::remove_file(entry).unwrap();
        assert!(!Command::new(&python).args(["-m", "pip", "--version"]).output().unwrap().status.success());
        let python = prepare_environment(&environment).unwrap();
        assert_ready(&python, &environment);
        std::fs::remove_dir_all(environment).unwrap();
    }
}
