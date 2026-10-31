//! What the app remembers between runs ([`Prefs`]): the theme, the recent projects, the folders last used, the tab
//! and the method chosen. eframe keeps them with the window's size and place and the panels' widths (its
//! `persistence`), in the app's storage folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The key the preferences are stored under.
const KEY: &str = "florastack-prefs";
/// How many recent projects File > Open recent lists.
pub const MAX_RECENT: usize = 10;

/// The UI's theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

/// The workflow's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    #[default]
    Stack,
    Retouch,
    Export,
    Batch,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Stack, Tab::Retouch, Tab::Export, Tab::Batch];

    pub fn label_key(self) -> &'static str {
        match self {
            Tab::Stack => "tab-stack",
            Tab::Retouch => "tab-retouch",
            Tab::Export => "tab-export",
            Tab::Batch => "tab-batch",
        }
    }
}

/// The stacking method the Stack tab runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Average,
    #[default]
    Pyramid,
    DepthMap,
}

impl Method {
    pub const ALL: [Method; 3] = [Method::Pyramid, Method::DepthMap, Method::Average];

    /// The command that stacks by it.
    pub fn command(self) -> &'static str {
        match self {
            Method::Average => "stack.average",
            Method::Pyramid => "stack.pyramid",
            Method::DepthMap => "stack.depthMap",
        }
    }

    /// Its name, as `stack.preview` takes it.
    pub fn name(self) -> &'static str {
        match self {
            Method::Average => "average",
            Method::Pyramid => "pyramid",
            Method::DepthMap => "depth-map",
        }
    }

    pub fn label_key(self) -> &'static str {
        match self {
            Method::Average => "method-average",
            Method::Pyramid => "method-pyramid",
            Method::DepthMap => "method-depth-map",
        }
    }
}

/// The app's remembered settings. See the module documentation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: Theme,
    /// Project folders, the most recent first.
    pub recent: Vec<PathBuf>,
    /// The folder the last file dialog was in.
    pub last_folder: Option<PathBuf>,
    pub tab: Tab,
    pub method: Method,
}

impl Prefs {
    /// The preferences eframe stored, or the defaults.
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Prefs {
        storage.and_then(|s| s.get_string(KEY)).and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        if let Ok(text) = serde_json::to_string(self) {
            storage.set_string(KEY, text);
        }
    }

    /// Puts `project` at the top of the recent projects.
    pub fn remember(&mut self, project: &Path) {
        self.recent.retain(|p| p != project);
        self.recent.insert(0, project.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// Drops the recent projects whose folder is gone.
    pub fn forget_missing(&mut self) {
        self.recent.retain(|p| p.is_dir());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_projects_are_most_recent_first_without_duplicates() {
        let mut prefs = Prefs::default();
        for i in 0..12 {
            prefs.remember(Path::new(&format!("/p{i}")));
        }
        prefs.remember(Path::new("/p5"));
        assert_eq!(prefs.recent.len(), MAX_RECENT);
        assert_eq!(prefs.recent[0], Path::new("/p5"));
        assert_eq!(prefs.recent.iter().filter(|p| *p == Path::new("/p5")).count(), 1);
        prefs.forget_missing();
        assert!(prefs.recent.is_empty(), "none of them exists");
    }

    #[test]
    fn preferences_round_trip_and_unknown_or_missing_fields_are_fine() {
        let prefs = Prefs { theme: Theme::Light, tab: Tab::Export, method: Method::DepthMap, ..Prefs::default() };
        let text = serde_json::to_string(&prefs).unwrap();
        assert_eq!(serde_json::from_str::<Prefs>(&text).unwrap(), prefs);
        let partial: Prefs = serde_json::from_str(r#"{ "theme": "light", "future": 1 }"#).unwrap();
        assert_eq!(partial.theme, Theme::Light);
    }
}
