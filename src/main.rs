mod config;
mod desktop;
mod icons;
mod mime;
mod model;

use iced::widget::{
    Space, button, column, container, image, opaque, row, scrollable, stack, svg, text, text_input,
};
use iced::{Color, Element, Length, Subscription, Task, Theme};
use icons::{IconCache, LoadedIcon};
use model::Edits;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TEAL: Color = Color::from_rgb(0.0, 128.0 / 255.0, 128.0 / 255.0);
const ROW_HEIGHT: f32 = 88.0;
const LIST_ID: &str = "mime-list";

fn main() -> iced::Result {
    iced::application("Super Simple Defaults", update, view)
        .window(iced::window::Settings {
            size: iced::Size::new(1120.0, 760.0),
            min_size: Some(iced::Size::new(840.0, 520.0)),
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: "io.github.MiPSR.SuperSimpleDefaults".into(),
                ..Default::default()
            },
            exit_on_close_request: false,
            ..Default::default()
        })
        .theme(theme)
        .subscription(subscription)
        .run_with(|| {
            (
                State::default(),
                Task::batch([
                    Task::perform(load(), Message::Loaded),
                    Task::perform(desktop::appearance(), Message::Appearance),
                ]),
            )
        })
}

fn theme(state: &State) -> Theme {
    let mut palette = if state.dark {
        Theme::Dark.palette()
    } else {
        Theme::Light.palette()
    };
    palette.primary = TEAL;
    Theme::custom("Super Simple Teal".into(), palette)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Save,
    Reload,
    Reset,
    Close(iced::window::Id),
}

#[derive(Debug, Clone)]
enum Dialog {
    Picker {
        entry: usize,
        slot: Option<usize>,
        query: String,
    },
    Confirm(Action),
    Conflict(Vec<String>),
    Error(String),
}

struct State {
    data: Option<mime::Data>,
    apps: BTreeMap<String, usize>,
    filtered: Vec<usize>,
    search: String,
    status: String,
    busy: Option<Action>,
    loading: bool,
    feedback: Option<(Action, Instant)>,
    dialog: Option<Dialog>,
    dark: bool,
    height: f32,
    offset: f32,
    icons: IconCache,
    icon_queue: VecDeque<String>,
    icon_theme: Option<String>,
    icon_generation: u64,
    icon_resolver: Option<Arc<Mutex<icons::Resolver>>>,
    scale: u32,
    after_save: Option<Action>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            data: None,
            apps: BTreeMap::new(),
            filtered: vec![],
            search: String::new(),
            status: String::new(),
            busy: None,
            loading: true,
            feedback: None,
            dialog: None,
            dark: true,
            height: 760.0,
            offset: 0.0,
            icons: IconCache::new(),
            icon_queue: VecDeque::new(),
            icon_theme: None,
            icon_generation: 0,
            icon_resolver: None,
            scale: 1,
            after_save: None,
        }
    }
}

impl State {
    fn dirty(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(|d| d.entries.iter().any(|e| e.changed()))
    }

    fn available(&self) -> bool {
        !self.loading && self.busy.is_none()
    }

