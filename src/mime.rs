use crate::model::{AppEntry, MimeEntry};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let xdg_data_home = std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{}/.local/share", home));
        dirs.push(PathBuf::from(xdg_data_home));
    }
    let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for d in xdg_data_dirs.split(':') {
        if !d.is_empty() {
            dirs.push(PathBuf::from(d));
        }
    }
    // NixOS system sw
    dirs.push(PathBuf::from("/run/current-system/sw/share"));
    dirs
}

fn config_home() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(x)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config")
    } else {
        PathBuf::from(".config")
    }
}

pub fn mimeapps_path() -> PathBuf {
    config_home().join("mimeapps.list")
}

fn first_comment(xml: &str) -> Option<String> {
    // find <comment> without xml:lang, or first <comment>
    let mut default_comment: Option<String> = None;
    let mut any_comment: Option<String> = None;
    let mut rest = xml;
    while let Some(start) = rest.find("<comment") {
        let after = &rest[start..];
        // find end of tag
        let tag_end = after.find('>')?;
        let tag = &after[..=tag_end];
        let content_start = start + tag_end + 1;
        let content_end = rest[content_start..].find("</comment>")? + content_start;
        let content = rest[content_start..content_end].trim().to_string();
        if !tag.contains("xml:lang") && !tag.contains("lang=") {
            default_comment = Some(content.clone());
        }
        if any_comment.is_none() {
            any_comment = Some(content);
        }
        rest = &rest[content_end + "</comment>".len()..];
    }
    default_comment.or(any_comment)
}

