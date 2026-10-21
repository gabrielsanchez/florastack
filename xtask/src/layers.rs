//! The crate dependency rules behind `cargo xtask check-layers`.
//!
//! The rules are data ([`RULES`]) checked by pure functions over a small model of the workspace ([`Crate`]), so every
//! rule has a unit test; [`from_metadata`] builds the model from `cargo metadata` and each crate's manifest.

use std::fmt;
use std::path::Path;

use serde_json::Value;

use crate::branding::Branding;

/// What each crate may depend on.
pub struct Rules {
    /// Every workspace crate, with the workspace crates it may depend on in any dependency table. Register a new crate
    /// here in the same change that adds it.
    pub allowed: &'static [(&'static str, &'static [&'static str])],
    /// Extra workspace crates that a crate may use only in `[dev-dependencies]`, such as the CPU reference that a GPU
    /// kernel's test compares against.
    pub allowed_dev: &'static [(&'static str, &'static [&'static str])],
    /// Test-only crates: any crate may list them in `[dev-dependencies]`, none in `[dependencies]` or
    /// `[build-dependencies]`.
    pub dev_only: &'static [&'static str],
    /// Test-only features of workspace crates, as (crate, feature): enabled only from `[dev-dependencies]`, and never
    /// by the crate's `default` feature, so that their code doesn't ship.
    pub dev_only_features: &'static [(&'static str, &'static str)],
    /// External UI toolkit crates; a trailing `*` matches every name with that prefix.
    pub ui_crates: &'static [&'static str],
    /// The only crates that may depend on a UI toolkit.
    pub ui_allowed: &'static [&'static str],
}

const CORE: &str = "florastack-core";
const ENGINE: &str = "florastack-engine";

/// FloraStack's layering: the library crates build on `florastack-core`, the engine on all of them, and the thin apps
/// on the engine alone.
pub const RULES: Rules = Rules {
    allowed: &[
        (CORE, &[]),
        ("florastack-codecs", &[CORE]),
        ("florastack-raw", &[CORE]),
        ("florastack-align", &[CORE]),
        ("florastack-stack", &[CORE]),
        ("florastack-gpu", &[CORE]),
        ("florastack-project", &[CORE]),
        ("florastack-retouch", &[CORE, "florastack-project"]),
        (
            ENGINE,
            &[
                CORE,
                "florastack-codecs",
                "florastack-raw",
                "florastack-align",
                "florastack-stack",
                "florastack-gpu",
                "florastack-project",
                "florastack-retouch",
            ],
        ),
        ("florastack-metrics", &[CORE]),
        ("florastack-testkit", &[]),
        ("florastack", &[ENGINE]),
        ("florastack-cli", &[ENGINE]),
        ("xtask", &[]),
    ],
    // The CPU references that the GPU kernels' agreement tests compare against (AGENTS.md, CPU oracle).
    allowed_dev: &[("florastack-gpu", &["florastack-align", "florastack-stack"])],
    dev_only: &["florastack-metrics", "florastack-testkit"],
    // The synthetic stack generator and test scenes (crates/core/src/testutil).
    dev_only_features: &[(CORE, "testutil")],
    ui_crates: &["egui*", "eframe", "winit", "rfd"],
    ui_allowed: &["florastack"],
};

/// The manifest table a dependency is declared in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Build,
    Dev,
}

impl fmt::Display for DepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DepKind::Normal => "[dependencies]",
            DepKind::Build => "[build-dependencies]",
            DepKind::Dev => "[dev-dependencies]",
        })
    }
}

/// A dependency of a workspace crate.
#[derive(Debug)]
pub struct Dep {
    /// Package name (never a `rename`d alias).
    pub name: String,
    pub kind: DepKind,
    /// Whether the dependency is itself a workspace crate.
    pub workspace: bool,
    /// The features the dependency declaration enables.
    pub features: Vec<String>,
}