    fn filter(&mut self) {
        let query = self.search.to_lowercase();
        self.filtered = self
            .data
            .as_ref()
            .map(|d| {
                d.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| {
                        query.is_empty()
                            || e.mime.to_lowercase().contains(&query)
                            || e.name.to_lowercase().contains(&query)
                    })
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
    }

    fn install(&mut self, data: mime::Data) {
        self.apps = data
            .apps
            .iter()
            .enumerate()
            .map(|(i, a)| (a.id.clone(), i))
            .collect();
        self.data = Some(data);
        self.loading = false;
        self.filter();
        self.queue_icons();
    }

    fn queue_icons(&mut self) {
        self.icon_generation += 1;
        let mut names = Vec::new();
        if let Some(data) = &self.data {
            self.icon_resolver = Some(Arc::new(Mutex::new(icons::Resolver::new(
                &data.snapshot.paths,
                self.icon_theme.clone(),
                self.scale,
            ))));
            let (start, end) = visible_range(self.filtered.len(), self.offset, self.height);
            for &index in &self.filtered[start..end] {
                for id in &data.entries[index].choices {
                    if let Some(&app) = self.apps.get(id) {
                        names.push(data.apps[app].icon_name.clone());
                    }
                }
            }
            names.extend(data.apps.iter().map(|a| a.icon_name.clone()));
        }
        let mut seen = std::collections::HashSet::new();
        self.icon_queue = names
            .into_iter()
            .filter(|n| !n.is_empty() && !self.icons.contains_key(n) && seen.insert(n.clone()))
            .collect();
    }

    fn next_icons(&mut self) -> Task<Message> {
        let names: Vec<_> = (0..24)
            .filter_map(|_| self.icon_queue.pop_front())
            .collect();
        if names.is_empty() {
            return Task::none();
        }
        let Some(resolver) = self.icon_resolver.clone() else {
            return Task::none();
        };
        let generation = self.icon_generation;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    resolver
                        .lock()
                        .map_err(|e| e.to_string())
                        .map(|mut r| r.resolve(names))
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
            },
            move |result| Message::Icons(generation, result),
        )
    }

    fn done(&mut self, action: Action) {
        self.busy = None;
        self.feedback = Some((action, Instant::now()));
    }

    fn app_name(&self, id: &str) -> String {
        self.data
            .as_ref()
            .and_then(|d| self.apps.get(id).map(|&i| d.apps[i].name.clone()))
            .unwrap_or_else(|| format!("{id} (unavailable)"))
    }
}

fn visible_range(total: usize, offset: f32, height: f32) -> (usize, usize) {
    let first = (offset / ROW_HEIGHT).floor() as usize;
    let start = first.saturating_sub(4).min(total);
    let end = (first + (height / ROW_HEIGHT).ceil() as usize + 5).min(total);
    (start, end.max(start))
}

#[derive(Debug, Clone)]
enum Message {
    Loaded(Result<mime::Data, String>),
    Appearance(desktop::Appearance),
    Theme(bool),
    Scale(f32),
    Icons(u64, Result<IconCache, String>),
    Search(String),
    Scroll(f32),
    Pick(usize, Option<usize>),
    PickerSearch(String),
    Choose(String),
    Remove(usize, usize),
    Move(usize, usize, bool),
    Inherit(usize),
    Undo(usize),
    Request(Action),
    Confirm,
    SaveThen,
    Cancel,
    Overwrite,
    Saved(
        Result<(), config::SaveError>,
        Result<mime::Data, String>,
        Edits,
    ),
    ResetConfirmed(desktop::Confirmation),
    ResetFinished(Result<(), String>, Result<mime::Data, String>),
    Interacted,
    Tick,
    Window(iced::window::Id, iced::window::Event),
    Escape,
}

async fn load() -> Result<mime::Data, String> {
    tokio::task::spawn_blocking(|| mime::load(config::Paths::environment()?))
        .await
        .map_err(|e| e.to_string())?
}

fn subscription(_: &State) -> Subscription<Message> {
    Subscription::batch([
        iced::window::events().map(|(id, event)| Message::Window(id, event)),
        iced::event::listen_with(|event, _, _| match event {
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_))
            | iced::Event::Mouse(iced::mouse::Event::WheelScrolled { .. })
            | iced::Event::Touch(iced::touch::Event::FingerPressed { .. }) => {
                Some(Message::Interacted)
            }
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. }) => {
                if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) {
                    Some(Message::Escape)
                } else {
                    Some(Message::Interacted)
                }
            }
            _ => None,
        }),
        iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick),
        Subscription::run(desktop::theme_changes).map(Message::Theme),
    ])
}

fn save_task(state: &mut State, overwrite: bool) -> Task<Message> {
    let Some(data) = &state.data else {
        return Task::none();
    };
    let edits = model::edits(&data.entries);
    if edits.is_empty() {
        return Task::none();
    }
    let snapshot = data.snapshot.clone();
    state.busy = Some(Action::Save);
    state.dialog = None;
    Task::perform(
        async move {
            let pending = edits.clone();
            match tokio::task::spawn_blocking(move || {
                let result = config::save(&snapshot, &edits, overwrite);
                let fresh = mime::load(snapshot.paths.clone());
                (result, fresh)
            })
            .await
            {
                Ok((result, fresh)) => (result, fresh, pending),
                Err(e) => (
                    Err(config::SaveError::Failed(e.to_string())),
                    Err(e.to_string()),
                    pending,
                ),
            }
        },
        |(result, fresh, edits)| Message::Saved(result, fresh, edits),
    )
}