pub fn enumerate_mimes() -> Vec<MimeEntry> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<MimeEntry> = Vec::new();
    for base in data_dirs() {
        let mime_dir = base.join("mime");
        if !mime_dir.is_dir() {
            continue;
        }
        // walk subdirs (media types), each containing *.xml
        if let Ok(entries) = fs::read_dir(&mime_dir) {
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                let media_name = p.file_name().unwrap_or_default().to_string_lossy();
                if media_name == "packages" {
                    continue;
                }
                if let Ok(files) = fs::read_dir(&p) {
                    for f in files.flatten() {
                        let fp = f.path();
                        if fp.extension().map(|x| x == "xml").unwrap_or(false) {
                            if let Ok(content) = fs::read_to_string(&fp) {
                                // type from filename? parse type attribute
                                let mime = if let Some(i) = content.find("type=\"") {
                                    let s = &content[i + 6..];
                                    if let Some(j) = s.find('"') {
                                        s[..j].to_string()
                                    } else {
                                        continue;
                                    }
                                } else {
                                    // fallback: media/filename stem
                                    format!("{}/{}", media_name, fp.file_stem().unwrap().to_string_lossy())
                                };
                                if seen.insert(mime.clone()) {
                                    let name = first_comment(&content).unwrap_or_else(|| mime.clone());
                                    out.push(MimeEntry { mime, name });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // Also include mime types referenced by desktop files (x-scheme-handler etc may already be covered)
    out.sort_by(|a, b| a.mime.cmp(&b.mime));
    out
}

fn parse_desktop(path: &Path) -> Option<AppEntry> {
    let content = fs::read_to_string(path).ok()?;
    let mut in_entry = false;
    let mut typev = String::new();
    let mut name = String::new();
    let mut exec = String::new();
    let mut mime_types: Vec<String> = Vec::new();
    let mut hidden = false;
    let mut no_display = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            if line == "[Desktop Entry]" {
                in_entry = true;
            } else {
                in_entry = false;
            }
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            match k {
                "Type" => typev = v.to_string(),
                "Name" => {
                    if name.is_empty() {
                        name = v.to_string();
                    }
                }
                "Exec" => exec = v.to_string(),
                "MimeType" => {
                    for m in v.split(';') {
                        let m = m.trim();
                        if !m.is_empty() {
                            mime_types.push(m.to_string());
                        }
                    }
                }
                "Hidden" => hidden = v.eq_ignore_ascii_case("true"),
                "NoDisplay" => no_display = v.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
    }
    if typev != "Application" {
        return None;
    }
    if hidden {
        return None;
    }
    if exec.is_empty() {
        return None;
    }
    if name.is_empty() {
        name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string());
    }
    let id = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    // no_display apps still included (terminals etc.)
    let _ = no_display;
    Some(AppEntry {
        id,
        name,
        mime_types,
    })
}

pub fn enumerate_apps() -> Vec<AppEntry> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<AppEntry> = Vec::new();
    // user dirs first so they take precedence
    let mut dirs = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let xdg_data_home = std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{}/.local/share", home));
        dirs.push(PathBuf::from(xdg_data_home));
    }
    let xdg_data_dirs = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for d in xdg_data_dirs.split(':') {
        if !d.is_empty() {
            dirs.push(PathBuf::from(d));
        }
    }
    dirs.push(PathBuf::from("/run/current-system/sw/share"));

    for base in dirs {
        let app_dir = base.join("applications");
        if !app_dir.is_dir() {
            continue;
        }
        // walk recursively
        let mut stack = vec![app_dir];
        while let Some(dir) = stack.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                        continue;
                    }
                    if p.extension().map(|x| x == "desktop").unwrap_or(false) {
                        if let Some(app) = parse_desktop(&p) {
                            if seen.insert(app.id.clone()) {
                                out.push(app);
                            }
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn read_mimeapps() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let path = mimeapps_path();
    if let Ok(content) = fs::read_to_string(&path) {
        let mut in_default = false;
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_default = t == "[Default Applications]";
                continue;
            }
            if in_default {
                if let Some((k, v)) = t.split_once('=') {
                    let k = k.trim();
                    let v = v.trim();
                    if !k.is_empty() && !v.is_empty() {
                        map.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }
    }
    // also user data dir mimeapps.list
    if let Ok(home) = std::env::var("HOME") {
        let xdg_data_home = std::env::var("XDG_DATA_HOME")
            .unwrap_or_else(|_| format!("{}/.local/share", home));
        let p = PathBuf::from(xdg_data_home).join("applications/mimeapps.list");
        if let Ok(content) = fs::read_to_string(&p) {
            let mut in_default = false;
            for line in content.lines() {
                let t = line.trim();
                if t.starts_with('[') {
                    in_default = t == "[Default Applications]";
                    continue;
                }
                if in_default {
                    if let Some((k, v)) = t.split_once('=') {
                        let k = k.trim();
                        let v = v.trim();
                        if !k.is_empty() && !v.is_empty() && !map.contains_key(k) {
                            map.insert(k.to_string(), v.to_string());
                        }
                    }
                }
            }
        }
    }
    map
}

/// Preserve non-Default-Applications sections from existing file.
fn read_other_sections() -> Vec<(String, Vec<String>)> {
    let path = mimeapps_path();
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if let Some(sec) = current.take() {
                sections.push(sec);
            }
            current = Some((t.to_string(), Vec::new()));
        } else if let Some(sec) = current.as_mut() {
            if sec.0 != "[Default Applications]" {
                sec.1.push(line.to_string());
            }
        }
    }
    if let Some(sec) = current.take() {
        if sec.0 != "[Default Applications]" {
            sections.push(sec);
        }
    }
    sections
}

pub fn write_mimeapps(defaults: &BTreeMap<String, String>) -> Result<(), String> {
    let path = mimeapps_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut out = String::new();
    out.push_str("[Default Applications]\n");
    for (k, v) in defaults {
        out.push_str(&format!("{}={}\n", k, v));
    }
    out.push('\n');
    for (name, lines) in read_other_sections() {
        out.push_str(&format!("{}\n", name));
        for l in lines {
            out.push_str(&format!("{}\n", l));
        }
        out.push('\n');
    }
    let tmp = path.with_extension("list.tmp");
    fs::write(&tmp, out).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}