mod mime;
mod model;

use crate::model::*;
use iced::widget::{button, column, container, pick_list, row, scrollable, text, Space};
use iced::{Element, Length, Task};
use std::collections::{BTreeMap, HashMap, HashSet};

fn main() -> iced::Result {
    let state = build_state();
    iced::application("Default Applications", update, view)
        .settings(iced::Settings::default())
        .theme(|s: &State| if s.dark { iced::Theme::Dark } else { iced::Theme::Light })
        .run_with(move || (state, Task::none()))
}

fn detect_dark() -> bool {
    // Try freedesktop portal (works on Wayland/Niri if a portal backend is present).
    if let Ok(out) = std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings.Read",
            "string:org.freedesktop.appearance",
            "string:color-scheme",
        ])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        if s.contains("uint32 1") {
            return true;
        }
        if s.contains("uint32 2") {
            return false;
        }
    }
    // Fallback: GNOME gsettings.
    if let Ok(out) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        if s.contains("prefer-dark") {
            return true;
        }
        if s.contains("prefer-light") {
            return false;
        }
    }
    // Fallback: env hint.
    if let Ok(v) = std::env::var("GTK_THEME") {
        if v.to_lowercase().contains("dark") {
            return true;
        }
        if v.to_lowercase().contains("light") {
            return false;
        }
    }
    // Default dark.
    true
}

fn build_state() -> State {
    let mimes = mime::enumerate_mimes();
    let apps = mime::enumerate_apps();

    // Collect any MIME types referenced by apps but missing from the mime db
    // (e.g. x-scheme-handler/http, mailto, tel) so every case is covered.
    let mut mime_name: HashMap<String, String> = HashMap::new();
    for m in &mimes {
        mime_name.insert(m.mime.clone(), m.name.clone());
    }
    for a in &apps {
        for mt in &a.mime_types {
            mime_name.entry(mt.clone()).or_insert_with(|| mt.clone());
        }
    }
    let apps_by_id: HashMap<String, AppEntry> = apps.iter().map(|a| (a.id.clone(), a.clone())).collect();

    // group mimes by media (top-level before '/')
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for mtype in mime_name.keys() {
        let media = mtype.split('/').next().unwrap_or("other").to_string();
        groups.entry(media).or_default().push(mtype.clone());
    }

    let current = mime::read_mimeapps();

    // Build categories
    let mut categories: Vec<CategoryState> = Vec::new();
    for (media, mime_list) in groups {
        // skip empty
        if mime_list.is_empty() {
            continue;
        }
        // sort mimes
        let mut mimes_sorted = mime_list.clone();
        mimes_sorted.sort();

        // infer common default
        let defaults: Vec<Option<String>> = mimes_sorted
            .iter()
            .map(|m| current.get(m).cloned())
            .collect();
        // common app among Some
        let mut common: Option<String> = None;
        let some_vals: Vec<&String> = defaults.iter().filter_map(|x| x.as_ref()).collect();
        if !some_vals.is_empty() {
            let first = some_vals[0];
            if some_vals.iter().all(|x| *x == first) {
                common = Some(first.clone());
            }
        }

        let mut overrides: HashMap<String, SubChoice> = HashMap::new();
        for (i, m) in mimes_sorted.iter().enumerate() {
            let cur = &defaults[i];
            if common.is_some() && cur.as_ref() == common.as_ref() {
                overrides.insert(m.clone(), SubChoice::Inherit);
            } else {
                match cur {
                    Some(id) => overrides.insert(m.clone(), SubChoice::App(id.clone(), app_name(&apps_by_id, id))),
                    None => overrides.insert(m.clone(), SubChoice::Inherit),
                };
            }
        }

        let main = match &common {
            Some(id) => Choice::App(id.clone(), app_name(&apps_by_id, id)),
            None => Choice::None,
        };

        let mut mime_names = HashMap::new();
        for m in &mimes_sorted {
            if let Some(n) = mime_name.get(m) {
                mime_names.insert(m.clone(), n.clone());
            }
        }

        categories.push(CategoryState {
            label: media_label(&media),
            mimes: mimes_sorted,
            mime_names,
            main,
            overrides,
        });
    }

    // sort categories by label
    categories.sort_by(|a, b| a.label.cmp(&b.label));

    State {
        apps,
        categories,
        status: String::new(),
        dirty: false,
        dark: detect_dark(),
    }
}

