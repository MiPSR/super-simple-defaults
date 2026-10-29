use crate::config::{self, ADDED, DEFAULTS, Paths, REMOVED, Snapshot};
use crate::model::{AppEntry, MimeEntry};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Data {
    pub snapshot: Snapshot,
    pub apps: Vec<AppEntry>,
    pub entries: Vec<MimeEntry>,
}

/// Canonical-directory tracking allows Nix/Flatpak symlinks without following cycles.
pub fn walk(root: &Path, extension: &str) -> Vec<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    let mut visited = HashSet::new();
    let mut files = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(real) = fs::canonicalize(&dir) else {
            continue;
        };
        if !visited.insert(real) {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == extension) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn unescape(value: &str) -> String {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                result.push(match next {
                    's' => ' ',
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    _ => next,
                });
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn desktop_values(text: &str) -> BTreeMap<String, String> {
    let mut inside = false;
    let mut values = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
        } else if inside
            && !line.starts_with('#')
            && let Some((key, value)) = line.split_once('=')
        {
            values.insert(key.trim().into(), value.trim().into());
        }
    }
    values
}

fn executable(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let check = |p: &Path| {
        p.is_file() && fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    };
    if program.contains('/') {
        return Path::new(program).is_absolute() && check(Path::new(program));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| check(&dir.join(program)))
}

fn parse_desktop(text: &str, id: String, origin: usize, locales: &[String]) -> Option<AppEntry> {
    let values = desktop_values(text);
    let get = |key: &str| values.get(key).map(String::as_str).unwrap_or("");
    if get("Hidden").eq_ignore_ascii_case("true") || get("Type") != "Application" {
        return None;
    }
    if get("Exec").is_empty() && !get("DBusActivatable").eq_ignore_ascii_case("true") {
        return None;
    }
    if !get("TryExec").is_empty() && !executable(&unescape(get("TryExec"))) {
        return None;
    }
    let name = locales
        .iter()
        .find_map(|locale| values.get(&format!("Name[{locale}]")))
        .or_else(|| values.get("Name"))
        .map(|s| unescape(s))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| id.trim_end_matches(".desktop").into());
    // NoDisplay is intentionally accepted: these apps can still handle files.
    Some(AppEntry {
        id,
        name,
        origin,
        mime_types: config::list(get("MimeType")),
        icon_name: unescape(get("Icon")),
    })
}

pub fn enumerate_apps(paths: &Paths) -> Vec<AppEntry> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    for (origin, base) in paths.data_roots().iter().enumerate() {
        let root = base.join("applications");
        for path in walk(&root, "desktop") {
            let Ok(relative) = path.strip_prefix(&root) else {
                continue;
            };
            let id = relative.to_string_lossy().replace('/', "-");
            // Record before parsing: a hidden or unusable higher-priority entry
            // must mask all lower-priority entries with this desktop ID.
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path)
                && let Some(app) = parse_desktop(&text, id, origin, &paths.locales)
            {
                apps.push(app);
            }
        }
    }
    apps.sort_by_cached_key(|a| (a.name.to_lowercase(), a.id.clone()));
    apps
}

#[derive(Default)]
struct MimeDb {
    names: BTreeMap<String, String>,
    aliases: BTreeMap<String, String>,
    parents: BTreeMap<String, Vec<String>>,
}