/// A workspace crate, as far as the rules are concerned.
#[derive(Debug)]
pub struct Crate {
    pub name: String,
    pub deps: Vec<Dep>,
    /// Whether its manifest has `[lints] workspace = true`.
    pub inherits_lints: bool,
    /// Names of its binary targets.
    pub bins: Vec<String>,
    /// What its own `default` feature enables.
    pub default_features: Vec<String>,
}

/// A broken rule; its message says how to fix it.
#[derive(Debug, PartialEq, Eq)]
pub enum Violation {
    /// A workspace crate with no entry in [`Rules::allowed`].
    Unregistered { krate: String },
    /// An entry in [`Rules::allowed`] that names no workspace crate.
    StaleEntry { krate: &'static str },
    /// A dependency on a workspace crate outside the crate's allowlist.
    NotAllowed { krate: String, dep: String, kind: DepKind, allowed: &'static [&'static str] },
    /// A test-only crate outside `[dev-dependencies]`.
    DevOnly { krate: String, dep: String, kind: DepKind },
    /// A test-only feature enabled outside `[dev-dependencies]`.
    DevOnlyFeature { krate: String, dep: String, feature: &'static str, kind: DepKind },
    /// A test-only feature in a crate's `default` feature.
    DefaultDevOnlyFeature { krate: String, feature: &'static str },
    /// A UI toolkit used by a crate that may not use one.
    UiToolkit { krate: String, dep: String, allowed: &'static [&'static str] },
    /// A manifest without `[lints] workspace = true`.
    LintsNotInherited { krate: String },
    /// A binary named in `branding.toml` that no workspace crate builds.
    MissingBinary { name: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Unregistered { krate } => {
                write!(f, "{krate} is not in the allowlist: add it to RULES in xtask/src/layers.rs, with the workspace crates it may depend on")
            }
            Violation::StaleEntry { krate } => {
                write!(f, "RULES in xtask/src/layers.rs lists {krate}, but no workspace crate has that name: remove or rename the entry")
            }
            Violation::NotAllowed { krate, dep, kind, allowed } => {
                let allowed = if allowed.is_empty() { "no workspace crate".to_owned() } else { allowed.join(", ") };
                write!(f, "{krate} depends on {dep} in {kind}, but it may depend only on {allowed} (RULES in xtask/src/layers.rs)")
            }
            Violation::DevOnly { krate, dep, kind } => {
                write!(f, "{krate} depends on {dep} in {kind}, but {dep} is test-only: move it to [dev-dependencies]")
            }
            Violation::DevOnlyFeature { krate, dep, feature, kind } => {
                write!(f, "{krate} enables {dep}'s test-only feature `{feature}` in {kind}: enable it in [dev-dependencies] only")
            }
            Violation::DefaultDevOnlyFeature { krate, feature } => {
                write!(f, "{krate}'s default features include its test-only feature `{feature}`: take it out of `default`")
            }
            Violation::UiToolkit { krate, dep, allowed } => {
                let allowed = allowed.join(", ");
                write!(f, "{krate} depends on the UI toolkit {dep}, but only {allowed} may: keep the engine and libraries UI-free")
            }
            Violation::LintsNotInherited { krate } => {
                write!(f, "{krate}: its Cargo.toml lacks `[lints] workspace = true`, so the workspace lints (pedantic Clippy) don't apply")
            }
            Violation::MissingBinary { name } => {
                write!(f, "branding.toml names the binary {name}, but no workspace crate builds a binary with that name")
            }
        }
    }
}

