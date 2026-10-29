use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub mime_types: Vec<String>,
    pub icon_name: String,
    pub origin: usize,
}

#[derive(Clone, Debug)]
pub struct MimeEntry {
    pub mime: String,
    pub name: String,
    pub choices: Vec<String>,
    pub original: Vec<String>,
    pub associated: Vec<String>,
    pub effective: Option<String>,
    pub user_override: bool,
    pub inherit: bool,
}

impl MimeEntry {
    pub fn changed(&self) -> bool {
        self.inherit || self.choices != self.original
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Set(Vec<String>),
    Inherit,
}

pub type Edits = BTreeMap<String, Edit>;

pub fn edits(entries: &[MimeEntry]) -> Edits {
    entries
        .iter()
        .filter(|entry| entry.changed())
        .map(|entry| {
            (
                entry.mime.clone(),
                if entry.inherit {
                    Edit::Inherit
                } else {
                    Edit::Set(entry.choices.clone())
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_actual_edits_are_dirty_and_reverting_cleans_them() {
        let mut entry = MimeEntry {
            mime: "text/plain".into(),
            name: "Text".into(),
            choices: vec!["a.desktop".into()],
            original: vec!["a.desktop".into()],
            associated: vec![],
            effective: None,
            user_override: false,
            inherit: false,
        };
        assert!(edits(&[entry.clone()]).is_empty());
        entry.choices.push("b.desktop".into());
        assert_eq!(edits(&[entry.clone()]).len(), 1);
        entry.choices.pop();
        assert!(edits(&[entry.clone()]).is_empty());
        entry.inherit = true;
        assert_eq!(edits(&[entry])["text/plain"], Edit::Inherit);
    }
}
