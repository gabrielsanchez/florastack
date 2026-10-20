//! The schemas of `corpus/manifest.toml` (stacks to download) and of a local stack's `stack.toml`, and the checks
//! that keep a manifest entry from writing outside its folder or installing unverified bytes.

use std::collections::BTreeSet;

use serde::Deserialize;

/// The manifest format this xtask reads.
pub const VERSION: u32 = 1;
/// The largest file an entry may declare, and the cap on a download whose entry declares no size.
pub const MAX_FILE_BYTES: u64 = 4 << 30;

/// What a stack shows; the kinds of subject the stacking methods are tuned and scored on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    Insect,
    Flower,
    Coin,
    Landscape,
    Microscopy,
}

impl Subject {
    pub fn as_str(self) -> &'static str {
        match self {
            Subject::Insect => "insect",
            Subject::Flower => "flower",
            Subject::Coin => "coin",
            Subject::Landscape => "landscape",
            Subject::Microscopy => "microscopy",
        }
    }
}

/// Which way focus moves through the frames, in their listed order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FrameOrder {
    /// The first frame is focused nearest the camera.
    NearToFar,
    /// The first frame is focused farthest away.
    FarToNear,
    /// Not known, or deliberately shuffled (to test ordering).
    Unknown,
}

impl FrameOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            FrameOrder::NearToFar => "near-to-far",
            FrameOrder::FarToNear => "far-to-near",
            FrameOrder::Unknown => "unknown order",
        }
    }
}

/// What is known for sure about a real stack, for judging results by eye or against a reference.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundTruth {
    /// Free text: which details must come out sharp, where halos tend to appear, which frames show movement.
    #[serde(default)]
    pub notes: String,
    /// A file of the stack that is a trusted result, such as a carefully retouched stack; it is not a frame.
    pub reference: Option<String>,
}

/// `corpus/manifest.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default, rename = "stack")]
    pub stacks: Vec<RemoteStack>,
}

/// A `[[stack]]` of the manifest: downloaded into `corpus/stacks/<name>/`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteStack {
    /// The folder name: lower-case letters, digits and dashes.
    pub name: String,
    /// A slot for a stack still to be chosen: it has no files and is never downloaded.
    #[serde(default)]
    pub placeholder: bool,
    pub subject: Subject,
    pub frame_order: FrameOrder,
    #[serde(default)]
    pub description: String,
    /// The page the stack comes from.
    pub source: Option<String>,
    /// The license of the images (an SPDX identifier where there is one).
    pub license: Option<String>,
    /// Who made the images, as the license asks to credit them.
    pub attribution: Option<String>,
    pub ground_truth: Option<GroundTruth>,
    /// The frames in focus order, and the ground-truth reference if there is one.
    #[serde(default)]
    pub files: Vec<RemoteFile>,
}

/// A `[[stack.files]]` entry: one pinned download.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteFile {
    /// The file name in the stack's folder.
    pub name: String,
    /// `https://`, `http://` (the hash protects it) or `file://`.
    pub url: String,
    /// The lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// The size in bytes: the download stops past it.
    pub size: Option<u64>,
}

/// A local stack's `stack.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackToml {
    pub subject: Subject,
    pub frame_order: FrameOrder,
    #[serde(default)]
    pub description: String,
    /// The frames in focus order; without it, every image file in the folder in name order.
    pub frames: Option<Vec<String>>,
    /// Where the stack comes from, for people (and later the quality report); xtask only checks that it parses.
    #[allow(dead_code)]
    pub source: Option<String>,
    pub license: Option<String>,
    pub attribution: Option<String>,
    pub ground_truth: Option<GroundTruth>,
}

pub fn parse(text: &str) -> Result<Manifest, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

/// A stack folder name: 1 to 64 lower-case ASCII letters, digits and dashes, starting with a letter or digit.
pub fn is_stack_name(name: &str) -> bool {
    (1..=64).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !name.starts_with('-')
}