impl MimeDb {
    fn load(paths: &Paths) -> Self {
        let mut db = Self::default();
        for base in paths.data_roots() {
            let root = base.join("mime");
            if let Ok(text) = fs::read_to_string(root.join("aliases")) {
                for line in text.lines() {
                    let mut words = line.split_whitespace();
                    if let (Some(alias), Some(canonical)) = (words.next(), words.next()) {
                        db.aliases
                            .entry(alias.into())
                            .or_insert_with(|| canonical.into());
                    }
                }
            }
            if let Ok(text) = fs::read_to_string(root.join("subclasses")) {
                for line in text.lines() {
                    let mut words = line.split_whitespace();
                    if let (Some(child), Some(parent)) = (words.next(), words.next()) {
                        let parents = db.parents.entry(child.into()).or_default();
                        if !parents.iter().any(|p| p == parent) {
                            parents.push(parent.into());
                        }
                    }
                }
            }
            let Ok(media) = fs::read_dir(&root) else {
                continue;
            };
            for dir in media.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                if dir.file_name().is_some_and(|n| n == "packages") {
                    continue;
                }
                let Ok(files) = fs::read_dir(dir) else {
                    continue;
                };
                for file in files
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "xml"))
                {
                    let Ok(xml) = fs::read_to_string(file) else {
                        continue;
                    };
                    let Ok(doc) = roxmltree::Document::parse(&xml) else {
                        continue;
                    };
                    let Some(node) = doc.descendants().find(|n| n.has_tag_name("mime-type")) else {
                        continue;
                    };
                    let Some(mime) = node.attribute("type") else {
                        continue;
                    };
                    let comments: Vec<_> = node
                        .children()
                        .filter(|n| n.has_tag_name("comment"))
                        .collect();
                    let localized = paths.locales.iter().find_map(|locale| {
                        comments.iter().find(|n| {
                            n.attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
                                .is_some_and(|lang| lang.replace('-', "_") == *locale)
                        })
                    });
                    let name = localized
                        .or_else(|| {
                            comments.iter().find(|n| {
                                n.attribute(("http://www.w3.org/XML/1998/namespace", "lang"))
                                    .is_none()
                            })
                        })
                        .and_then(|n| n.text())
                        .unwrap_or(mime)
                        .to_string();
                    db.names.entry(mime.into()).or_insert(name);
                }
            }
        }
        db
    }

    fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        let mut current = mime;
        let mut seen = HashSet::new();
        while seen.insert(current) {
            match self.aliases.get(current) {
                Some(next) => current = next,
                None => break,
            }
        }
        current
    }

    fn lineage(&self, mime: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut queue = std::collections::VecDeque::from([self.canonical(mime).to_string()]);
        while let Some(current) = queue.pop_front() {
            if out.contains(&current) {
                continue;
            }
            if let Some(parents) = self.parents.get(&current) {
                queue.extend(parents.iter().map(|p| self.canonical(p).to_string()));
            }
            out.push(current);
        }
        out
    }
}

struct Layer {
    user: bool,
    origin: Option<usize>,
    desktop_specific: bool,
    defaults: BTreeMap<String, Vec<String>>,
    added: BTreeMap<String, Vec<String>>,
    removed: BTreeMap<String, Vec<String>>,
}

fn layers(snapshot: &Snapshot, db: &MimeDb) -> Vec<Layer> {
    snapshot
        .candidates
        .iter()
        .map(|candidate| {
            let doc = snapshot.document(&candidate.path);
            let canonicalize = |values: BTreeMap<String, Vec<String>>| {
                values
                    .into_iter()
                    .map(|(k, v)| (db.canonical(&k).to_string(), v))
                    .collect()
            };
            Layer {
                user: candidate.user,
                origin: candidate.origin,
                desktop_specific: candidate.desktop_specific,
                defaults: canonicalize(doc.values(DEFAULTS)),
                added: canonicalize(doc.values(ADDED)),
                removed: canonicalize(doc.values(REMOVED)),
            }
        })
        .collect()
}

fn associated(mime: &str, apps: &[AppEntry], layers: &[Layer], db: &MimeDb) -> Vec<String> {
    let mut result = Vec::new();
    for kind in db.lineage(mime) {
        for id in associated_exact(&kind, apps, layers, db) {
            if !result.contains(&id) {
                result.push(id);
            }
        }
    }
    result
}

fn associated_exact(kind: &str, apps: &[AppEntry], layers: &[Layer], db: &MimeDb) -> Vec<String> {
    let mut result = Vec::new();
    let mut blocked = HashSet::new();
    for layer in layers.iter().filter(|l| !l.desktop_specific) {
        let applicable = |app: &AppEntry| layer.origin.is_none_or(|origin| app.origin >= origin);
        if let Some(added) = layer.added.get(kind) {
            for id in added {
                if !blocked.contains(id)
                    && !result.contains(id)
                    && apps.iter().any(|a| a.id == *id && applicable(a))
                {
                    result.push(id.clone());
                }
            }
        }
        if let Some(removed) = layer.removed.get(kind) {
            for id in removed {
                if apps.iter().any(|a| a.id == *id && applicable(a)) {
                    blocked.insert(id.clone());
                }
            }
        }
        if let Some(origin) = layer.origin {
            for app in apps.iter().filter(|a| a.origin == origin) {
                if !blocked.contains(&app.id)
                    && !result.contains(&app.id)
                    && app.mime_types.iter().any(|m| db.canonical(m) == kind)
                {
                    result.push(app.id.clone());
                }
                blocked.insert(app.id.clone());
            }
        }
    }
    result
}

