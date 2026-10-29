//! Indexed icon-theme lookup: no recursive icon-directory scans.
use crate::config::Paths;
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug)]
pub enum LoadedIcon {
    Svg(iced::widget::svg::Handle),
    Raster(iced::widget::image::Handle),
}

pub type IconCache = BTreeMap<String, Option<LoadedIcon>>;

fn decode(path: &Path) -> Option<LoadedIcon> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e == "svg") {
        roxmltree::Document::parse(std::str::from_utf8(&bytes).ok()?).ok()?;
        Some(LoadedIcon::Svg(iced::widget::svg::Handle::from_memory(
            bytes,
        )))
    } else {
        let mut reader = image::io::Reader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::io::Limits::default();
        limits.max_image_width = Some(4096);
        limits.max_image_height = Some(4096);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let rgba = reader.decode().ok()?.into_rgba8();
        Some(LoadedIcon::Raster(iced::widget::image::Handle::from_rgba(
            rgba.width(),
            rgba.height(),
            rgba.into_raw(),
        )))
    }
}

#[derive(Clone)]
struct Directory {
    name: String,
    size: u32,
    scale: u32,
    min: u32,
    max: u32,
}

impl Directory {
    fn distance(&self, size: u32, scale: u32) -> u32 {
        let wanted = size.saturating_mul(scale);
        let low = self.min.saturating_mul(self.scale);
        let high = self.max.saturating_mul(self.scale);
        if wanted < low {
            low - wanted
        } else {
            wanted.saturating_sub(high)
        }
    }
}

#[derive(Default)]
struct IconTheme {
    directories: Vec<Directory>,
    parents: Vec<String>,
}

pub struct Resolver {
    roots: Vec<PathBuf>,
    pixmaps: Vec<PathBuf>,
    selected: String,
    scale: u32,
    themes: BTreeMap<String, IconTheme>,
    paths: BTreeMap<String, Option<PathBuf>>,
}

fn safe_relative(name: &str) -> bool {
    !name.is_empty()
        && Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn ini(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut sections = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.into();
        } else if let Some((key, value)) = line.split_once('=') {
            sections
                .entry(section.clone())
                .or_default()
                .insert(key.trim().into(), value.trim().into());
        }
    }
    sections
}

impl Resolver {
    pub fn new(paths: &Paths, theme: Option<String>, scale: u32) -> Self {
        let mut roots = Vec::new();
        if let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
        {
            roots.push(home.join(".icons"));
        }
        roots.extend(paths.data_roots().iter().map(|p| p.join("icons")));
        Self {
            roots,
            pixmaps: paths
                .data_roots()
                .iter()
                .map(|p| p.join("pixmaps"))
                .collect(),
            selected: theme
                .filter(|t| safe_relative(t) && !t.contains('/'))
                .unwrap_or_else(|| "hicolor".into()),
            scale: scale.clamp(1, 4),
            themes: BTreeMap::new(),
            paths: BTreeMap::new(),
        }
    }

    fn theme(&mut self, name: &str) -> &IconTheme {
        if !self.themes.contains_key(name) {
            let mut theme = IconTheme::default();
            for root in &self.roots {
                let Ok(text) = std::fs::read_to_string(root.join(name).join("index.theme")) else {
                    continue;
                };
                let sections = ini(&text);
                let Some(header) = sections.get("Icon Theme") else {
                    continue;
                };
                if let Some(parents) = header.get("Inherits") {
                    for parent in parents
                        .split(',')
                        .map(str::trim)
                        .filter(|p| safe_relative(p) && !p.contains('/'))
                    {
                        if !theme.parents.iter().any(|p| p == parent) {
                            theme.parents.push(parent.into());
                        }
                    }
                }
                for key in ["Directories", "ScaledDirectories"] {
                    for directory in header
                        .get(key)
                        .into_iter()
                        .flat_map(|v| v.split(','))
                        .map(str::trim)
                    {
                        if !safe_relative(directory)
                            || theme.directories.iter().any(|d| d.name == directory)
                        {
                            continue;
                        }
                        let Some(values) = sections.get(directory) else {
                            continue;
                        };
                        let number = |key: &str, fallback: u32| {
                            values
                                .get(key)
                                .and_then(|v| v.parse::<u32>().ok())
                                .unwrap_or(fallback)
                                .min(4096)
                        };
                        let size = number("Size", 24);
                        let scale = number("Scale", 1).max(1);
                        let (min, max) = match values
                            .get("Type")
                            .map(String::as_str)
                            .unwrap_or("Threshold")
                        {
                            "Fixed" => (size, size),
                            "Scalable" => (number("MinSize", size), number("MaxSize", size)),
                            _ => {
                                let threshold = number("Threshold", 2);
                                (
                                    size.saturating_sub(threshold),
                                    size.saturating_add(threshold),
                                )
                            }
                        };
                        theme.directories.push(Directory {
                            name: directory.into(),
                            size,
                            scale,
                            min,
                            max,
                        });
                    }
                }
            }
            // Some application bundles install hicolor icons without index.theme.
            // Probe standard paths directly rather than recursively scanning them.
            if name == "hicolor" && theme.directories.is_empty() {
                for context in [
                    "apps",
                    "mimetypes",
                    "actions",
                    "status",
                    "devices",
                    "places",
                ] {
                    for size in [24, 22, 32, 48, 64, 128, 256] {
                        theme.directories.push(Directory {
                            name: format!("{size}x{size}/{context}"),
                            size,
                            scale: 1,
                            min: size,
                            max: size,
                        });
                    }
                    theme.directories.push(Directory {
                        name: format!("scalable/{context}"),
                        size: 24,
                        scale: 1,
                        min: 1,
                        max: 4096,
                    });
                }
            }
            self.themes.insert(name.into(), theme);
        }
        &self.themes[name]
    }