fn app_name(map: &HashMap<String, AppEntry>, id: &str) -> String {
    map.get(id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| id.to_string())
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::SetMain(cat_idx, choice) => {
            if let Some(cat) = state.categories.get_mut(cat_idx) {
                cat.main = choice;
                state.dirty = true;
            }
        }
        Message::SetSub(mime, choice) => {
            for cat in &mut state.categories {
                if cat.mimes.contains(&mime) {
                    cat.overrides.insert(mime.clone(), choice.clone());
                    state.dirty = true;
                }
            }
        }
        Message::Save => {
            let mut map: BTreeMap<String, String> = BTreeMap::new();
            // start from current file to preserve unknowns? We'll rebuild from state only, but keep unknown from read? 
            // Read current to preserve entries not in our categories.
            let current = mime::read_mimeapps();
            let known: HashSet<String> = state
                .categories
                .iter()
                .flat_map(|c| c.mimes.iter().cloned())
                .collect();
            for (k, v) in &current {
                if !known.contains(k) {
                    map.insert(k.clone(), v.clone());
                }
            }
            for cat in &state.categories {
                for m in &cat.mimes {
                    let ov = cat.overrides.get(m).cloned().unwrap_or(SubChoice::Inherit);
                    let eff = match ov {
                        SubChoice::Inherit => match &cat.main {
                            Choice::App(id, _) => Some(id.clone()),
                            Choice::None => None,
                        },
                        SubChoice::None => None,
                        SubChoice::App(id, _) => Some(id.clone()),
                    };
                    if let Some(id) = eff {
                        map.insert(m.clone(), id);
                    }
                }
            }
            match mime::write_mimeapps(&map) {
                Ok(()) => {
                    state.status = "Saved.".to_string();
                    state.dirty = false;
                }
                Err(e) => {
                    state.status = format!("Save failed: {}", e);
                }
            }
        }
        Message::Reload => {
            *state = build_state();
        }
    }
    Task::none()
}

#[derive(Debug, Clone)]
enum Message {
    SetMain(usize, Choice),
    SetSub(String, SubChoice),
    Save,
    Reload,
}

fn view<'a>(state: &'a State) -> Element<'a, Message> {
    let title = text("Default Applications").size(24);

    let mut save_btn = button("Save");
    if state.dirty {
        save_btn = save_btn.on_press(Message::Save);
    }
    let reload_btn = button("Reload").on_press(Message::Reload);

    let status = text(&state.status);

    let header = row![title, Space::with_width(Length::Fill), save_btn, reload_btn, status]
        .spacing(8)
        .align_y(iced::Alignment::Center);

    // categories
    let mut cats_col = column![].spacing(12);
    for (idx, cat) in state.categories.iter().enumerate() {
        cats_col = cats_col.push(category_view(state, idx, cat));
    }

    let body = scrollable(cats_col).height(Length::Fill);

    column![header, body].spacing(12).padding(12).into()
}

fn category_view<'a>(state: &'a State, idx: usize, cat: &'a CategoryState) -> Element<'a, Message> {
    let header = text(&cat.label).size(18);

    // main dropdown
    let main_options: Vec<Choice> = {
        let mut v = vec![Choice::None];
        let apps = state.apps_for_category(cat);
        for a in apps {
            v.push(Choice::App(a.id.clone(), a.name.clone()));
        }
        v
    };
    let main_pick = pick_list(main_options, Some(cat.main.clone()), move |c| {
        Message::SetMain(idx, c)
    });

    let main_row = row![text("Main default:"), main_pick].spacing(8);

    // subelements
    let capable_apps_cache = state.apps_for_category(cat);

    let mut sub_col = column![].spacing(4);
    for m in &cat.mimes {
        let name = cat.mime_names.get(m).cloned().unwrap_or_else(|| m.clone());
        let label = text(format!("{} ({})", name, m)).size(13);

        // options for this mime
        let mut opts: Vec<SubChoice> = vec![SubChoice::Inherit, SubChoice::None];
        let mime_apps = state.apps_for_mime(m);
        // if no specific apps, fall back to category apps
        let list = if mime_apps.is_empty() {
            capable_apps_cache.clone()
        } else {
            mime_apps
        };
        for a in list {
            opts.push(SubChoice::App(a.id.clone(), a.name.clone()));
        }
        let current = cat.overrides.get(m).cloned().unwrap_or(SubChoice::Inherit);
        let mime_clone = m.clone();
        let pick = pick_list(opts, Some(current), move |c| {
            Message::SetSub(mime_clone.clone(), c)
        });
        sub_col = sub_col.push(row![label, Space::with_width(Length::Fill), pick].spacing(8));
    }

    let sub_scroll = scrollable(sub_col).height(Length::Fixed(300.0));

    let card = container(column![header, main_row, sub_scroll].spacing(8))
        .padding(12)
        .width(Length::Fill);

    card.into()
}