//! Layered MIME configuration and lossless, optimistic user-file updates.
use crate::model::{Edit, Edits};
use fs2::FileExt;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const DEFAULTS: &str = "Default Applications";
pub const ADDED: &str = "Added Associations";
pub const REMOVED: &str = "Removed Associations";

#[derive(Clone, Debug)]
pub struct Paths {
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    pub data_dirs: Vec<PathBuf>,
    pub desktops: Vec<String>,
    pub locales: Vec<String>,
}

fn absolute(value: Option<String>) -> Option<PathBuf> {
    value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn home_path(value: Option<String>, home: &Path, suffix: &str) -> PathBuf {
    absolute(value).unwrap_or_else(|| home.join(suffix))
}

fn search_paths(value: Option<String>, fallback: &str) -> Vec<PathBuf> {
    let value = value
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.into());
    let mut paths = Vec::new();
    for path in value
        .split(':')
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        paths = fallback.split(':').map(PathBuf::from).collect();
    }
    paths
}

impl Paths {
    pub fn environment() -> Result<Self, String> {
        Self::from_env(|key| std::env::var(key).ok())
    }

    fn from_env(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        // Never fall back to the working directory for configuration writes.
        let home = absolute(get("HOME"));
        let config_home = absolute(get("XDG_CONFIG_HOME"))
            .or_else(|| home.as_ref().map(|h| home_path(None, h, ".config")))
            .ok_or("Set HOME or an absolute XDG_CONFIG_HOME")?;
        let data_home = absolute(get("XDG_DATA_HOME"))
            .or_else(|| home.as_ref().map(|h| home_path(None, h, ".local/share")))
            .ok_or("Set HOME or an absolute XDG_DATA_HOME")?;
        let mut data_dirs = search_paths(get("XDG_DATA_DIRS"), "/usr/local/share:/usr/share");
        let nix = PathBuf::from("/run/current-system/sw/share");
        if nix.is_dir() && !data_dirs.contains(&nix) {
            data_dirs.push(nix);
        }
        let desktops = get("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .split(':')
            .map(str::to_ascii_lowercase)
            .filter(|s| {
                !s.is_empty()
                    && s.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
            })
            .collect();
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .into_iter()
            .find_map(|key| get(key).filter(|v| !v.is_empty()))
            .unwrap_or_default();
        Ok(Self {
            config_home,
            data_home,
            config_dirs: search_paths(get("XDG_CONFIG_DIRS"), "/etc/xdg"),
            data_dirs,
            desktops,
            locales: locale_variants(&locale),
        })
    }

    pub fn data_roots(&self) -> Vec<PathBuf> {
        let mut roots = vec![self.data_home.clone()];
        for root in &self.data_dirs {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        roots
    }

    pub fn target(&self) -> PathBuf {
        self.config_home.join("mimeapps.list")
    }

    pub fn candidates(&self) -> Vec<Candidate> {
        let mut result = Vec::new();
        let mut append = |base: &Path, user: bool, origin: Option<usize>| {
            for desktop in &self.desktops {
                result.push(Candidate {
                    path: base.join(format!("{desktop}-mimeapps.list")),
                    user,
                    desktop_specific: true,
                    origin,
                });
            }
            result.push(Candidate {
                path: base.join("mimeapps.list"),
                user,
                desktop_specific: false,
                origin,
            });
        };
        append(&self.config_home, true, None);
        for base in &self.config_dirs {
            append(base, false, None);
        }
        for (origin, base) in self.data_roots().iter().enumerate() {
            append(&base.join("applications"), origin == 0, Some(origin));
        }
        let mut seen = BTreeSet::new();
        result.retain(|c| seen.insert(c.path.clone()));
        result
    }

    /// Include desktop-specific user files even for desktops not currently active.
    /// Reset is explicitly "all user defaults", not deletion of entire files.
    pub fn user_files(&self) -> Result<Vec<PathBuf>, String> {
        let mut paths = BTreeSet::from([
            self.target(),
            self.data_home.join("applications/mimeapps.list"),
        ]);
        for base in [&self.config_home, &self.data_home.join("applications")] {
            match fs::read_dir(base) {
                Ok(entries) => {
                    for entry in entries {
                        let entry = entry.map_err(|e| format!("{}: {e}", base.display()))?;
                        let name = entry.file_name();
                        if name.to_str().is_some_and(|n| n.ends_with("-mimeapps.list")) {
                            paths.insert(entry.path());
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("{}: {e}", base.display())),
            }
        }
        for c in self.candidates().into_iter().filter(|c| c.user) {
            paths.insert(c.path);
        }
        Ok(paths.into_iter().collect())
    }
}

pub fn locale_variants(locale: &str) -> Vec<String> {
    let (base, modifier) = locale
        .split_once('@')
        .map_or((locale, None), |(b, m)| (b, Some(m)));
    let base = base.split('.').next().unwrap_or("");
    if base.is_empty() || base == "C" || base == "POSIX" {
        return vec![];
    }
    let language = base.split('_').next().unwrap_or(base);
    let mut result = Vec::new();
    if let Some(m) = modifier {
        result.push(format!("{base}@{m}"));
    }
    result.push(base.to_string());
    if let Some(m) = modifier {
        result.push(format!("{language}@{m}"));
    }
    result.push(language.into());
    result.dedup();
    result
}

#[derive(Clone, Debug)]
pub struct Candidate {
    pub path: PathBuf,
    pub user: bool,
    pub desktop_specific: bool,
    pub origin: Option<usize>,
}

/// Original lines remain intact unless their exact key is edited.
#[derive(Clone, Debug, Default)]
pub struct Document {
    pub text: String,
}

fn section(line: &str) -> Option<&str> {
    line.trim().strip_prefix('[')?.strip_suffix(']')
}

fn key_value(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

pub fn list(value: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut part = String::new();
    let mut escaped = false;
    for c in value.chars() {
        if escaped {
            part.push(match c {
                's' => ' ',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                _ => c,
            });
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == ';' {
            let item = part.trim().to_string();
            if !item.is_empty() && !result.contains(&item) {
                result.push(item);
            }
            part.clear();
        } else {
            part.push(c);
        }
    }
    let item = part.trim().to_string();
    if !item.is_empty() && !result.contains(&item) {
        result.push(item);
    }
    result
}

fn serialize(values: &[String]) -> String {
    values
        .iter()
        .map(|v| format!("{};", v.replace('\\', "\\\\").replace(';', "\\;")))
        .collect()
}

impl Document {
    pub fn values(&self, wanted: &str) -> BTreeMap<String, Vec<String>> {
        let mut current = "";
        let mut result = BTreeMap::new();
        for line in self.text.lines() {
            if let Some(name) = section(line) {
                current = name;
            } else if current == wanted
                && let Some((key, value)) = key_value(line)
            {
                result.insert(key.into(), list(value));
            }
        }
        result
    }

    pub fn set(&mut self, wanted: &str, key: &str, value: Option<&[String]>) {
        if value.is_none_or(|v| v.is_empty()) && !self.values(wanted).contains_key(key) {
            return;
        }
        let newline = if self.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut lines: Vec<String> = self.text.lines().map(str::to_string).collect();
        let mut current = String::new();
        let mut found_section = false;
        let mut inserted = false;
        let replacement = value
            .filter(|v| !v.is_empty())
            .map(|v| format!("{key}={}", serialize(v)));
        let mut out = Vec::new();
        for line in lines.drain(..) {
            if let Some(name) = section(&line) {
                if current == wanted
                    && !inserted
                    && let Some(v) = &replacement
                {
                    out.push(v.clone());
                    inserted = true;
                }
                current = name.into();
                found_section |= current == wanted;
                out.push(line);
            } else if current == wanted && key_value(&line).is_some_and(|(k, _)| k == key) {
                if !inserted && let Some(v) = &replacement {
                    out.push(v.clone());
                    inserted = true;
                }
            } else {
                out.push(line);
            }
        }
        if !inserted && let Some(v) = replacement {
            if !found_section {
                if !out.is_empty() && !out.last().is_some_and(|s| s.is_empty()) {
                    out.push(String::new());
                }
                out.push(format!("[{wanted}]"));
            }
            out.push(v);
        }
        self.text = out.join(newline);
        if !self.text.is_empty() {
            self.text.push_str(newline);
        }
    }

    pub fn clear_defaults(&mut self) {
        if !self
            .text
            .lines()
            .any(|line| section(line) == Some(DEFAULTS))
        {
            return;
        }
        let newline = if self.text.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let mut in_defaults = false;
        let mut out = Vec::new();
        for line in self.text.lines() {
            if let Some(name) = section(line) {
                in_defaults = name == DEFAULTS;
                if !in_defaults {
                    out.push(line);
                }
            } else if !in_defaults || key_value(line).is_none() {
                // Keep comments and whitespace even when removing a default section.
                out.push(line);
            }
        }
        self.text = out.join(newline);
        if !self.text.is_empty() {
            self.text.push_str(newline);
        }
    }
}

pub fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub paths: Paths,
    pub files: BTreeMap<PathBuf, Option<String>>,
    pub candidates: Vec<Candidate>,
    pub aliases: BTreeMap<String, String>,
}

impl Snapshot {
    pub fn load(paths: Paths) -> Result<Self, String> {
        let candidates = paths.candidates();
        let mut files = BTreeMap::new();
        for path in candidates
            .iter()
            .map(|c| c.path.clone())
            .chain(paths.user_files()?)
        {
            if !files.contains_key(&path) {
                // Unavailable system files are skipped by the lookup standard.
                // Unreadable user files must fail closed to avoid replacing
                // configuration whose contents we could not preserve.
                let user_file = candidates.iter().any(|c| c.user && c.path == path)
                    || path.starts_with(&paths.config_home)
                    || path.starts_with(paths.data_home.join("applications"));
                let contents = match read(&path) {
                    Ok(contents) => contents,
                    Err(error) if user_file => return Err(error),
                    Err(_) => None,
                };
                files.insert(path.clone(), contents);
            }
        }
        let mut aliases = BTreeMap::new();
        for root in paths.data_roots() {
            if let Ok(text) = fs::read_to_string(root.join("mime/aliases")) {
                for line in text.lines() {
                    let mut words = line.split_whitespace();
                    if let (Some(alias), Some(canonical)) = (words.next(), words.next()) {
                        aliases
                            .entry(alias.into())
                            .or_insert_with(|| canonical.into());
                    }
                }
            }
        }
        Ok(Self {
            paths,
            files,
            candidates,
            aliases,
        })
    }

    pub fn document(&self, path: &Path) -> Document {
        Document {
            text: self
                .files
                .get(path)
                .and_then(Clone::clone)
                .unwrap_or_default(),
        }
    }

    fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        let mut current = mime;
        let mut seen = BTreeSet::new();
        while seen.insert(current) {
            match self.aliases.get(current) {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }

    fn matching(&self, doc: &Document, section: &str, mime: &str) -> BTreeMap<String, Vec<String>> {
        doc.values(section)
            .into_iter()
            .filter(|(key, _)| self.canonical(key) == mime)
            .collect()
    }

    fn remove_default(&self, doc: &mut Document, mime: &str) {
        for key in self.matching(doc, DEFAULTS, mime).keys() {
            doc.set(DEFAULTS, key, None);
        }
    }
}

#[derive(Clone, Debug)]
pub enum SaveError {
    Conflict(Vec<String>),
    Failed(String),
}

fn ensure_directory(path: &Path) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn lock(paths: &Paths) -> Result<File, String> {
    ensure_directory(&paths.config_home)?;
    let path = paths.config_home.join("ssd.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.lock_exclusive().map_err(|e| e.to_string())?;
    Ok(file)
}

fn atomic_write(path: &Path, text: &str, expected: &Option<String>) -> Result<(), String> {
    let parent = path.parent().ok_or("Configuration path has no parent")?;
    ensure_directory(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    if let Ok(metadata) = fs::metadata(path) {
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(
                metadata.permissions().mode() & 0o777,
            ))
            .map_err(|e| e.to_string())?;
    }
    temp.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    temp.as_file().sync_all().map_err(|e| e.to_string())?;
    if &read(path)? != expected {
        return Err(format!(
            "{} changed during the write; reload and retry",
            path.display()
        ));
    }
    temp.persist(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())
}

pub fn save(snapshot: &Snapshot, edits: &Edits, overwrite: bool) -> Result<(), SaveError> {
    for (mime, edit) in edits {
        if mime.is_empty() || mime.chars().any(|c| c.is_control() || "=[]".contains(c)) {
            return Err(SaveError::Failed(
                "Invalid MIME key; nothing was written".into(),
            ));
        }
        if let Edit::Set(ids) = edit
            && ids
                .iter()
                .any(|id| id.is_empty() || id.chars().any(char::is_control) || id.contains('/'))
        {
            return Err(SaveError::Failed(
                "Invalid desktop ID; nothing was written".into(),
            ));
        }
    }
    let _lock = lock(&snapshot.paths).map_err(SaveError::Failed)?;
    let fresh = Snapshot::load(snapshot.paths.clone()).map_err(SaveError::Failed)?;
    if !overwrite {
        let mut conflicts = BTreeSet::new();
        let paths: BTreeSet<_> = snapshot.files.keys().chain(fresh.files.keys()).collect();
        for path in paths {
            let before = snapshot.document(path);
            let after = fresh.document(path);
            for section in [DEFAULTS, ADDED, REMOVED] {
                for mime in edits.keys() {
                    if snapshot.matching(&before, section, mime)
                        != fresh.matching(&after, section, mime)
                    {
                        conflicts.insert(mime.clone());
                    }
                }
            }
        }
        if !conflicts.is_empty() {
            return Err(SaveError::Conflict(conflicts.into_iter().collect()));
        }
    }
    let target = snapshot.paths.target();
    let mut updates = BTreeMap::new();
    // Remove edited defaults from other applicable user files so higher-priority
    // desktop-specific entries cannot silently override the canonical user file.
    for candidate in fresh
        .candidates
        .iter()
        .filter(|c| c.user && c.path != target)
    {
        let mut doc = fresh.document(&candidate.path);
        for mime in edits.keys() {
            fresh.remove_default(&mut doc, mime);
        }
        updates.insert(candidate.path.clone(), doc);
    }
    let mut doc = fresh.document(&target);
    for (mime, edit) in edits {
        fresh.remove_default(&mut doc, mime);
        match edit {
            Edit::Inherit => {}
            Edit::Set(ids) => {
                doc.set(DEFAULTS, mime, Some(ids));
                // Explicitly associate selected applications and undo conflicting
                // local removals. Unrelated associations and their order survive.
                if !ids.is_empty() {
                    let mut added = Vec::new();
                    for (key, values) in fresh.matching(&doc, ADDED, mime) {
                        for id in values {
                            if !added.contains(&id) {
                                added.push(id);
                            }
                        }
                        doc.set(ADDED, &key, None);
                    }
                    for id in ids {
                        if !added.contains(id) {
                            added.push(id.clone());
                        }
                    }
                    doc.set(ADDED, mime, Some(&added));
                    for (key, values) in fresh.matching(&doc, REMOVED, mime) {
                        let removed: Vec<_> =
                            values.into_iter().filter(|id| !ids.contains(id)).collect();
                        doc.set(REMOVED, &key, Some(&removed));
                    }
                }
            }
        }
    }
    updates.insert(target, doc);
    write_updates(&fresh, updates).map_err(SaveError::Failed)
}

fn write_updates(snapshot: &Snapshot, updates: BTreeMap<PathBuf, Document>) -> Result<(), String> {
    let mut written = 0;
    for (path, doc) in updates {
        let expected = snapshot.files.get(&path).cloned().unwrap_or_default();
        if doc.text == expected.clone().unwrap_or_default() {
            continue;
        }
        atomic_write(&path, &doc.text, &expected).map_err(|e| {
            format!("{e}. {written} file(s) were updated before this failure; the display will be refreshed.")
        })?;
        written += 1;
    }
    Ok(())
}

pub fn reset(paths: &Paths) -> Result<(), String> {
    let _lock = lock(paths)?;
    let fresh = Snapshot::load(paths.clone())?;
    let mut updates = BTreeMap::new();
    for path in paths.user_files()? {
        let mut doc = fresh.document(&path);
        doc.clear_defaults();
        updates.insert(path, doc);
    }
    write_updates(&fresh, updates)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn fixture() -> (tempfile::TempDir, Paths) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_home: tmp.path().join("config"),
            data_home: tmp.path().join("data"),
            config_dirs: vec![tmp.path().join("etc")],
            data_dirs: vec![tmp.path().join("share")],
            desktops: vec!["kde".into()],
            locales: vec!["fr".into()],
        };
        (tmp, paths)
    }

    pub fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn xdg_empty_relative_and_missing_home() {
        let vars = BTreeMap::from([
            ("HOME", "/home/test"),
            ("XDG_CONFIG_HOME", ""),
            ("XDG_DATA_HOME", "relative"),
        ]);
        let paths = Paths::from_env(|k| vars.get(k).map(|s| s.to_string())).unwrap();
        assert_eq!(paths.config_home, PathBuf::from("/home/test/.config"));
        assert_eq!(paths.data_home, PathBuf::from("/home/test/.local/share"));
        let paths = Paths::from_env(|k| match k {
            "XDG_CONFIG_HOME" => Some("/config".into()),
            "XDG_DATA_HOME" => Some("/data".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(paths.data_home, PathBuf::from("/data"));
        assert!(Paths::from_env(|_| None).is_err());
    }

    #[test]
    fn candidates_follow_desktop_and_config_precedence() {
        let (_tmp, paths) = fixture();
        let candidates = paths.candidates();
        assert_eq!(
            candidates[0].path,
            paths.config_home.join("kde-mimeapps.list")
        );
        assert_eq!(candidates[1].path, paths.target());
        assert_eq!(
            candidates[2].path,
            paths.config_dirs[0].join("kde-mimeapps.list")
        );
        assert_eq!(
            candidates[4].path,
            paths.data_home.join("applications/kde-mimeapps.list")
        );
        assert!(
            candidates
                .iter()
                .all(|c| !c.path.to_string_lossy().contains("gnome"))
        );
    }

    #[test]
    fn lists_and_locales() {
        assert_eq!(
            list("a.desktop;b.desktop;a.desktop;"),
            vec!["a.desktop", "b.desktop"]
        );
        assert_eq!(
            locale_variants("sr_RS.UTF-8@latin"),
            vec!["sr_RS@latin", "sr_RS", "sr@latin", "sr"]
        );
    }

    #[test]
    fn editing_preserves_other_lines_comments_and_crlf() {
        let mut doc = Document { text: "# top\r\n[Default Applications]\r\n# note\r\na=a.desktop;\r\nb=b.desktop;\r\n[Added Associations]\r\nc=c.desktop;\r\n".into() };
        doc.set(DEFAULTS, "a", Some(&["new.desktop".into()]));
        assert_eq!(doc.text.matches("[Default Applications]").count(), 1);
        assert!(doc.text.starts_with("# top\r\n"));
        assert!(
            doc.text
                .contains("# note\r\na=new.desktop;\r\nb=b.desktop;")
        );
        assert!(
            doc.text
                .ends_with("[Added Associations]\r\nc=c.desktop;\r\n")
        );
    }

    #[test]
    fn insertion_goes_inside_existing_section() {
        let mut doc = Document {
            text: "[Default Applications]\na=a;\n[Other]\nb=b;\n".into(),
        };
        doc.set(DEFAULTS, "c", Some(&["c.desktop".into()]));
        assert_eq!(doc.values(DEFAULTS)["c"], vec!["c.desktop"]);
        assert!(!doc.values("Other").contains_key("c"));
    }

    #[test]
    fn targeted_save_preserves_external_and_inherited_values() {
        let (_tmp, paths) = fixture();
        put(
            &paths.target(),
            "# preserved\n[Default Applications]\na=a.desktop;\nb=b.desktop;\n",
        );
        put(
            &paths.data_dirs[0].join("applications/mimeapps.list"),
            "[Default Applications]\nsystem=system.desktop;\n",
        );
        let snapshot = Snapshot::load(paths.clone()).unwrap();
        put(
            &paths.target(),
            "# preserved\n[Default Applications]\na=a.desktop;\nb=external.desktop;\n",
        );
        save(
            &snapshot,
            &BTreeMap::from([("a".into(), Edit::Set(vec!["new.desktop".into()]))]),
            false,
        )
        .unwrap();
        let doc = Document {
            text: read(&paths.target()).unwrap().unwrap(),
        };
        assert_eq!(doc.values(DEFAULTS)["b"], vec!["external.desktop"]);
        assert!(!doc.values(DEFAULTS).contains_key("system"));
        assert_eq!(doc.values(ADDED)["a"], vec!["new.desktop"]);
    }

    #[test]
    fn same_key_external_change_requires_explicit_overwrite() {
        let (_tmp, paths) = fixture();
        put(&paths.target(), "[Default Applications]\na=a.desktop;\n");
        let snapshot = Snapshot::load(paths.clone()).unwrap();
        put(
            &paths.target(),
            "[Default Applications]\na=external.desktop;\n",
        );
        let edits = BTreeMap::from([("a".into(), Edit::Set(vec!["new.desktop".into()]))]);
        assert!(matches!(
            save(&snapshot, &edits, false),
            Err(SaveError::Conflict(_))
        ));
        save(&snapshot, &edits, true).unwrap();
        assert_eq!(
            Document {
                text: read(&paths.target()).unwrap().unwrap()
            }
            .values(DEFAULTS)["a"],
            vec!["new.desktop"]
        );
    }

    #[test]
    fn saving_removes_higher_priority_user_override_and_conflicting_removal() {
        let (_tmp, paths) = fixture();
        let desktop = paths.config_home.join("kde-mimeapps.list");
        put(
            &desktop,
            "[Default Applications]\na=old.desktop;\nb=keep.desktop;\n",
        );
        put(
            &paths.target(),
            "[Removed Associations]\na=new.desktop;other.desktop;\n",
        );
        let snapshot = Snapshot::load(paths.clone()).unwrap();
        save(
            &snapshot,
            &BTreeMap::from([("a".into(), Edit::Set(vec!["new.desktop".into()]))]),
            false,
        )
        .unwrap();
        let doc = Document {
            text: read(&desktop).unwrap().unwrap(),
        };
        assert!(!doc.values(DEFAULTS).contains_key("a"));
        assert_eq!(doc.values(DEFAULTS)["b"], vec!["keep.desktop"]);
        let doc = Document {
            text: read(&paths.target()).unwrap().unwrap(),
        };
        assert_eq!(doc.values(REMOVED)["a"], vec!["other.desktop"]);
    }

    #[test]
    fn reset_preserves_associations_and_includes_other_desktops() {
        let (_tmp, paths) = fixture();
        let other = paths.config_home.join("gnome-mimeapps.list");
        let text = "# top\n[Default Applications]\n# comment\na=a.desktop;\n[Added Associations]\na=b.desktop;\n[Removed Associations]\na=c.desktop;\n";
        put(&paths.target(), text);
        put(&other, text);
        reset(&paths).unwrap();
        for path in [paths.target(), other] {
            let doc = Document {
                text: read(&path).unwrap().unwrap(),
            };
            assert!(doc.values(DEFAULTS).is_empty());
            assert_eq!(doc.values(ADDED)["a"], vec!["b.desktop"]);
            assert_eq!(doc.values(REMOVED)["a"], vec!["c.desktop"]);
            assert!(doc.text.contains("# comment"));
        }
    }

    #[test]
    fn inherit_does_not_delete_associations() {
        let (_tmp, paths) = fixture();
        put(
            &paths.target(),
            "[Default Applications]\na=a.desktop;\n[Added Associations]\na=a.desktop;\n",
        );
        let snapshot = Snapshot::load(paths.clone()).unwrap();
        save(
            &snapshot,
            &BTreeMap::from([("a".into(), Edit::Inherit)]),
            false,
        )
        .unwrap();
        let doc = Document {
            text: read(&paths.target()).unwrap().unwrap(),
        };
        assert!(doc.values(DEFAULTS).is_empty());
        assert_eq!(doc.values(ADDED)["a"], vec!["a.desktop"]);
    }

    #[test]
    fn atomic_write_detects_changed_destination_and_preserves_permissions() {
        let (_tmp, paths) = fixture();
        put(&paths.target(), "old");
        fs::set_permissions(paths.target(), fs::Permissions::from_mode(0o640)).unwrap();
        assert!(atomic_write(&paths.target(), "new", &Some("different".into())).is_err());
        atomic_write(&paths.target(), "new", &Some("old".into())).unwrap();
        assert_eq!(
            fs::metadata(paths.target()).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn canonical_save_removes_higher_priority_alias_keys() {
        let (_tmp, paths) = fixture();
        put(
            &paths.data_home.join("mime/aliases"),
            "text/x-alias text/plain\n",
        );
        let desktop = paths.config_home.join("kde-mimeapps.list");
        put(
            &desktop,
            "[Default Applications]\ntext/x-alias=old.desktop;\n",
        );
        put(
            &paths.target(),
            "[Removed Associations]\ntext/x-alias=new.desktop;keep.desktop;\n",
        );
        let snapshot = Snapshot::load(paths.clone()).unwrap();
        save(
            &snapshot,
            &BTreeMap::from([("text/plain".into(), Edit::Set(vec!["new.desktop".into()]))]),
            false,
        )
        .unwrap();
        assert!(
            Document {
                text: read(&desktop).unwrap().unwrap()
            }
            .values(DEFAULTS)
            .is_empty()
        );
        let doc = Document {
            text: read(&paths.target()).unwrap().unwrap(),
        };
        assert_eq!(doc.values(DEFAULTS)["text/plain"], vec!["new.desktop"]);
        assert_eq!(doc.values(REMOVED)["text/x-alias"], vec!["keep.desktop"]);
    }

    #[test]
    fn no_op_edits_do_not_normalize_unrelated_files() {
        let mut doc = Document {
            text: "[Unrelated]\nvalue=keep".into(),
        };
        let original = doc.text.clone();
        doc.set(DEFAULTS, "missing", None);
        assert_eq!(doc.text, original);
        doc.clear_defaults();
        assert_eq!(doc.text, original);
    }

    #[test]
    fn all_relative_search_paths_use_standard_defaults() {
        assert_eq!(
            search_paths(Some("relative:also-relative".into()), "/etc/xdg"),
            vec![PathBuf::from("/etc/xdg")]
        );
    }

    #[test]
    fn unavailable_system_files_are_skipped_but_user_files_fail_closed() {
        let (_tmp, paths) = fixture();
        std::fs::create_dir_all(paths.config_dirs[0].join("mimeapps.list")).unwrap();
        assert!(Snapshot::load(paths.clone()).is_ok());
        std::fs::create_dir_all(paths.target()).unwrap();
        assert!(Snapshot::load(paths).is_err());
    }
}
