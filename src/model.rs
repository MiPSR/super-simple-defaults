use std::fmt;

#[derive(Clone, PartialEq, Debug)]
pub enum Choice {
    None,                    // no default (for category main)
    App(String, String),     // desktop id, display name
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Choice::None => write!(f, "None"),
            Choice::App(_, name) => write!(f, "{}", name),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub mime_types: Vec<String>,
    pub icon: Option<std::path::PathBuf>,
}

#[derive(Clone, Debug)]
pub struct MimeEntry {
    pub mime: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct MimeDefaultState {
    pub mime: String,
    pub name: String,
    pub default: Choice,
}

#[derive(Clone, Debug)]
pub struct State {
    pub apps: Vec<AppEntry>,
    pub entries: Vec<MimeDefaultState>,
    pub search: String,
    pub status: String,
    pub dirty: bool,
    pub dark: bool,
    pub open: Option<String>,
}

impl State {
    pub fn apps_for_mime(&self, mime: &str) -> Vec<AppEntry> {
        let mut v: Vec<AppEntry> = self
            .apps
            .iter()
            .filter(|a| a.mime_types.iter().any(|m| m == mime))
            .cloned()
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}