fn perform(state: &mut State, action: Action) -> Task<Message> {
    state.dialog = None;
    match action {
        Action::Close(id) => iced::window::close(id),
        Action::Save => save_task(state, false),
        Action::Reload => {
            state.busy = Some(action);
            Task::batch([
                Task::perform(load(), Message::Loaded),
                Task::perform(desktop::appearance(), Message::Appearance),
            ])
        }
        Action::Reset => {
            state.busy = Some(action);
            Task::perform(desktop::confirm_reset(), Message::ResetConfirmed)
        }
    }
}

fn reset_task(state: &mut State) -> Task<Message> {
    let Some(data) = &state.data else {
        return Task::none();
    };
    let paths = data.snapshot.paths.clone();
    state.dialog = None;
    state.busy = Some(Action::Reset);
    Task::perform(
        async move {
            match tokio::task::spawn_blocking(move || {
                let result = config::reset(&paths);
                let fresh = mime::load(paths);
                (result, fresh)
            })
            .await
            {
                Ok(result) => result,
                Err(e) => (Err(e.to_string()), Err(e.to_string())),
            }
        },
        |(result, fresh)| Message::ResetFinished(result, fresh),
    )
}

fn update(state: &mut State, message: Message) -> Task<Message> {
    if matches!(
        message,
        Message::Search(_)
            | Message::Pick(..)
            | Message::PickerSearch(_)
            | Message::Choose(_)
            | Message::Remove(..)
            | Message::Move(..)
            | Message::Inherit(_)
            | Message::Undo(_)
            | Message::Request(_)
            | Message::Confirm
            | Message::SaveThen
            | Message::Cancel
            | Message::Overwrite
            | Message::Interacted
            | Message::Escape
    ) {
        state.feedback = None;
        state.status.clear();
    }
    match message {
        Message::Loaded(result) => {
            state.loading = false;
            let action = state.busy.take();
            match result {
                Ok(data) => {
                    if action == Some(Action::Reload) {
                        state.icons.clear();
                    }
                    state.install(data);
                    if action == Some(Action::Reload) {
                        state.done(Action::Reload);
                    }
                    return state.next_icons();
                }
                Err(e) => {
                    state.status = "Load failed".into();
                    state.dialog = Some(Dialog::Error(e));
                }
            }
        }
        Message::Appearance(appearance) => {
            state.dark = appearance.dark;
            if state.icon_theme != appearance.icon_theme {
                state.icon_theme = appearance.icon_theme;
                state.icons.clear();
                state.queue_icons();
                return state.next_icons();
            }
        }
        Message::Theme(dark) => state.dark = dark,
        Message::Scale(scale) => {
            let scale = (scale.ceil() as u32).clamp(1, 4);
            if scale != state.scale {
                state.scale = scale;
                state.icons.clear();
                state.queue_icons();
                return state.next_icons();
            }
        }
        Message::Icons(generation, result) => {
            if generation == state.icon_generation {
                if let Ok(icons) = result {
                    state.icons.extend(icons);
                }
                return state.next_icons();
            }
        }
        Message::Search(query) => {
            state.search = query;
            state.offset = 0.0;
            state.filter();
            return scrollable::scroll_to(
                scrollable::Id::new(LIST_ID),
                scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
            );
        }
        Message::Scroll(offset) => state.offset = offset,
        Message::Pick(entry, slot) if state.available() => {
            state.dialog = Some(Dialog::Picker {
                entry,
                slot,
                query: String::new(),
            });
        }
        Message::PickerSearch(query) => {
            if let Some(Dialog::Picker { query: q, .. }) = &mut state.dialog {
                *q = query;
            }
        }
        Message::Choose(id) if state.available() => {
            if let Some(Dialog::Picker { entry, slot, .. }) = state.dialog.take()
                && let Some(e) = state.data.as_mut().and_then(|d| d.entries.get_mut(entry))
                && !e.choices.contains(&id)
            {
                if let Some(slot) = slot {
                    e.choices[slot] = id;
                } else {
                    e.choices.push(id);
                }
                e.inherit = false;
            }
        }
        Message::Remove(index, slot) if state.available() => {
            if let Some(e) = state.data.as_mut().and_then(|d| d.entries.get_mut(index))
                && slot < e.choices.len()
            {
                e.choices.remove(slot);
                e.inherit = false;
            }
        }
        Message::Move(index, slot, up) if state.available() => {
            if let Some(e) = state.data.as_mut().and_then(|d| d.entries.get_mut(index)) {
                let other = if up {
                    slot.checked_sub(1)
                } else {
                    slot.checked_add(1)
                };
                if let Some(other) =
                    other.filter(|&i| i < e.choices.len() && slot < e.choices.len())
                {
                    e.choices.swap(slot, other);
                    e.inherit = false;
                }
            }
        }
        Message::Inherit(index) if state.available() => {
            if let Some(e) = state.data.as_mut().and_then(|d| d.entries.get_mut(index)) {
                if e.user_override {
                    e.choices.clear();
                    e.inherit = true;
                } else {
                    e.choices = e.original.clone();
                    e.inherit = false;
                }
            }
        }
        Message::Undo(index) if state.available() => {
            if let Some(e) = state.data.as_mut().and_then(|d| d.entries.get_mut(index)) {
                e.choices = e.original.clone();
                e.inherit = false;
            }
        }
        Message::Request(action) => {
            if !state.available() {
                if matches!(action, Action::Close(_)) {
                    state.status = "Please wait for the current operation".into();
                }
                return Task::none();
            }
            if action != Action::Save && state.dirty() {
                state.dialog = Some(Dialog::Confirm(action));
            } else {
                return perform(state, action);
            }
        }
        Message::Confirm => {
            if let Some(Dialog::Confirm(action)) = state.dialog.take() {
                return if action == Action::Reset {
                    reset_task(state)
                } else {
                    perform(state, action)
                };
            }
        }
        Message::SaveThen => {
            if let Some(Dialog::Confirm(action)) = state.dialog.take() {
                state.after_save = Some(action);
                return save_task(state, false);
            }
        }
        Message::Overwrite => return save_task(state, true),
        Message::Cancel | Message::Escape => {
            state.dialog = None;
            state.after_save = None;
        }
        Message::Saved(result, fresh, pending) => {
            state.busy = None;
            match result {
                Ok(()) => {
                    match fresh {
                        Ok(data) => state.install(data),
                        Err(e) => {
                            state.dialog =
                                Some(Dialog::Error(format!("Saved, but refreshing failed: {e}")));
                            state.status = "Refresh failed".into();
                            state.after_save = None;
                            return Task::none();
                        }
                    }
                    state.done(Action::Save);
                    if let Some(action) = state.after_save.take() {
                        return perform(state, action);
                    }
                    return state.next_icons();
                }
                Err(config::SaveError::Conflict(keys)) => {
                    state.dialog = Some(Dialog::Conflict(keys));
                    state.status = "External changes detected".into();
                }
                Err(config::SaveError::Failed(e)) => {
                    // A multi-file operation may have partially succeeded. Refresh
                    // the snapshot, retain unsaved intent, and show the actual error.
                    if let Ok(data) = fresh {
                        state.install(data);
                        if let Some(data) = &mut state.data {
                            for entry in &mut data.entries {
                                match pending.get(&entry.mime) {
                                    Some(model::Edit::Set(ids)) => entry.choices = ids.clone(),
                                    Some(model::Edit::Inherit) if entry.user_override => {
                                        entry.choices.clear();
                                        entry.inherit = true;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    state.after_save = None;
                    state.status = "Save failed".into();
                    state.dialog = Some(Dialog::Error(e));
                    return state.next_icons();
                }
            }
        }
        Message::ResetConfirmed(confirmation) => match confirmation {
            desktop::Confirmation::Yes => return reset_task(state),
            desktop::Confirmation::No => {
                state.busy = None;
                state.status = "Reset cancelled".into();
            }
            desktop::Confirmation::Unavailable => {
                state.busy = None;
                state.dialog = Some(Dialog::Confirm(Action::Reset));
            }
        },
        Message::ResetFinished(result, fresh) => {
            state.busy = None;
            let refresh_error = match fresh {
                Ok(data) => {
                    state.install(data);
                    None
                }
                Err(e) => Some(e),
            };
            match (result, refresh_error) {
                (Ok(()), None) => state.done(Action::Reset),
                (result, refresh) => {
                    let error = [
                        result.err(),
                        refresh.map(|e| format!("Refresh failed: {e}")),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                    state.status = "Reset/refresh failed".into();
                    state.dialog = Some(Dialog::Error(error));
                }
            }
            return state.next_icons();
        }
        Message::Tick => {
            if state
                .feedback
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(4))
            {
                state.feedback = None;
            }
        }
        Message::Window(id, event) => match event {
            iced::window::Event::CloseRequested => {
                if state.data.is_none() {
                    return iced::window::close(id);
                }
                return update(state, Message::Request(Action::Close(id)));
            }
            iced::window::Event::Opened { size, .. } | iced::window::Event::Resized(size) => {
                state.height = size.height;
                return iced::window::get_scale_factor(id).map(Message::Scale);
            }
            _ => {}
        },
        Message::Interacted => {}
        _ => {}
    }
    Task::none()
}

fn icon(state: &State, id: &str) -> Element<'static, Message> {
    let loaded = state
        .data
        .as_ref()
        .and_then(|d| state.apps.get(id).map(|&i| &d.apps[i].icon_name))
        .and_then(|name| state.icons.get(name))
        .and_then(Option::as_ref);
    match loaded {
        Some(LoadedIcon::Svg(handle)) => svg(handle.clone()).width(20).height(20).into(),
        Some(LoadedIcon::Raster(handle)) => image(handle.clone()).width(20).height(20).into(),
        None => Space::new(20, 20).into(),
    }
}

fn action_button<'a>(
    state: &State,
    action: Action,
    normal: &'a str,
    working: &'a str,
    done: &'a str,
) -> Element<'a, Message> {
    let label = if state.busy == Some(action) {
        working
    } else if state.feedback.is_some_and(|(a, _)| a == action) {
        done
    } else {
        normal
    };
    button(
        text(label)
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center),
    )
    .width(108)
    .on_press_maybe(
        (state.available()
            && (action == Action::Reload || state.data.is_some())
            && (action != Action::Save || state.dirty()))
        .then_some(Message::Request(action)),
    )
    .into()
}

fn mime_row<'a>(state: &'a State, index: usize, position: usize) -> Element<'a, Message> {
    let data = state.data.as_ref().expect("rows require loaded data");
    let entry = &data.entries[index];
    let changed = entry.changed();
    let effective = if changed {
        "Unsaved changes".to_string()
    } else if let Some(id) = &entry.effective {
        format!("Opens with {}", state.app_name(id))
    } else {
        "No default set".into()
    };
    let label = column![
        text(format!("{}{}", if changed { "● " } else { "" }, entry.name)).size(14),
        text(&entry.mime).size(12),
        text(effective).size(11).color(if changed {
            theme(state).palette().text
        } else {
            theme(state).palette().text.scale_alpha(0.65)
        }),
    ]
    .width(Length::FillPortion(2))
    .spacing(2);
    let mut choices = row![].spacing(4).align_y(iced::Alignment::Center);
    for (slot, id) in entry.choices.iter().enumerate() {
        let choice = button(
            row![icon(state, id), text(state.app_name(id)).size(13)]
                .spacing(6)
                .align_y(iced::Alignment::Center),
        )
        .width(190)
        .height(52)
        .on_press_maybe(
            state
                .available()
                .then_some(Message::Pick(index, Some(slot))),
        );
        let controls = column![
            button("↑").padding([0, 4]).on_press_maybe(
                (state.available() && slot > 0).then_some(Message::Move(index, slot, true))
            ),
            button("↓").padding([0, 4]).on_press_maybe(
                (state.available() && slot + 1 < entry.choices.len())
                    .then_some(Message::Move(index, slot, false))
            ),
        ];
        choices = choices.push(choice).push(controls).push(
            button("−").on_press_maybe(state.available().then_some(Message::Remove(index, slot))),
        );
    }
    let can_add = data.apps.iter().any(|a| !entry.choices.contains(&a.id));
    if can_add {
        choices = choices.push(
            button("+").on_press_maybe(state.available().then_some(Message::Pick(index, None))),
        );
    }
    let choices = scrollable(choices)
        .direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new(),
        ))
        .width(Length::FillPortion(3))
        .height(64);
    let undo = button("Undo")
        .width(62)
        .on_press_maybe((state.available() && changed).then_some(Message::Undo(index)));
    let inherit = button("System").width(72).on_press_maybe(
        (state.available() && (entry.user_override || changed)).then_some(Message::Inherit(index)),
    );
    let background = if changed {
        if state.dark {
            Color::from_rgb(0.02, 0.23, 0.23)
        } else {
            Color::from_rgb(0.84, 0.95, 0.94)
        }
    } else if state.dark {
        if position.is_multiple_of(2) {
            Color::from_rgb(0.14, 0.14, 0.16)
        } else {
            Color::from_rgb(0.11, 0.11, 0.13)
        }
    } else if position.is_multiple_of(2) {
        Color::from_rgb(0.96, 0.96, 0.97)
    } else {
        Color::from_rgb(0.91, 0.92, 0.93)
    };
    container(
        row![label, choices, column![undo, inherit].spacing(4)]
            .spacing(10)
            .align_y(iced::Alignment::Center),
    )
    .padding(8)
    .height(ROW_HEIGHT)
    .width(Length::Fill)
    .style(move |_| container::background(background))
    .into()
}

