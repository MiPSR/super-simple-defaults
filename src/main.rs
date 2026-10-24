mod mime;
mod model;

use crate::model::*;
use iced::widget::{button, column, container, image, row, scrollable, svg, text, text_input, Space};
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
    if let Ok(v) = std::env::var("GTK_THEME") {
        if v.to_lowercase().contains("dark") {
            return true;
        }
        if v.to_lowercase().contains("light") {
            return false;
        }
    }
    true
}

fn build_state() -> State {
    let mimes = mime::enumerate_mimes();
    let apps = mime::enumerate_apps();

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

    let current = mime::read_mimeapps();

    let mut entries: Vec<MimeDefaultState> = mime_name
        .iter()
        .map(|(m, n)| {
            let cur = current.get(m).cloned();
            let default = match cur {
                Some(id) => Choice::App(id.clone(), app_name(&apps_by_id, &id)),
                None => Choice::None,
            };
            MimeDefaultState {
                mime: m.clone(),
                name: n.clone(),
                default,
            }
        })
        .collect();

    entries.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.mime.cmp(&b.mime))
    });

    State {
        apps,
        entries,
        search: String::new(),
        status: String::new(),
        dirty: false,
        dark: detect_dark(),
        open: None,
    }
}

fn app_name(map: &HashMap<String, AppEntry>, id: &str) -> String {
    map.get(id)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| id.to_string())
}

fn app_icon(state: &State, id: &str) -> Option<std::path::PathBuf> {
    state.apps.iter().find(|a| a.id == id).and_then(|a| a.icon.clone())
}

fn icon_widget(icon: &Option<std::path::PathBuf>) -> Element<'static, Message> {
    const SIZE: f32 = 18.0;
    match icon {
        Some(p) if p.extension().map(|e| e == "svg").unwrap_or(false) => {
            svg(iced::widget::svg::Handle::from_path(p))
                .width(SIZE)
                .height(SIZE)
                .into()
        }
        Some(p) => image(iced::widget::image::Handle::from_path(p))
            .width(SIZE)
            .height(SIZE)
            .into(),
        None => Space::with_width(SIZE).into(),
    }
}

fn icon_for_choice(state: &State, c: &Choice) -> Option<std::path::PathBuf> {
    match c {
        Choice::App(id, _) => app_icon(state, id),
        Choice::None => None,
    }
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    match message {
        Message::SetDefault(mime, choice) => {
            if let Some(e) = state.entries.iter_mut().find(|e| e.mime == mime) {
                e.default = choice;
                state.dirty = true;
            }
            state.open = None;
        }
        Message::Toggle(mime) => {
            if state.open.as_ref() == Some(&mime) {
                state.open = None;
            } else {
                state.open = Some(mime);
            }
        }
        Message::SetSearch(q) => {
            state.search = q;
        }
        Message::Save => {
            let mut map: BTreeMap<String, String> = BTreeMap::new();
            let current = mime::read_mimeapps();
            let known: HashSet<String> = state.entries.iter().map(|e| e.mime.clone()).collect();
            for (k, v) in &current {
                if !known.contains(k) {
                    map.insert(k.clone(), v.clone());
                }
            }
            for e in &state.entries {
                if let Choice::App(id, _) = &e.default {
                    map.insert(e.mime.clone(), id.clone());
                }
            }
            match mime::write_mimeapps(&map) {
                Ok(()) => {
                    state.status = "Saved.".to_string();
                    state.dirty = false;
                }
                Err(err) => {
                    state.status = format!("Save failed: {}", err);
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
    SetDefault(String, Choice),
    Toggle(String),
    SetSearch(String),
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

    let search = text_input("Search MIME types or names...", &state.search)
        .on_input(Message::SetSearch)
        .width(Length::Fill);

    let query = state.search.to_lowercase();
    let filtered: Vec<&MimeDefaultState> = state
        .entries
        .iter()
        .filter(|e| {
            query.is_empty()
                || e.name.to_lowercase().contains(&query)
                || e.mime.to_lowercase().contains(&query)
        })
        .collect();

    let mut list = column![].spacing(4);
    for e in filtered {
        let mime = e.mime.clone();
        let is_open = state.open.as_ref() == Some(&mime);
        let cur_icon = icon_for_choice(state, &e.default);
        let toggle = button(row![icon_widget(&cur_icon), text(e.default.to_string())].spacing(6))
            .on_press(Message::Toggle(mime.clone()));

        let label = text(format!("{} ({})", e.name, e.mime)).size(13);
        let row_main = row![label, Space::with_width(Length::Fill), toggle].spacing(8);

        let mut col = column![row_main].spacing(2);
        if is_open {
            let mut opts: Vec<AppEntry> = state.apps_for_mime(&e.mime);
            if opts.is_empty() {
                opts = state.apps.clone();
            }
            // ensure current app present
            if let Choice::App(id, name) = &e.default {
                if !opts.iter().any(|a| a.id == *id) {
                    opts.push(AppEntry {
                        id: id.clone(),
                        name: name.clone(),
                        mime_types: Vec::new(),
                        icon: app_icon(state, id),
                    });
                }
            }
            opts.sort_by(|a, b| a.name.cmp(&b.name));

            let mut lst = column![].spacing(2);
            for a in &opts {
                let choice = Choice::App(a.id.clone(), a.name.clone());
                let is_sel = e.default == choice;
                let icon = a.icon.clone();
                let mut btn = button(row![icon_widget(&icon), text(a.name.clone())].spacing(6));
                btn = btn.on_press(Message::SetDefault(mime.clone(), choice));
                if is_sel {
                    btn = btn.style(iced::widget::button::primary);
                }
                lst = lst.push(btn);
            }
            col = col.push(scrollable(lst).height(Length::Fixed(200.0)));
        }
        list = list.push(col);
    }

    let body = scrollable(list).height(Length::Fill);

    let content = column![header, search, body]
        .spacing(12)
        .padding(12)
        .width(Length::Fixed(900.0));

    container(content)
        .center_x(Length::Fill)
        .into()
}

// keep State method apps_for_mime (defined in model? it's on State in model.rs)
// Need apps_for_mime exists; it does. apps_for_category removed usage.