fn entry(
    mime: String,
    name: String,
    apps: &[AppEntry],
    layers: &[Layer],
    db: &MimeDb,
) -> MimeEntry {
    let associated = associated(&mime, apps, layers, db);
    let mut choices = Vec::new();
    let mut effective = None;
    let user_override = layers
        .iter()
        .any(|l| l.user && l.defaults.contains_key(&mime));
    'types: for kind in db.lineage(&mime) {
        let supporting = self::associated(&kind, apps, layers, db);
        for layer in layers {
            if let Some(ids) = layer.defaults.get(&kind)
                && let Some(id) = ids.iter().find(|id| supporting.contains(id))
            {
                choices = ids.clone();
                effective = Some(id.clone());
                break 'types;
            }
        }
        // Without an explicit usable default, the first preferred association
        // wins before falling back to a less-specific type.
        if let Some(id) = associated_exact(&kind, apps, layers, db).first() {
            effective = Some(id.clone());
            break;
        }
    }
    MimeEntry {
        mime,
        name,
        original: choices.clone(),
        choices,
        associated,
        effective,
        user_override,
        inherit: false,
    }
}

pub fn load(paths: Paths) -> Result<Data, String> {
    let snapshot = Snapshot::load(paths.clone())?;
    let apps = enumerate_apps(&paths);
    let mut db = MimeDb::load(&paths);
    for app in &apps {
        for mime in &app.mime_types {
            let canonical = db.canonical(mime).to_string();
            db.names.entry(canonical.clone()).or_insert(canonical);
        }
    }
    let layers = layers(&snapshot, &db);
    let mut names = BTreeMap::new();
    for (mime, name) in &db.names {
        let canonical = db.canonical(mime).to_string();
        if canonical == *mime {
            names.insert(canonical, name.clone());
        } else {
            names.entry(canonical).or_insert_with(|| name.clone());
        }
    }
    for layer in &layers {
        for mime in layer
            .defaults
            .keys()
            .chain(layer.added.keys())
            .chain(layer.removed.keys())
        {
            names.entry(mime.clone()).or_insert_with(|| mime.clone());
        }
    }
    let mut entries: Vec<_> = names
        .into_iter()
        .map(|(mime, name)| entry(mime, name, &apps, &layers, &db))
        .collect();
    entries.sort_by_cached_key(|e| (e.name.to_lowercase(), e.mime.clone()));
    Ok(Data {
        snapshot,
        apps,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn fixture() -> (tempfile::TempDir, Paths) {
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

    fn put(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn app(paths: &Paths, name: &str, mime: &str) {
        put(
            &paths.data_dirs[0].join("applications").join(name),
            &format!(
                "[Desktop Entry]\nType=Application\nName={name}\nExec=example\nMimeType={mime};\n"
            ),
        );
    }

    #[test]
    fn nested_ids_hidden_masking_and_directory_cycles() {
        let (_tmp, paths) = fixture();
        app(&paths, "vendor/editor.desktop", "text/plain");
        app(&paths, "hidden.desktop", "text/plain");
        put(
            &paths.data_home.join("applications/hidden.desktop"),
            "[Desktop Entry]\nHidden=true\n",
        );
        let root = paths.data_dirs[0].join("applications");
        symlink(&root, root.join("loop")).unwrap();
        let apps = enumerate_apps(&paths);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "vendor-editor.desktop");
    }

    #[test]
    fn localized_escaped_names_and_dbus_activation() {
        let app = parse_desktop("[Desktop Entry]\nType=Application\nName=Default\nName[fr]=Mon\\séditeur\nDBusActivatable=true\nNoDisplay=true\n", "a.desktop".into(), 0, &["fr".into()]).unwrap();
        assert_eq!(app.name, "Mon éditeur");
    }

    #[test]
    fn default_lists_skip_missing_and_unassociated_apps() {
        let (_tmp, paths) = fixture();
        app(&paths, "reader.desktop", "application/pdf");
        app(&paths, "other.desktop", "image/png");
        put(
            &paths.target(),
            "[Default Applications]\napplication/pdf=missing.desktop;other.desktop;reader.desktop;\n",
        );
        let data = load(paths).unwrap();
        let e = data
            .entries
            .iter()
            .find(|e| e.mime == "application/pdf")
            .unwrap();
        assert_eq!(e.effective.as_deref(), Some("reader.desktop"));
        assert_eq!(e.choices.len(), 3);
    }

    #[test]
    fn unusable_higher_layer_falls_through_to_desktop_default() {
        let (_tmp, paths) = fixture();
        app(&paths, "a.desktop", "text/plain");
        app(&paths, "b.desktop", "text/plain");
        put(
            &paths.target(),
            "[Default Applications]\ntext/plain=missing.desktop;\n",
        );
        put(
            &paths.config_dirs[0].join("kde-mimeapps.list"),
            "[Default Applications]\ntext/plain=b.desktop;\n",
        );
        let data = load(paths).unwrap();
        assert_eq!(
            data.entries
                .iter()
                .find(|e| e.mime == "text/plain")
                .unwrap()
                .effective
                .as_deref(),
            Some("b.desktop")
        );
    }

    #[test]
    fn added_and_removed_associations_affect_resolution() {
        let (_tmp, paths) = fixture();
        app(&paths, "a.desktop", "text/plain");
        app(&paths, "b.desktop", "image/png");
        put(
            &paths.target(),
            "[Default Applications]\ntext/plain=a.desktop;b.desktop;\n[Added Associations]\ntext/plain=b.desktop;\n[Removed Associations]\ntext/plain=a.desktop;\n",
        );
        let data = load(paths).unwrap();
        let e = data
            .entries
            .iter()
            .find(|e| e.mime == "text/plain")
            .unwrap();
        assert_eq!(e.effective.as_deref(), Some("b.desktop"));
        assert_eq!(e.associated, vec!["b.desktop"]);
    }

    #[test]
    fn higher_desktop_file_ignores_lower_association_changes() {
        let (_tmp, paths) = fixture();
        app(&paths, "a.desktop", "text/plain");
        put(
            &paths.data_home.join("applications/a.desktop"),
            "[Desktop Entry]\nType=Application\nName=A\nExec=a\nMimeType=text/plain;\n",
        );
        put(
            &paths.data_dirs[0].join("applications/mimeapps.list"),
            "[Removed Associations]\ntext/plain=a.desktop;\n",
        );
        let data = load(paths).unwrap();
        assert_eq!(
            data.entries
                .iter()
                .find(|e| e.mime == "text/plain")
                .unwrap()
                .associated,
            vec!["a.desktop"]
        );
    }

    #[test]
    fn mime_xml_is_localized_and_entities_are_decoded() {
        let (_tmp, paths) = fixture();
        put(
            &paths.data_home.join("mime/text/plain.xml"),
            "<mime-type type=\"text/plain\"><comment>Text</comment><comment xml:lang=\"fr\">Texte &amp; notes</comment></mime-type>",
        );
        let data = load(paths).unwrap();
        assert_eq!(data.entries[0].name, "Texte & notes");
        assert!(data.entries[0].choices.is_empty());
    }

    #[test]
    fn alias_and_parent_support_are_resolved_without_cycles() {
        let (_tmp, paths) = fixture();
        app(&paths, "a.desktop", "text/plain");
        put(
            &paths.data_home.join("mime/aliases"),
            "text/x-alias text/x-child\n",
        );
        put(
            &paths.data_home.join("mime/subclasses"),
            "text/x-child text/plain\ntext/plain text/x-child\n",
        );
        put(
            &paths.target(),
            "[Default Applications]\ntext/x-alias=a.desktop;\n",
        );
        let data = load(paths).unwrap();
        let e = data
            .entries
            .iter()
            .find(|e| e.mime == "text/x-child")
            .unwrap();
        assert_eq!(e.effective.as_deref(), Some("a.desktop"));
    }

    #[test]
    fn parent_explicit_default_is_used_when_child_has_no_specific_handler() {
        let (_tmp, paths) = fixture();
        app(&paths, "a.desktop", "text/plain");
        app(&paths, "b.desktop", "text/plain");
        put(
            &paths.data_home.join("mime/subclasses"),
            "text/x-child text/plain\n",
        );
        put(
            &paths.data_home.join("mime/text/x-child.xml"),
            "<mime-type type=\"text/x-child\"><comment>Child</comment></mime-type>",
        );
        put(
            &paths.target(),
            "[Default Applications]\ntext/plain=b.desktop;\n",
        );
        let data = load(paths).unwrap();
        let e = data
            .entries
            .iter()
            .find(|e| e.mime == "text/x-child")
            .unwrap();
        assert_eq!(e.effective.as_deref(), Some("b.desktop"));
    }

    #[test]
    fn aliases_do_not_create_duplicate_editable_rows() {
        let (_tmp, paths) = fixture();
        put(
            &paths.data_home.join("mime/aliases"),
            "text/x-alias text/plain\n",
        );
        put(
            &paths.data_home.join("mime/text/x-alias.xml"),
            "<mime-type type=\"text/x-alias\"><comment>Alias</comment></mime-type>",
        );
        put(
            &paths.data_home.join("mime/text/plain.xml"),
            "<mime-type type=\"text/plain\"><comment>Canonical</comment></mime-type>",
        );
        let data = load(paths).unwrap();
        assert_eq!(data.entries.len(), 1);
        assert_eq!(data.entries[0].mime, "text/plain");
        assert_eq!(data.entries[0].name, "Canonical");
    }
}