    fn in_theme(
        &mut self,
        icon: &str,
        name: &str,
        visited: &mut HashSet<String>,
    ) -> Option<PathBuf> {
        if !visited.insert(name.into()) {
            return None;
        }
        let theme = self.theme(name);
        let mut directories = theme.directories.clone();
        let parents = theme.parents.clone();
        directories.sort_by_key(|d| {
            (
                !(d.scale == self.scale && d.min <= 24 && d.max >= 24),
                d.distance(24, self.scale),
                d.size.abs_diff(24),
            )
        });
        for directory in directories {
            for root in &self.roots {
                if let Some(path) = image_path(&root.join(name).join(&directory.name), icon) {
                    return Some(path);
                }
            }
        }
        for parent in parents {
            if let Some(path) = self.in_theme(icon, &parent, visited) {
                return Some(path);
            }
        }
        None
    }

    fn find(&mut self, name: &str) -> Option<PathBuf> {
        if let Some(cached) = self.paths.get(name) {
            return cached.clone();
        }
        let path = Path::new(name);
        let found = if path.is_absolute() {
            path.is_file().then(|| path.to_path_buf())
        } else if safe_relative(name) && !name.contains('/') {
            let selected = self.selected.clone();
            let mut visited = HashSet::new();
            self.in_theme(name, &selected, &mut visited)
                .or_else(|| self.in_theme(name, "hicolor", &mut visited))
                .or_else(|| self.pixmaps.iter().find_map(|root| image_path(root, name)))
                .or_else(|| self.roots.iter().find_map(|root| image_path(root, name)))
        } else {
            None
        };
        self.paths.insert(name.into(), found.clone());
        found
    }

    pub fn resolve(&mut self, names: Vec<String>) -> IconCache {
        names
            .into_iter()
            .filter(|n| !n.is_empty())
            .map(|name| {
                let decoded = self.find(&name).as_deref().and_then(decode);
                (name, decoded)
            })
            .collect()
    }
}

fn image_path(root: &Path, name: &str) -> Option<PathBuf> {
    // SVG is preferred so it remains sharp on fractional/high-DPI displays.
    for filename in [
        format!("{name}.svg"),
        format!("{name}.png"),
        format!("{name}.ico"),
        name.to_string(),
    ] {
        let path = root.join(filename);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_and_raster_bytes_are_loaded_on_the_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let svg = tmp.path().join("icon.svg");
        std::fs::write(
            &svg,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\"/>",
        )
        .unwrap();
        assert!(matches!(decode(&svg), Some(LoadedIcon::Svg(_))));
        let png = tmp.path().join("icon.png");
        image::RgbaImage::new(2, 2).save(&png).unwrap();
        assert!(matches!(decode(&png), Some(LoadedIcon::Raster(_))));
        assert!(decode(&tmp.path().join("missing.svg")).is_none());
    }

    #[test]
    fn indexed_theme_inheritance_size_and_cycles() {
        let tmp = tempfile::tempdir().unwrap();
        let mut resolver = Resolver {
            roots: vec![tmp.path().into()],
            pixmaps: vec![],
            selected: "child".into(),
            scale: 1,
            themes: BTreeMap::new(),
            paths: BTreeMap::new(),
        };
        for (name, parent) in [
            ("child", "parent"),
            ("parent", "grandparent"),
            ("grandparent", "child"),
        ] {
            let root = tmp.path().join(name);
            std::fs::create_dir_all(root.join("24/apps")).unwrap();
            std::fs::create_dir_all(root.join("48/apps")).unwrap();
            std::fs::write(root.join("index.theme"), format!("[Icon Theme]\nInherits={parent}\nDirectories=48/apps,24/apps\n[48/apps]\nSize=48\nType=Fixed\n[24/apps]\nSize=24\nType=Fixed\n")).unwrap();
        }
        for size in [24, 48] {
            std::fs::write(
                tmp.path()
                    .join(format!("grandparent/{size}/apps/example.svg")),
                "<svg/>",
            )
            .unwrap();
        }
        assert_eq!(
            resolver.find("example"),
            Some(tmp.path().join("grandparent/24/apps/example.svg"))
        );
        assert!(resolver.find("missing").is_none());
        assert!(resolver.paths.contains_key("missing"));
        assert!(!safe_relative("../outside"));
    }
}