/// Every rule that `crates` breaks, sorted by message.
pub fn check(rules: &Rules, crates: &[Crate], branding: &Branding) -> Vec<Violation> {
    let mut out = Vec::new();
    for &(krate, _) in rules.allowed {
        if !crates.iter().any(|c| c.name == krate) {
            out.push(Violation::StaleEntry { krate });
        }
    }
    for c in crates {
        if !c.inherits_lints {
            out.push(Violation::LintsNotInherited { krate: c.name.clone() });
        }
        let Some(allowed) = lookup(rules.allowed, &c.name) else {
            out.push(Violation::Unregistered { krate: c.name.clone() });
            continue;
        };
        for &(owner, feature) in rules.dev_only_features {
            if owner == c.name && c.default_features.iter().any(|f| f == feature) {
                out.push(Violation::DefaultDevOnlyFeature { krate: c.name.clone(), feature });
            }
        }
        for d in c.deps.iter().filter(|d| d.workspace && d.kind != DepKind::Dev) {
            for &(owner, feature) in rules.dev_only_features {
                if owner == d.name && d.features.iter().any(|f| f == feature) {
                    out.push(Violation::DevOnlyFeature { krate: c.name.clone(), dep: d.name.clone(), feature, kind: d.kind });
                }
            }
        }
        let allowed_dev = lookup(rules.allowed_dev, &c.name).unwrap_or_default();
        let permitted = |dep: &str, kind: DepKind| allowed.contains(&dep) || (kind == DepKind::Dev && allowed_dev.contains(&dep));
        for d in &c.deps {
            let dep = d.name.as_str();
            // A crate may dev-depend on itself, to turn on features for its own tests.
            if d.workspace && dep == c.name {
                continue;
            }
            if !d.workspace {
                if is_ui_crate(rules, dep) && !rules.ui_allowed.contains(&c.name.as_str()) {
                    out.push(Violation::UiToolkit { krate: c.name.clone(), dep: d.name.clone(), allowed: rules.ui_allowed });
                }
            } else if rules.dev_only.contains(&dep) {
                if d.kind != DepKind::Dev {
                    out.push(Violation::DevOnly { krate: c.name.clone(), dep: d.name.clone(), kind: d.kind });
                }
            } else if !permitted(dep, d.kind) {
                out.push(Violation::NotAllowed { krate: c.name.clone(), dep: d.name.clone(), kind: d.kind, allowed });
            }
        }
    }
    for name in branding.binary_names() {
        if !crates.iter().any(|c| c.bins.iter().any(|bin| bin == name)) {
            out.push(Violation::MissingBinary { name: name.to_owned() });
        }
    }
    out.sort_by_key(ToString::to_string);
    out.dedup();
    out
}

fn lookup(table: &'static [(&'static str, &'static [&'static str])], krate: &str) -> Option<&'static [&'static str]> {
    table.iter().find(|(name, _)| *name == krate).map(|&(_, deps)| deps)
}

fn is_ui_crate(rules: &Rules, name: &str) -> bool {
    rules.ui_crates.iter().any(|pattern| match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => name == *pattern,
    })
}