/// A file name that stays inside its folder on every OS: 1 to 128 ASCII letters, digits, `.`, `_`, `-` and `+`,
/// not starting or ending with a dot, and not a reserved Windows device name.
pub fn is_file_name(name: &str) -> bool {
    const RESERVED: [&str; 4] = ["con", "prn", "aux", "nul"];
    let stem = name.split('.').next().unwrap_or_default().to_ascii_lowercase();
    let device = RESERVED.contains(&stem.as_str())
        || ((stem.starts_with("com") || stem.starts_with("lpt")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    (1..=128).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !device
}

fn is_sha256(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_url(url: &str) -> bool {
    ["https://", "http://", "file://"].iter().any(|scheme| url.len() > scheme.len() && url.starts_with(scheme))
}

impl Manifest {
    /// Everything wrong with the manifest, one message per problem.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.version != VERSION {
            out.push(format!("version is {}, but this xtask reads version {VERSION}", self.version));
        }
        let mut names = BTreeSet::new();
        for stack in &self.stacks {
            let mut problem = |what: String| out.push(format!("stack `{}`: {what}", stack.name));
            if !is_stack_name(&stack.name) {
                problem("the name must be 1 to 64 lower-case letters, digits and dashes".to_owned());
            }
            if !names.insert(stack.name.as_str()) {
                problem("the name is used twice".to_owned());
            }
            if !stack.placeholder {
                for (field, value) in [("source", &stack.source), ("license", &stack.license)] {
                    if value.as_deref().is_none_or(|v| v.trim().is_empty()) {
                        problem(format!("`{field}` is required (every downloaded stack records where it comes from and its license)"));
                    }
                }
                if stack.files.is_empty() {
                    problem("no files: list the frames under [[stack.files]], or mark the stack `placeholder = true`".to_owned());
                }
            }
            let mut file_names = BTreeSet::new();
            for file in &stack.files {
                if !is_file_name(&file.name) {
                    problem(format!("file `{}`: names are 1 to 128 letters, digits, `.`, `_`, `-` and `+`", file.name));
                }
                // Case-insensitive file systems (Windows, macOS) would merge names that differ only in case.
                if !file_names.insert(file.name.to_ascii_lowercase()) {
                    problem(format!("file `{}` is listed twice", file.name));
                }
                if !is_url(&file.url) {
                    problem(format!("file `{}`: the url must start with https://, http:// or file://", file.name));
                }
                if !is_sha256(&file.sha256) {
                    problem(format!("file `{}`: sha256 must be 64 lower-case hex digits", file.name));
                }
                if file.size.is_some_and(|size| size == 0 || size > MAX_FILE_BYTES) {
                    problem(format!("file `{}`: size must be 1 to {MAX_FILE_BYTES} bytes", file.name));
                }
            }
            if let Some(reference) = stack.ground_truth.as_ref().and_then(|truth| truth.reference.as_deref())
                && !stack.files.iter().any(|file| file.name == reference)
            {
                problem(format!("the ground-truth reference `{reference}` is not one of its files"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn manifest(stacks: &str) -> Manifest {
        parse(&format!("version = 1\n{stacks}")).unwrap()
    }

    #[test]
    fn the_repository_manifest_is_valid() {
        let m = parse(include_str!("../../../corpus/manifest.toml")).unwrap();
        assert_eq!(m.problems(), Vec::<String>::new());
        assert!(!m.stacks.is_empty(), "the manifest lists stacks");
        let subjects: BTreeSet<&str> = m.stacks.iter().map(|s| s.subject.as_str()).collect();
        assert_eq!(subjects.len(), 5, "a placeholder per subject: {subjects:?}");
    }

    #[test]
    fn a_complete_stack_passes() {
        let m = manifest(&format!(
            "[[stack]]\nname = \"bee-2026\"\nsubject = \"insect\"\nframe_order = \"near-to-far\"\nsource = \"https://example.org/bee\"\n\
             license = \"CC0-1.0\"\nground_truth = {{ notes = \"hairs\", reference = \"ref.tif\" }}\n\
             [[stack.files]]\nname = \"IMG_0001.JPG\"\nurl = \"https://example.org/1.jpg\"\nsha256 = \"{HASH}\"\nsize = 3\n\
             [[stack.files]]\nname = \"ref.tif\"\nurl = \"file:///C:/stacks/ref.tif\"\nsha256 = \"{HASH}\"\n"
        ));
        assert_eq!(m.problems(), Vec::<String>::new());
        assert_eq!(m.stacks[0].ground_truth.as_ref().unwrap().reference.as_deref(), Some("ref.tif"));
    }

    #[test]
    fn unsafe_or_incomplete_entries_are_reported() {
        let m = manifest(&format!(
            "[[stack]]\nname = \"Bad Name\"\nsubject = \"coin\"\nframe_order = \"unknown\"\n\
             [[stack]]\nname = \"twice\"\nplaceholder = true\nsubject = \"flower\"\nframe_order = \"far-to-near\"\n\
             [[stack]]\nname = \"twice\"\nsubject = \"flower\"\nframe_order = \"far-to-near\"\nsource = \"s\"\nlicense = \"CC-BY-4.0\"\n\
             ground_truth = {{ reference = \"missing.tif\" }}\n\
             [[stack.files]]\nname = \"../escape.jpg\"\nurl = \"ftp://example.org/a\"\nsha256 = \"{}\"\n\
             [[stack.files]]\nname = \"A.jpg\"\nurl = \"https://example.org/a\"\nsha256 = \"{HASH}\"\nsize = 0\n\
             [[stack.files]]\nname = \"a.JPG\"\nurl = \"https://example.org/b\"\nsha256 = \"{HASH}\"\n",
            HASH.to_uppercase()
        ));
        let problems = m.problems().join("\n");
        for expected in [
            "stack `Bad Name`: the name must be",
            "stack `Bad Name`: `source` is required",
            "stack `Bad Name`: `license` is required",
            "stack `Bad Name`: no files",
            "stack `twice`: the name is used twice",
            "file `../escape.jpg`: names are",
            "file `../escape.jpg`: the url must start with",
            "file `../escape.jpg`: sha256 must be 64 lower-case hex digits",
            "file `A.jpg`: size must be",
            "file `a.JPG` is listed twice",
            "the ground-truth reference `missing.tif` is not one of its files",
        ] {
            assert!(problems.contains(expected), "missing {expected:?} in:\n{problems}");
        }
        assert!(!problems.contains("stack `twice`: no files"), "placeholders need no files:\n{problems}");
        assert!(parse("version = 2\n").unwrap().problems()[0].contains("version is 2"));
    }

    #[test]
    fn unknown_keys_and_subjects_are_parse_errors() {
        assert!(parse("version = 1\n[[stack]]\nname = \"a\"\nsubject = \"mineral\"\nframe_order = \"unknown\"\n").is_err());
        assert!(parse("version = 1\n[[stack]]\nname = \"a\"\nsubject = \"coin\"\nframe_order = \"unknown\"\nsha = \"x\"\n").is_err());
    }

    #[test]
    fn file_names_stay_in_their_folder() {
        for good in ["IMG_0001.CR3", "frame-001.tif", "a+b.png", "x"] {
            assert!(is_file_name(good), "{good}");
        }
        for bad in ["", ".hidden", "a/b.jpg", "a\\b.jpg", "..", "c:x", "trailing.", "con.txt", "COM1.jpg", "lpt9", &"a".repeat(129)] {
            assert!(!is_file_name(bad), "{bad}");
        }
        assert!(is_file_name("com10.jpg") && is_file_name("console.jpg"));
        assert!(is_stack_name("bee-2026") && !is_stack_name("-bee") && !is_stack_name("Bee") && !is_stack_name(""));
    }
}