fn view(state: &State) -> Element<'_, Message> {
    let logo = svg(svg::Handle::from_memory(include_bytes!(
        "../assets/super-simple-defaults.svg"
    )))
    .width(30)
    .height(30);
    let title = row![logo, text("Default Applications").size(23)]
        .spacing(8)
        .align_y(iced::Alignment::Center);
    let header = row![
        title,
        container(text(&state.status).size(12))
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Right),
        action_button(state, Action::Save, "Save", "Saving…", "Saved"),
        action_button(state, Action::Reload, "Reload", "Reloading…", "Reloaded"),
        action_button(state, Action::Reset, "Reset all", "Resetting…", "Done"),
    ]
    .height(56)
    .spacing(8)
    .align_y(iced::Alignment::Center);
    let changed = state
        .data
        .as_ref()
        .map(|d| d.entries.iter().filter(|e| e.changed()).count())
        .unwrap_or(0);
    let search = text_input("Search MIME types or names…", &state.search).on_input(Message::Search);
    let summary = text(format!(
        "{} types · {changed} changed · + adds an ordered fallback · System removes your override",
        state.filtered.len()
    ))
    .size(12);
    let body: Element<'_, Message> = if state.loading {
        container(text("Loading applications and defaults…"))
            .center(Length::Fill)
            .into()
    } else if state.filtered.is_empty() {
        container(text(if state.data.is_none() {
            "Unable to load defaults. Use Reload to retry."
        } else {
            "No matching MIME types"
        }))
        .center(Length::Fill)
        .into()
    } else {
        let (start, end) = visible_range(state.filtered.len(), state.offset, state.height);
        let mut rows = column![Space::with_height(start as f32 * ROW_HEIGHT)].spacing(0);
        for position in start..end {
            rows = rows.push(mime_row(state, state.filtered[position], position));
        }
        rows = rows.push(Space::with_height(
            (state.filtered.len() - end) as f32 * ROW_HEIGHT,
        ));
        scrollable(rows)
            .id(scrollable::Id::new(LIST_ID))
            .on_scroll(|viewport| Message::Scroll(viewport.absolute_offset().y))
            .height(Length::Fill)
            .into()
    };
    let content: Element<'_, Message> =
        container(column![header, search, summary, body].spacing(10))
            .padding(12)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    if let Some(dialog) = &state.dialog {
        let panel = dialog_view(state, dialog);
        let shade = container(panel)
            .center(Length::Fill)
            .padding(24)
            .style(|_| container::background(Color::from_rgba(0.0, 0.0, 0.0, 0.6)));
        stack![content, opaque(shade)].into()
    } else {
        content
    }
}