/// Builds the model from the output of `cargo metadata --format-version 1 --no-deps`, reading each crate's manifest
/// for its `[lints]` table (which `cargo metadata` doesn't report).
pub fn from_metadata(meta: &Value) -> Result<Vec<Crate>, String> {
    let packages = meta["packages"].as_array().ok_or("cargo metadata: no packages array")?;
    let members: Vec<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    let mut crates = Vec::with_capacity(packages.len());
    for p in packages {
        let name = p["name"].as_str().ok_or("cargo metadata: a package has no name")?;
        let mut deps = Vec::new();
        for d in p["dependencies"].as_array().into_iter().flatten() {
            let dep = d["name"].as_str().ok_or_else(|| format!("cargo metadata: a dependency of {name} has no name"))?;
            let kind = match d["kind"].as_str() {
                None => DepKind::Normal,
                Some("build") => DepKind::Build,
                Some("dev") => DepKind::Dev,
                Some(other) => return Err(format!("cargo metadata: unknown dependency kind {other:?} in {name}")),
            };
            let features = strings(&d["features"]);
            deps.push(Dep { name: dep.to_owned(), kind, workspace: members.contains(&dep), features });
        }
        let bins = p["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["kind"].as_array().is_some_and(|kinds| kinds.iter().any(|k| k.as_str() == Some("bin"))))
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        let manifest = p["manifest_path"].as_str().ok_or_else(|| format!("cargo metadata: {name} has no manifest_path"))?;
        let text = std::fs::read_to_string(manifest).map_err(|e| format!("cannot read {manifest}: {e}"))?;
        let inherits_lints = inherits_workspace_lints(&text).map_err(|e| format!("{manifest}: {e}"))?;
        let default_features = strings(&p["features"]["default"]);
        crates.push(Crate { name: name.to_owned(), deps, inherits_lints, bins, default_features });
    }
    crates.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(crates)
}

/// The strings of a JSON array (none for anything else).
fn strings(value: &Value) -> Vec<String> {
    value.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

/// Whether a manifest has `[lints] workspace = true`.
fn inherits_workspace_lints(manifest: &str) -> Result<bool, String> {
    let table: toml::Table = toml::from_str(manifest).map_err(|e| e.to_string())?;
    Ok(table.get("lints").and_then(|lints| lints.get("workspace")).and_then(toml::Value::as_bool) == Some(true))
}

/// `cargo xtask check-layers`: checks the workspace that `meta` (the output of `cargo metadata`) describes.
pub fn run(meta: &Value) -> Result<(), String> {
    let root = meta["workspace_root"].as_str().ok_or("cargo metadata: no workspace_root")?;
    let crates = from_metadata(meta)?;
    let branding = crate::branding::load(Path::new(root))?;
    let violations = check(&RULES, &crates, &branding);
    if !violations.is_empty() {
        let list: Vec<String> = violations.iter().map(|v| format!("  - {v}")).collect();
        return Err(format!("check-layers found {} problem(s):\n{}", violations.len(), list.join("\n")));
    }

    println!("check-layers: all {} crates follow the dependency, lint and branding rules", crates.len());
    let width = crates.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in &crates {
        let mut deps: Vec<String> = c
            .deps
            .iter()
            .filter(|d| d.workspace && d.name != c.name)
            .map(|d| if d.kind == DepKind::Dev { format!("{} (dev)", d.name) } else { d.name.clone() })
            .collect();
        deps.sort();
        deps.dedup();
        let deps = if deps.is_empty() { "-".to_owned() } else { deps.join(", ") };
        println!("  {:<width$}  {deps}", c.name);
    }
    println!(
        "branding: {} ({}), project folders <name>{}, binaries {}, XMP namespace {} ({}:)",
        branding.display_name,
        branding.app_id,
        branding.project_extension,
        branding.binary_names().join(", "),
        branding.xmp.namespace,
        branding.xmp.prefix
    );
    if branding.app_id.contains("OWNER") {
        println!("note: the app ID still contains the OWNER placeholder; replace it in branding.toml before a release");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workspace exactly as the rules allow it: every crate depends on all it may, and every binary exists.
    fn workspace() -> Vec<Crate> {
        let branding = crate::branding::repository();
        RULES
            .allowed
            .iter()
            .map(|&(name, allowed)| Crate {
                name: name.to_owned(),
                deps: allowed.iter().map(|&dep| Dep { name: dep.to_owned(), kind: DepKind::Normal, workspace: true, features: Vec::new() }).collect(),
                inherits_lints: true,
                bins: branding.binary_names().into_iter().filter(|bin| *bin == name).map(str::to_owned).collect(),
                default_features: Vec::new(),
            })
            .collect()
    }

    fn get<'a>(crates: &'a mut [Crate], name: &str) -> &'a mut Crate {
        crates.iter_mut().find(|c| c.name == name).expect("crate in the test workspace")
    }

    fn add_dep(crates: &mut [Crate], krate: &str, dep: &str, kind: DepKind) {
        get(crates, krate).deps.push(Dep { name: dep.to_owned(), kind, workspace: dep.starts_with("florastack"), features: Vec::new() });
    }

    fn violations(crates: &[Crate]) -> Vec<Violation> {
        check(&RULES, crates, &crate::branding::repository())
    }

    #[test]
    fn allowed_workspace_passes() {
        let v = violations(&workspace());
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn dependency_outside_the_allowlist_fails_in_every_table() {
        for kind in [DepKind::Normal, DepKind::Build, DepKind::Dev] {
            let mut ws = workspace();
            add_dep(&mut ws, "florastack-codecs", "florastack-project", kind);
            let v = violations(&ws);
            assert!(
                matches!(&v[..], [Violation::NotAllowed { krate, dep, kind: k, .. }]
                    if krate == "florastack-codecs" && dep == "florastack-project" && *k == kind),
                "{v:?}"
            );
        }
    }

    #[test]
    fn apps_may_depend_only_on_the_engine() {
        for app in ["florastack", "florastack-cli"] {
            let mut ws = workspace();
            add_dep(&mut ws, app, CORE, DepKind::Normal);
            let v = violations(&ws);
            assert!(matches!(&v[..], [Violation::NotAllowed { krate, .. }] if krate == app), "{v:?}");
        }
    }

    #[test]
    fn metrics_and_testkit_are_dev_only() {
        assert_eq!(RULES.dev_only, ["florastack-metrics", "florastack-testkit"]);
        for &test_crate in RULES.dev_only {
            for kind in [DepKind::Normal, DepKind::Build] {
                let mut ws = workspace();
                add_dep(&mut ws, "florastack-stack", test_crate, kind);
                let v = violations(&ws);
                assert!(
                    matches!(&v[..], [Violation::DevOnly { krate, dep, kind: k }]
                        if krate == "florastack-stack" && dep == test_crate && *k == kind),
                    "{v:?}"
                );
            }
            let mut ws = workspace();
            for krate in ["florastack-stack", ENGINE, "florastack-cli", "xtask"] {
                add_dep(&mut ws, krate, test_crate, DepKind::Dev);
            }
            let v = violations(&ws);
            assert!(v.is_empty(), "{v:?}");
        }
    }

    #[test]
    fn gpu_tests_may_use_the_cpu_references_as_dev_dependencies_only() {
        let mut ws = workspace();
        for reference in ["florastack-align", "florastack-stack"] {
            add_dep(&mut ws, "florastack-gpu", reference, DepKind::Dev);
        }
        let v = violations(&ws);
        assert!(v.is_empty(), "{v:?}");
        add_dep(&mut ws, "florastack-gpu", "florastack-stack", DepKind::Normal);
        let v = violations(&ws);
        assert!(matches!(&v[..], [Violation::NotAllowed { krate, kind: DepKind::Normal, .. }] if krate == "florastack-gpu"), "{v:?}");
        // The allowance is the GPU crate's alone.
        let mut ws = workspace();
        add_dep(&mut ws, "florastack-codecs", "florastack-stack", DepKind::Dev);
        let v = violations(&ws);
        assert!(matches!(&v[..], [Violation::NotAllowed { krate, kind: DepKind::Dev, .. }] if krate == "florastack-codecs"), "{v:?}");
    }

    #[test]
    fn unregistered_crate_and_stale_entry_fail() {
        let mut ws = workspace();
        ws.retain(|c| c.name != "florastack-gpu");
        get(&mut ws, ENGINE).deps.retain(|d| d.name != "florastack-gpu");
        ws.push(Crate {
            name: "florastack-capture".to_owned(),
            deps: Vec::new(),
            inherits_lints: true,
            bins: Vec::new(),
            default_features: Vec::new(),
        });
        let v = violations(&ws);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.contains(&Violation::StaleEntry { krate: "florastack-gpu" }), "{v:?}");
        assert!(v.contains(&Violation::Unregistered { krate: "florastack-capture".to_owned() }), "{v:?}");
    }

    #[test]
    fn ui_toolkits_only_in_the_desktop_app() {
        let mut ws = workspace();
        add_dep(&mut ws, "florastack", "eframe", DepKind::Normal);
        add_dep(&mut ws, "florastack", "egui_kittest", DepKind::Dev);
        let v = violations(&ws);
        assert!(v.is_empty(), "{v:?}");
        for (krate, dep) in [(ENGINE, "egui"), ("florastack-gpu", "egui-wgpu"), ("florastack-retouch", "winit"), ("florastack-cli", "rfd")] {
            let mut ws = workspace();
            add_dep(&mut ws, krate, dep, DepKind::Normal);
            let v = violations(&ws);
            assert!(matches!(&v[..], [Violation::UiToolkit { krate: k, .. }] if k == krate), "{krate} -> {dep}: {v:?}");
        }
    }

    #[test]
    fn test_only_features_are_enabled_from_dev_dependencies_only() {
        assert_eq!(RULES.dev_only_features, [(CORE, "testutil")]);
        let with_feature = |kind| Dep { name: CORE.to_owned(), kind, workspace: true, features: vec!["testutil".to_owned()] };
        let mut ws = workspace();
        for krate in ["florastack-align", "florastack-metrics", CORE] {
            get(&mut ws, krate).deps.push(with_feature(DepKind::Dev));
        }
        let v = violations(&ws);
        assert!(v.is_empty(), "{v:?}");
        for kind in [DepKind::Normal, DepKind::Build] {
            let mut ws = workspace();
            get(&mut ws, "florastack-stack").deps.push(with_feature(kind));
            let v = violations(&ws);
            assert!(
                matches!(&v[..], [Violation::DevOnlyFeature { krate, feature: "testutil", kind: k, .. }] if krate == "florastack-stack" && *k == kind),
                "{v:?}"
            );
        }
        let mut ws = workspace();
        get(&mut ws, CORE).default_features = vec!["testutil".to_owned()];
        assert_eq!(violations(&ws), [Violation::DefaultDevOnlyFeature { krate: CORE.to_owned(), feature: "testutil" }]);
    }

    #[test]
    fn every_crate_inherits_the_workspace_lints() {
        let mut ws = workspace();
        get(&mut ws, "florastack-align").inherits_lints = false;
        assert_eq!(violations(&ws), [Violation::LintsNotInherited { krate: "florastack-align".to_owned() }]);
    }

    #[test]
    fn a_crate_may_dev_depend_on_itself() {
        let mut ws = workspace();
        add_dep(&mut ws, CORE, CORE, DepKind::Dev);
        let v = violations(&ws);
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn branding_binaries_must_exist() {
        let mut ws = workspace();
        get(&mut ws, "florastack-cli").bins = vec!["fs".to_owned()];
        let v = violations(&ws);
        assert!(matches!(&v[..], [Violation::MissingBinary { name }] if name == "florastack-cli"), "{v:?}");
    }

    #[test]
    fn rules_are_consistent() {
        let names: Vec<&str> = RULES.allowed.iter().map(|&(name, _)| name).collect();
        for (i, name) in names.iter().enumerate() {
            assert!(!names[..i].contains(name), "{name} is listed twice");
        }
        let referenced = RULES
            .allowed
            .iter()
            .flat_map(|&(_, deps)| deps)
            .chain(RULES.allowed_dev.iter().flat_map(|(name, deps)| std::iter::once(name).chain(deps.iter())))
            .chain(RULES.dev_only)
            .chain(RULES.dev_only_features.iter().map(|(owner, _)| owner))
            .chain(RULES.ui_allowed);
        for name in referenced {
            assert!(names.contains(name), "{name} is not a registered crate");
        }
    }

    #[test]
    fn detects_inherited_workspace_lints() {
        assert_eq!(inherits_workspace_lints("[package]\nname = \"a\"\n\n[lints]\nworkspace = true\n"), Ok(true));
        assert_eq!(inherits_workspace_lints("lints.workspace = true\n"), Ok(true));
        assert_eq!(inherits_workspace_lints("[package]\nname = \"a\"\n"), Ok(false));
        assert_eq!(inherits_workspace_lints("[lints.clippy]\npedantic = \"warn\"\n"), Ok(false));
        assert!(inherits_workspace_lints("[lints\n").is_err());
    }
}
