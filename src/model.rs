use std::collections::HashMap;
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

#[derive(Clone, PartialEq, Debug)]
pub enum SubChoice {
    Inherit,
    None,
    App(String, String),
}

impl fmt::Display for SubChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubChoice::Inherit => write!(f, "Use category default"),
            SubChoice::None => write!(f, "None"),
            SubChoice::App(_, name) => write!(f, "{}", name),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub mime_types: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MimeEntry {
    pub mime: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct CategoryState {
    pub label: String,
    pub mimes: Vec<String>,       // mime strings
    pub mime_names: HashMap<String, String>,
    pub main: Choice,
    pub overrides: HashMap<String, SubChoice>,
}

#[derive(Clone, Debug)]
pub struct State {
    pub apps: Vec<AppEntry>,
    pub categories: Vec<CategoryState>,
    pub status: String,
    pub dirty: bool,
    pub dark: bool,
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

    pub fn apps_for_category(&self, cat: &CategoryState) -> Vec<AppEntry> {
        let mut seen = std::collections::HashSet::new();
        let mut v = Vec::new();
        for m in &cat.mimes {
            for a in &self.apps {
                if a.mime_types.iter().any(|x| x == m) && seen.insert(a.id.clone()) {
                    v.push(a.clone());
                }
            }
        }
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
}

pub fn media_label(media: &str) -> String {
    match media {
        "text" => "Text".to_string(),
        "image" => "Images".to_string(),
        "audio" => "Audio".to_string(),
        "video" => "Video".to_string(),
        "application" => "Applications / Documents".to_string(),
        "inode" => "Files & Folders".to_string(),
        "message" => "Messages / Email".to_string(),
        "model" => "3D Models".to_string(),
        "multipart" => "Multipart".to_string(),
        "font" => "Fonts".to_string(),
        "chemical" => "Chemical".to_string(),
        "x-content" => "Media / Devices".to_string(),
        "x-scheme-handler" => "URL Schemes".to_string(),
        "x-epoc" => "EPOC".to_string(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => other.to_string(),
            }
        }
    }
}