fn dialog_view<'a>(state: &'a State, dialog: &'a Dialog) -> Element<'a, Message> {
    let contents: Element<'a, Message> = match dialog {
        Dialog::Picker { entry, slot, query } => {
            let data = state.data.as_ref().expect("picker requires data");
            let entry = &data.entries[*entry];
            let q = query.to_lowercase();
            let current = slot.and_then(|i| entry.choices.get(i));
            let mut options: Vec<_> = data.apps.iter().filter(|a| {
                (!entry.choices.contains(&a.id) || current == Some(&a.id))
                    && (q.is_empty() || a.name.to_lowercase().contains(&q) || a.id.to_lowercase().contains(&q))
            }).collect();
            options.sort_by_key(|a| (!entry.associated.contains(&a.id), a.name.to_lowercase(), a.id.clone()));
            let mut list = column![].spacing(4);
            for app in options {
                let note = if entry.associated.contains(&app.id) { "" } else { " · adds association" };
                let label = column![text(format!("{}{note}", app.name)), text(&app.id).size(11)];
                list = list.push(button(row![icon(state, &app.id), label].spacing(8).align_y(iced::Alignment::Center))
                    .width(Length::Fill).on_press(Message::Choose(app.id.clone())));
            }
            column![
                text(if slot.is_some() { "Choose application" } else { "Add fallback application" }).size(22),
                text(&entry.mime).size(13),
                text_input("Search applications…", query).on_input(Message::PickerSearch),
                scrollable(list).height(360),
                button("Cancel").on_press(Message::Cancel),
            ].spacing(12).into()
        },
        Dialog::Confirm(action) => {
            let (title, description, discard) = match action {
                Action::Reload => ("Unsaved changes", "Reloading will discard your pending changes.", "Discard and reload"),
                Action::Close(_) => ("Unsaved changes", "Save your changes before closing?", "Discard and close"),
                Action::Reset => ("Reset all user defaults?", "This clears default-application entries in your user configuration, including desktop-specific files. Custom added and removed associations are preserved. Pending edits will be discarded.", "Reset defaults"),
                Action::Save => ("Save", "Save pending changes?", "Save"),
            };
            let mut buttons = row![button("Cancel").on_press(Message::Cancel)].spacing(8);
            if *action != Action::Reset { buttons = buttons.push(button("Save first").on_press(Message::SaveThen)); }
            buttons = buttons.push(button(discard).on_press(Message::Confirm));
            column![text(title).size(22), text(description), buttons].spacing(16).into()
        },
        Dialog::Conflict(keys) => column![
            text("Defaults changed outside this app").size(22),
            text("These entries changed since loading. Your edits have not been written. Overwrite only these entries, or cancel and reload to inspect the external changes."),
            scrollable(text(keys.join("\n"))).height(160),
            row![button("Cancel").on_press(Message::Cancel), button("Overwrite edited entries").on_press(Message::Overwrite)].spacing(8),
        ].spacing(16).into(),
        Dialog::Error(error) => column![
            text("Operation needs attention").size(22),
            scrollable(text(error)).height(240),
            button("OK").on_press(Message::Cancel),
        ].spacing(16).into(),
    };
    container(contents)
        .padding(24)
        .width(650)
        .style(container::rounded_box)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, State) {
        let tmp = tempfile::tempdir().unwrap();
        let paths = config::Paths {
            config_home: tmp.path().join("config"),
            data_home: tmp.path().join("data"),
            config_dirs: vec![],
            data_dirs: vec![],
            desktops: vec![],
            locales: vec![],
        };
        let applications = paths.data_home.join("applications");
        std::fs::create_dir_all(&applications).unwrap();
        std::fs::create_dir_all(&paths.config_home).unwrap();
        for name in ["a", "b"] {
            std::fs::write(applications.join(format!("{name}.desktop")), format!(
                "[Desktop Entry]\nType=Application\nName={name}\nExec=example\nMimeType=text/plain;\n"
            )).unwrap();
        }
        std::fs::write(
            paths.target(),
            "[Default Applications]\ntext/plain=a.desktop;\n",
        )
        .unwrap();
        let mut state = State::default();
        state.install(mime::load(paths).unwrap());
        (tmp, state)
    }

    #[test]
    fn virtualization_bounds_and_overscan() {
        assert_eq!(visible_range(0, 0.0, 760.0), (0, 0));
        let (start, end) = visible_range(10_000, ROW_HEIGHT * 100.0, 760.0);
        assert_eq!(start, 96);
        assert!(end > 100 && end < 120);
        assert_eq!(visible_range(3, 100_000.0, 760.0), (3, 3));
    }

    #[test]
    fn feedback_clears_on_interaction_and_expiry() {
        let mut state = State::default();
        state.done(Action::Reset);
        let _ = update(&mut state, Message::Interacted);
        assert!(state.feedback.is_none());
        state.feedback = Some((Action::Save, Instant::now() - Duration::from_secs(5)));
        let _ = update(&mut state, Message::Tick);
        assert!(state.feedback.is_none());
    }

    #[test]
    fn fallback_controls_add_reorder_remove_and_revert() {
        let (_tmp, mut state) = fixture();
        let _ = update(&mut state, Message::Pick(0, None));
        let _ = update(&mut state, Message::Choose("b.desktop".into()));
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["a.desktop", "b.desktop"]
        );
        assert!(state.dirty());
        let _ = update(&mut state, Message::Move(0, 1, true));
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["b.desktop", "a.desktop"]
        );
        let _ = update(&mut state, Message::Remove(0, 0));
        assert!(!state.dirty());
        let _ = update(&mut state, Message::Inherit(0));
        assert!(state.dirty());
        assert!(state.data.as_ref().unwrap().entries[0].choices.is_empty());
        let _ = update(&mut state, Message::Undo(0));
        assert!(!state.dirty());
    }

    #[test]
    fn picker_does_not_add_duplicate_fallbacks() {
        let (_tmp, mut state) = fixture();
        let _ = update(&mut state, Message::Pick(0, None));
        let _ = update(&mut state, Message::Choose("a.desktop".into()));
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["a.desktop"]
        );
        assert!(!state.dirty());
    }

    #[test]
    fn dirty_reload_and_close_require_confirmation() {
        let (_tmp, mut state) = fixture();
        let _ = update(&mut state, Message::Remove(0, 0));
        let _ = update(&mut state, Message::Request(Action::Reload));
        assert!(matches!(
            state.dialog,
            Some(Dialog::Confirm(Action::Reload))
        ));
        let _ = update(&mut state, Message::Cancel);
        assert!(state.dirty());
        let id = iced::window::Id::unique();
        let _ = update(&mut state, Message::Request(Action::Close(id)));
        assert!(matches!(
            state.dialog,
            Some(Dialog::Confirm(Action::Close(_)))
        ));
        let _ = update(&mut state, Message::SaveThen);
        assert_eq!(state.after_save, Some(Action::Close(id)));
        assert_eq!(state.busy, Some(Action::Save));
    }

    #[test]
    fn cancelled_native_reset_never_falls_through_to_another_dialog() {
        let (_tmp, mut state) = fixture();
        state.busy = Some(Action::Reset);
        let _ = update(
            &mut state,
            Message::ResetConfirmed(desktop::Confirmation::No),
        );
        assert!(state.busy.is_none());
        assert!(state.dialog.is_none());
        assert_eq!(state.status, "Reset cancelled");
    }

    #[test]
    fn save_conflicts_retain_pending_choices_and_require_overwrite() {
        let (_tmp, mut state) = fixture();
        let _ = update(&mut state, Message::Remove(0, 0));
        let pending = model::edits(&state.data.as_ref().unwrap().entries);
        let fresh = state.data.clone().unwrap();
        let _ = update(
            &mut state,
            Message::Saved(
                Err(config::SaveError::Conflict(vec!["text/plain".into()])),
                Ok(fresh),
                pending,
            ),
        );
        assert!(state.dirty());
        assert!(matches!(state.dialog, Some(Dialog::Conflict(_))));
        assert!(state.data.as_ref().unwrap().entries[0].choices.is_empty());
    }
}
