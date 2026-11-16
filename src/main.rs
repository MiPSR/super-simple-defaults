//! Default-applications editor, built on Rune.
//!
//! Layout: a header, a search field, a vertically scrolling list of MIME types
//! and a fixed footer (Defaults | OK, Cancel, Apply) under a thin rule. The
//! content above the footer scrolls independently; the footer stays visible.
//! Configuration work runs on background threads and lands through an event
//! pipe that `poll` drains on the UI thread.
mod config;
mod desktop;
mod icons;
mod mime;
mod model;

use icons::IconPaths;
use model::Edits;
use rune::edit::TextEdit;
use rune::tok::{self, Theme, type_};
use rune::{
    AppSpec, Cx, Color, El, GrowthSpec, Image, ImageCache, KeyEvent, Latest, ScrollSpec, SurfaceFit,
    TextStyle, Tx, col, el, row, text,
};
use rune_kit::glyphs::{CHEVRON_DOWN, CHEVRON_UP};
use rune_kit::ScrollBar;
use rune_kit::{
    Button, ButtonVariant, ControlSize, Dialog, Divider, IconButton, Kit, PaletteSeed, TextInput,
    root,
};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The app's accent: teal #008080, in OKLCH.
fn teal() -> Color {
    rune::oklch(0.543, 0.093, 194.77)
}
/// Slightly more compact than the previous 88-point rows.
const ROW_HEIGHT: f32 = 76.0;
const STRIP_HEIGHT: f32 = 54.0;
const CHIP_HEIGHT: f32 = 42.0;
const ICON_PT: f32 = 20.0;
/// Icons rasterize at 2×, so 1× and 2× displays draw them sharp and other
/// scales fall back to mips.
const ICON_SCALE: f32 = 2.0;
const FOOTER_HEIGHT: f32 = 56.0;
/// Seconds between checks for externally edited configuration files.
const WATCH_INTERVAL: f64 = 2.0;
const LIST_KEY: &str = "mime-list";
/// How long a success label must have shown before the next interaction may
/// revert it.
const FEEDBACK_MINIMUM: Duration = Duration::from_secs(1);

static MINUS: rune::Icon = rune::Icon {
    name: "ssd-minus",
    view: 16.0,
    body: r#"<path d="M3.5 8H12.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};
static PLUS: rune::Icon = rune::Icon {
    name: "ssd-plus",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>"#,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Save,
    Reset,
}

/// Which footer button a busy operation or a success label belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Ok,
    Apply,
    Defaults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterSave {
    Close,
}

enum Panel {
    Picker {
        entry: usize,
        slot: Option<usize>,
    },
    Defaults,
    Conflict(Vec<String>),
    Error(String),
}

/// Work results from background threads, delivered to `poll` through a pipe.
enum UiEvent {
    Loaded {
        attempt: u64,
        result: Result<mime::Data, String>,
        silent: bool,
    },
    Saved {
        attempt: u64,
        result: Result<(), config::SaveError>,
        fresh: Result<mime::Data, String>,
        edits: Edits,
        slot: Slot,
    },
    Reset {
        attempt: u64,
        result: Result<(), String>,
        fresh: Result<mime::Data, String>,
    },
    Appearance(desktop::Appearance),
    Theme(bool),
}

/// A cloneable handle for background threads: an event pipe plus the app's waker.
#[derive(Clone)]
struct Pipe {
    tx: mpsc::Sender<UiEvent>,
    wake: Arc<Mutex<Option<rune::Waker>>>,
}

impl Pipe {
    fn send(&self, event: UiEvent) {
        let woke = self.tx.send(event).is_ok();
        if woke
            && let Some(wake) = self.wake.lock().unwrap_or_else(|e| e.into_inner()).clone()
        {
            wake();
        }
    }

    fn set_waker(&self, waker: rune::Waker) {
        *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(waker);
    }
}

struct State {
    data: Option<mime::Data>,
    apps: BTreeMap<String, usize>,
    filtered: Vec<usize>,
    search: TextEdit,
    picker_search: TextEdit,
    status: String,
    busy: Option<(Op, Slot)>,
    feedback: Option<(Slot, Instant)>,
    dialog: Option<Panel>,
    dark: bool,
    icon_theme: Option<String>,
    /// Icon name → decoded image; Rune's `ImageCache` does the decoding.
    icons: BTreeMap<String, Image>,
    icon_cache: Option<Arc<ImageCache>>,
    icon_resolver: Arc<Mutex<icons::Resolver>>,
    icon_batch: Latest<IconPaths>,
    icon_rev: u64,
    /// Event pipe for background threads, and the receiving end `poll` drains.
    events: Pipe,
    event_rx: Arc<Mutex<mpsc::Receiver<UiEvent>>>,
    /// Fence for load/save/reset results: an event from an older generation is
    /// dropped.
    attempt: u64,
    reload_pending: bool,
    watch_due: Option<f64>,
    after_save: Option<AfterSave>,
    /// A snapshot scene installed fixture data; background loads are ignored.
    demo: bool,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            data: None,
            apps: BTreeMap::new(),
            filtered: vec![],
            search: TextEdit::new(""),
            picker_search: TextEdit::new(""),
            status: String::new(),
            busy: None,
            feedback: None,
            dialog: None,
            dark: true,
            icon_theme: None,
            icons: BTreeMap::new(),
            icon_cache: None,
            icon_resolver: Arc::new(Mutex::new(icons::Resolver::empty())),
            icon_batch: Latest::default(),
            icon_rev: 0,
            events: Pipe {
                tx,
                wake: Arc::new(Mutex::new(None)),
            },
            event_rx: Arc::new(Mutex::new(rx)),
            attempt: 0,
            reload_pending: false,
            watch_due: None,
            after_save: None,
            demo: false,
        }
    }
}

impl State {
    fn dirty(&self) -> bool {
        self.data
            .as_ref()
            .is_some_and(|d| d.entries.iter().any(|e| e.changed()))
    }

    fn changed_count(&self) -> usize {
        self.data
            .as_ref()
            .map(|d| d.entries.iter().filter(|e| e.changed()).count())
            .unwrap_or(0)
    }

    fn available(&self) -> bool {
        self.busy.is_none()
    }

    fn filter(&mut self) {
        let query = self.search.text.to_lowercase();
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
        // The resolver tracks the loaded snapshot's roots and the selected
        // theme; every install rebuilds it, as the theme may have changed.
        if let Some(paths) = self.data.as_ref().map(|d| d.snapshot.paths.clone()) {
            self.icon_resolver = Arc::new(Mutex::new(icons::Resolver::new(
                &paths,
                self.icon_theme.clone(),
                ICON_SCALE as u32,
            )));
        }
        self.filter();
        self.request_icons();
    }

    /// Asks the icon worker for every application icon's file; Rune's
    /// `ImageCache` decodes and rasterizes on its own threads afterwards.
    fn request_icons(&mut self) {
        let Some(data) = &self.data else {
            return;
        };
        let names: Vec<String> = data
            .apps
            .iter()
            .map(|a| a.icon_name.clone())
            .filter(|n| !n.is_empty())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if names.is_empty() {
            return;
        }
        let resolver = self.icon_resolver.clone();
        self.icon_batch
            .request(names, move |names| resolver.lock().unwrap().resolve(names));
    }

    fn apply_icon_paths(&mut self, paths: IconPaths) {
        let Some(cache) = &self.icon_cache else {
            return;
        };
        for (name, path) in paths {
            match path {
                Some(path) => {
                    self.icons
                        .insert(name, cache.file(path, ICON_PT, ICON_SCALE));
                }
                None => {
                    self.icons.remove(&name);
                }
            }
        }
        self.icon_rev += 1;
    }

    fn app_name(&self, id: &str) -> String {
        self.data
            .as_ref()
            .and_then(|d| self.apps.get(id).map(|&i| d.apps[i].name.clone()))
            .unwrap_or_else(|| format!("{id} (unavailable)"))
    }
}

/// The palette the kit derives its semantic tokens from: Rune's default
/// background and text for the appearance, with the app's teal accent.
fn kit_of(s: &State) -> Kit {
    let t = theme_of(s);
    let accent = teal();
    let mut k = Kit::from_palette(PaletteSeed {
        bg: t.bg,
        fg: t.text,
        accent,
    });
    // The primary (Submit) button carries the accent instead of a contrast fill.
    k.c.bg_icon_button_contrast = accent;
    k.c.submit_fg = Color::WHITE;
    k.c.submit_icon = Color::WHITE;
    k.c.submit_hover = Color::WHITE.alpha(0.12);
    k.c.contrast_pressed = Color::WHITE.alpha(0.20);
    k
}

fn theme_of(s: &State) -> Theme {
    tok::theme(s.dark)
}

fn setup(s: &mut State, waker: rune::Waker) {
    s.events.set_waker(waker.clone());
    s.icon_cache = Some(Arc::new(ImageCache::new(waker)));
    start_load(s, false);
    let events = s.events.clone();
    std::thread::spawn(move || {
        let appearance = desktop::appearance();
        events.send(UiEvent::Appearance(appearance));
    });
    let events = s.events.clone();
    std::thread::spawn(move || {
        desktop::watch_theme(move |dark| events.send(UiEvent::Theme(dark)));
    });
}

fn poll(s: &mut State, now: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    loop {
        let event = s
            .event_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_recv()
            .ok();
        let Some(event) = event else { break };
        changed |= apply_event(s, event, tx);
    }
    if let Some(paths) = s.icon_batch.take() {
        s.apply_icon_paths(paths);
        changed = true;
    }
    // Periodic check for configuration files edited outside this app.
    match s.watch_due {
        None => {
            let due = now + WATCH_INTERVAL;
            s.watch_due = Some(due);
            tx.wake_at(due);
        }
        Some(due) if now >= due => {
            let next = now + WATCH_INTERVAL;
            s.watch_due = Some(next);
            tx.wake_at(next);
            if s.data.is_some() && external_changes(s) {
                changed |= request_reload(s);
            }
        }
        _ => {}
    }
    if s.reload_pending && s.available() && s.dialog.is_none() {
        s.reload_pending = false;
        changed |= request_reload(s);
    }
    changed
}

/// Runs every key the focused control passes on, plus every key while nothing
/// has focus. Counts as user interaction; never takes a key.
fn on_key(s: &mut State, _event: &KeyEvent, _tx: &mut Tx) -> bool {
    interact(s);
    false
}

fn apply_event(s: &mut State, event: UiEvent, tx: &mut Tx) -> bool {
    match event {
        UiEvent::Loaded { attempt, result, silent } if attempt == s.attempt => {
            if s.demo {
                // A snapshot scene owns its fixture data; ignore real loads.
                return false;
            }
            apply_loaded(s, result, silent)
        }
        UiEvent::Loaded { .. } => false,
        UiEvent::Saved {
            attempt,
            result,
            fresh,
            edits,
            slot,
        } if attempt == s.attempt => apply_saved(s, result, fresh, edits, slot, tx),
        UiEvent::Saved { .. } => false,
        UiEvent::Reset { attempt, result, fresh } if attempt == s.attempt => apply_reset(s, result, fresh),
        UiEvent::Reset { .. } => false,
        UiEvent::Appearance(appearance) => {
            if s.demo {
                // A snapshot scene owns its appearance too.
                return false;
            }
            s.dark = appearance.dark;
            if s.icon_theme != appearance.icon_theme {
                s.icon_theme = appearance.icon_theme;
                s.icons.clear();
                if let Some(paths) = s.data.as_ref().map(|d| d.snapshot.paths.clone()) {
                    s.icon_resolver = Arc::new(Mutex::new(icons::Resolver::new(
                        &paths,
                        s.icon_theme.clone(),
                        ICON_SCALE as u32,
                    )));
                }
                s.request_icons();
                return true;
            }
            s.dark != appearance.dark
        }
        UiEvent::Theme(dark) => {
            if s.demo {
                return false;
            }
            if s.dark != dark {
                s.dark = dark;
                return true;
            }
            false
        }
    }
}

fn entries_of(s: &State) -> &[model::MimeEntry] {
    static EMPTY: [model::MimeEntry; 0] = [];
    s.data
        .as_ref()
        .map(|d| d.entries.as_slice())
        .unwrap_or(&EMPTY)
}

fn apply_loaded(s: &mut State, result: Result<mime::Data, String>, silent: bool) -> bool {
    // Pending edits survive a silent reload of files edited elsewhere.
    let pending = silent.then(|| model::edits(entries_of(s)));
    match result {
        Ok(data) => {
            s.install(data);
            if silent {
                if let Some(edits) = pending {
                    reapply(s, &edits);
                    s.filter();
                }
            } else {
                s.status.clear();
            }
            true
        }
        Err(e) => {
            if silent {
                s.status = "Reload failed".into();
            } else {
                s.status = "Load failed".into();
                s.dialog = Some(Panel::Error(e));
            }
            true
        }
    }
}

fn reapply(s: &mut State, edits: &Edits) {
    if let Some(data) = &mut s.data {
        for entry in &mut data.entries {
            match edits.get(&entry.mime) {
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

fn apply_saved(
    s: &mut State,
    result: Result<(), config::SaveError>,
    fresh: Result<mime::Data, String>,
    edits: Edits,
    slot: Slot,
    tx: &mut Tx,
) -> bool {
    s.busy = None;
    match result {
        Ok(()) => {
            match fresh {
                Ok(data) => s.install(data),
                Err(e) => {
                    s.dialog = Some(Panel::Error(format!("Saved, but refreshing failed: {e}")));
                    s.status = "Refresh failed".into();
                    s.after_save = None;
                    return true;
                }
            }
            if let Some(AfterSave::Close) = s.after_save.take() {
                tx.close_window();
                return true;
            }
            s.feedback = Some((slot, Instant::now()));
            true
        }
        Err(config::SaveError::Conflict(keys)) => {
            s.dialog = Some(Panel::Conflict(keys));
            s.status = "External changes detected".into();
            true
        }
        Err(config::SaveError::Failed(e)) => {
            // A multi-file operation may have partially succeeded. Refresh the
            // snapshot, retain unsaved intent, and show the actual error.
            if let Ok(data) = fresh {
                s.install(data);
                reapply(s, &edits);
            }
            s.after_save = None;
            s.status = "Save failed".into();
            s.dialog = Some(Panel::Error(e));
            true
        }
    }
}

fn apply_reset(
    s: &mut State,
    result: Result<(), String>,
    fresh: Result<mime::Data, String>,
) -> bool {
    s.busy = None;
    let refresh_error = match fresh {
        Ok(data) => {
            s.install(data);
            None
        }
        Err(e) => Some(e),
    };
    match (result, refresh_error) {
        (Ok(()), None) => {
            s.feedback = Some((Slot::Defaults, Instant::now()));
            true
        }
        (result, refresh) => {
            let error = [
                result.err(),
                refresh.map(|e| format!("Refresh failed: {e}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
            s.status = "Reset/refresh failed".into();
            s.dialog = Some(Panel::Error(error));
            true
        }
    }
}

/// Whether any watched configuration file differs from the loaded snapshot.
fn external_changes(s: &State) -> bool {
    let Some(data) = &s.data else {
        return false;
    };
    let mut paths: Vec<_> = data.snapshot.files.keys().cloned().collect();
    if let Ok(user) = data.snapshot.paths.user_files() {
        paths.extend(user);
    }
    for path in paths {
        let current = config::read(&path).unwrap_or_default();
        if data.snapshot.files.get(&path) != Some(&current) {
            return true;
        }
    }
    false
}

fn start_load(s: &mut State, silent: bool) -> bool {
    s.attempt += 1;
    let attempt = s.attempt;
    let paths = s.data.as_ref().map(|d| d.snapshot.paths.clone());
    let pipe = s.events.clone();
    std::thread::spawn(move || {
        let result = match paths {
            Some(paths) => mime::load(paths),
            None => config::Paths::environment().and_then(mime::load),
        };
        pipe.send(UiEvent::Loaded { attempt, result, silent });
    });
    true
}

fn request_reload(s: &mut State) -> bool {
    if !s.available() || s.dialog.is_some() {
        s.reload_pending = true;
        return false;
    }
    start_load(s, true)
}

fn start_save(s: &mut State, overwrite: bool, slot: Slot) -> bool {
    let Some(data) = &s.data else {
        return false;
    };
    let edits = model::edits(&data.entries);
    if edits.is_empty() {
        return false;
    }
    let snapshot = data.snapshot.clone();
    let pipe = s.events.clone();
    s.busy = Some((Op::Save, slot));
    s.dialog = None;
    s.attempt += 1;
    let attempt = s.attempt;
    std::thread::spawn(move || {
        let result = config::save(&snapshot, &edits, overwrite);
        let fresh = mime::load(snapshot.paths.clone());
        pipe.send(UiEvent::Saved {
            attempt,
            result,
            fresh,
            edits,
            slot,
        });
    });
    true
}

fn start_reset(s: &mut State) -> bool {
    let Some(data) = &s.data else {
        return false;
    };
    let paths = data.snapshot.paths.clone();
    let pipe = s.events.clone();
    s.busy = Some((Op::Reset, Slot::Defaults));
    s.dialog = None;
    s.attempt += 1;
    let attempt = s.attempt;
    std::thread::spawn(move || {
        let result = config::reset(&paths);
        let fresh = mime::load(paths);
        pipe.send(UiEvent::Reset { attempt, result, fresh });
    });
    true
}

/// The next user interaction reverts a success label, but only once it has been
/// visible for at least a second.
fn interact(s: &mut State) {
    if let Some((_, at)) = s.feedback
        && at.elapsed() >= FEEDBACK_MINIMUM
    {
        s.feedback = None;
    }
    s.status.clear();
}

fn edit_entry(s: &mut State, mime: &str, edit: impl FnOnce(&mut model::MimeEntry)) -> bool {
    let Some(data) = s.data.as_mut() else {
        return false;
    };
    let Some(index) = data.entries.iter().position(|e| e.mime == mime) else {
        return false;
    };
    edit(&mut data.entries[index]);
    true
}

fn pick(s: &mut State, mime: String, slot: Option<usize>) {
    interact(s);
    let index = s
        .data
        .as_ref()
        .and_then(|d| d.entries.iter().position(|e| e.mime == mime));
    if s.available()
        && let Some(entry) = index
    {
        s.dialog = Some(Panel::Picker { entry, slot });
    }
}

fn choose(s: &mut State, mime: String, slot: Option<usize>, id: String) {
    interact(s);
    if !s.available() {
        return;
    }
    s.dialog = None;
    s.picker_search = TextEdit::new("");
    edit_entry(s, &mime, |e| {
        if !e.choices.contains(&id) {
            if let Some(slot) = slot.filter(|&slot| slot < e.choices.len()) {
                e.choices[slot] = id;
            } else {
                e.choices.push(id);
            }
            e.inherit = false;
        }
    });
}

fn remove_choice(s: &mut State, mime: String, slot: usize) {
    interact(s);
    if s.available() {
        edit_entry(s, &mime, |e| {
            if slot < e.choices.len() {
                e.choices.remove(slot);
                e.inherit = false;
            }
        });
    }
}

fn move_choice(s: &mut State, mime: String, slot: usize, up: bool) {
    interact(s);
    if s.available() {
        edit_entry(s, &mime, |e| {
            let other = if up {
                slot.checked_sub(1)
            } else {
                slot.checked_add(1)
            };
            if let Some(other) = other.filter(|&i| i < e.choices.len() && slot < e.choices.len()) {
                e.choices.swap(slot, other);
                e.inherit = false;
            }
        });
    }
}

fn inherit_choice(s: &mut State, mime: String) {
    interact(s);
    if s.available() {
        edit_entry(s, &mime, |e| {
            if e.user_override {
                e.choices.clear();
                e.inherit = true;
            } else {
                e.choices = e.original.clone();
                e.inherit = false;
            }
        });
    }
}

fn undo_choice(s: &mut State, mime: String) {
    interact(s);
    if s.available() {
        edit_entry(s, &mime, |e| {
            e.choices = e.original.clone();
            e.inherit = false;
        });
    }
}

fn ok_pressed(s: &mut State, tx: &mut Tx) {
    interact(s);
    if !s.available() {
        return;
    }
    if s.dirty() {
        s.after_save = Some(AfterSave::Close);
        start_save(s, false, Slot::Ok);
    } else {
        tx.close_window();
    }
}

fn apply_pressed(s: &mut State) {
    interact(s);
    if s.available() && s.dirty() {
        start_save(s, false, Slot::Apply);
    }
}

fn cancel_pressed(s: &mut State, tx: &mut Tx) {
    interact(s);
    if s.available() {
        tx.close_window();
    }
}

fn defaults_pressed(s: &mut State) {
    interact(s);
    if s.available() {
        s.dialog = Some(Panel::Defaults);
    }
}

fn overwrite_pressed(s: &mut State) {
    interact(s);
    if s.available() {
        start_save(s, true, Slot::Ok);
    }
}

fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let t = theme_of(s);
    let k = kit_of(s);
    let mut layer = Vec::new();
    let content = col()
        .key("app")
        .w_full()
        .h_full()
        .fill(t.bg)
        .child(header(s, &k))
        .child(filters(s, &k))
        .child(list_area(s))
        .child(Divider::horizontal().build(&k))
        .child(footer(s, &k));
    dialog_layer(s, &k, &mut layer);
    root(content, layer).on_press(|s: &mut State| interact(s))
}

fn logo(s: &State) -> El {
    let Some(cache) = &s.icon_cache else {
        return el().size(30.0, 30.0);
    };
    let image = cache.svg(
        include_bytes!("../assets/super-simple-defaults.svg").as_slice(),
        30.0,
        ICON_SCALE,
    );
    el().key("logo")
        .size(30.0, 30.0)
        .pixels_fit(image.pixels(), SurfaceFit::Fit)
}

fn header(s: &State, k: &Kit) -> El {
    let t = theme_of(s);
    let title = row()
        .gap(8.0)
        .center_y()
        .child(logo(s))
        .child(
            text("Default Applications")
                .font(TextStyle::new(17.0, 600.0))
                .color(t.text),
        );
    row()
        .key("header")
        .h(52.0)
        .w_full()
        .pl(16.0)
        .pr(16.0)
        .gap(8.0)
        .center_y()
        .child(title)
        .child(el().grow(1.0))
        .child(
            text(&s.status)
                .font(k.ty.small)
                .color(k.c.text_muted)
                .truncate()
                .shrink()
                .min_w(0.0),
        )
}

fn filters(s: &State, k: &Kit) -> El {
    let summary = format!(
        "{} types · {} changed · + adds an ordered fallback · System removes your override",
        s.filtered.len(),
        s.changed_count()
    );
    col()
        .key("filters")
        .w_full()
        .pl(16.0)
        .pr(16.0)
        .pb(8.0)
        .gap(4.0)
        .child(
            el()
                .key("search")
                .w_full()
                .child(
                    TextInput::new(&s.search)
                        .placeholder("Search MIME types or names…")
                        .on_edit(|s: &mut State| &mut s.search)
                        .on_change(|s: &mut State, tx: &mut Tx| {
                            interact(s);
                            s.filter();
                            tx.scroll_to_instant(LIST_KEY, 0.0, 0.0);
                        })
                        .build(k),
                ),
        )
        .child(text(summary).font(k.ty.small).color(k.c.text_muted))
}

fn list_area(s: &State) -> El {
    let k = kit_of(s);
    let t = theme_of(s);
    let Some(_) = &s.data else {
        return col()
            .key("list")
            .grow(1.0)
            .min_h(0.0)
            .center()
            .child(
                text("Loading applications and defaults…")
                    .font(k.ty.ui)
                    .color(t.text_secondary),
            );
    };
    if s.filtered.is_empty() {
        return col()
            .key("list")
            .grow(1.0)
            .min_h(0.0)
            .center()
            .child(
                text("No matching MIME types")
                    .font(k.ty.ui)
                    .color(t.text_secondary),
            );
    }
    col()
        .key(LIST_KEY)
        .scroll(ScrollSpec { stick_bottom: false })
        .grow(1.0)
        .min_h(0.0)
        .scroll_anchor()
        .child(
            col()
                .key("rows")
                .growth(GrowthSpec::default())
                .virtual_rows(
                    s.filtered.len(),
                    ROW_HEIGHT,
                    |s: &State, i| -> Cow<'_, str> {
                        s.data
                            .as_ref()
                            .and_then(|d| s.filtered.get(i).and_then(|&index| d.entries.get(index)))
                            .map_or_else(
                                || Cow::Owned(format!("row-{i}")),
                                |entry| Cow::Owned(format!("mime-{}", entry.mime)),
                            )
                    },
                    |s: &State, i| match s.filtered.get(i) {
                        Some(&index) => {
                            let entry = &s.data.as_ref().expect("checked above").entries[index];
                            let mime = entry.mime.clone();
                            rune::memo(
                                format!("mime-{mime}"),
                                (entry, s.dark, s.icon_rev),
                                move || mime_row(s, index, i),
                            )
                        }
                        None => el(),
                    },
                ),
        )
        .scroll_bar(&k)
}

fn icon_el(s: &State, app: usize) -> El {
    let name = s.data.as_ref().map(|d| d.apps[app].icon_name.clone());
    let image = name.as_ref().and_then(|name| s.icons.get(name));
    match image {
        Some(image) => el()
            .size(ICON_PT, ICON_PT)
            .pixels_fit(image.pixels(), SurfaceFit::Fit),
        None => el().size(ICON_PT, ICON_PT),
    }
}

fn mime_row(s: &State, index: usize, position: usize) -> El {
    let data = s.data.as_ref().expect("rows require loaded data");
    let entry = &data.entries[index];
    let k = kit_of(s);
    let t = theme_of(s);
    let changed = entry.changed();
    let lock = !s.available();
    let effective = if changed {
        "Unsaved changes".to_string()
    } else if let Some(id) = &entry.effective {
        format!("Opens with {}", s.app_name(id))
    } else {
        "No default set".into()
    };
    let label = col()
        .grow(2.0)
        .shrink()
        .min_w(0.0)
        .gap(2.0)
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(
                    el()
                        .size(7.0, 7.0)
                        .radius(3.5)
                        .fill(if changed { k.c.bg_accent } else { Color::TRANSPARENT }),
                )
                .child(
                    text(&entry.name)
                        .font(type_::BODY)
                        .color(t.text)
                        .truncate()
                        .min_w(0.0),
                ),
        )
        .child(
            text(&entry.mime)
                .font(k.ty.mono)
                .color(k.c.text_muted)
                .truncate()
                .min_w(0.0),
        )
        .child(
            text(effective)
                .font(k.ty.small)
                .color(if changed { k.c.text_base } else { k.c.text_faint })
                .truncate()
                .min_w(0.0),
        );
    let mime = entry.mime.clone();
    let mut chips = Vec::new();
    for (slot, id) in entry.choices.iter().enumerate() {
        chips.push(choice_chip(s, &k, &mime, slot, id));
        chips.push(
            col()
                .key(format!("order-{slot}"))
                .gap(1.0)
                .child(
                    IconButton::new(&CHEVRON_UP)
                        .control_size(ControlSize::Mini)
                        .label("Move up")
                        .disabled(lock || slot == 0)
                        .on_press({
                            let mime = mime.clone();
                            move |s: &mut State| move_choice(s, mime.clone(), slot, true)
                        })
                        .build(&k),
                )
                .child(
                    IconButton::new(&CHEVRON_DOWN)
                        .control_size(ControlSize::Mini)
                        .label("Move down")
                        .disabled(lock || slot + 1 >= entry.choices.len())
                        .on_press({
                            let mime = mime.clone();
                            move |s: &mut State| move_choice(s, mime.clone(), slot, false)
                        })
                        .build(&k),
                ),
        );
        chips.push(
            IconButton::new(&MINUS)
                .control_size(ControlSize::Mini)
                .label("Remove fallback")
                .disabled(lock)
                .on_press({
                    let mime = mime.clone();
                    move |s: &mut State| remove_choice(s, mime.clone(), slot)
                })
                .build(&k),
        );
    }
    let can_add = data.apps.iter().any(|a| !entry.choices.contains(&a.id));
    if can_add {
        chips.push(
            IconButton::new(&PLUS)
                .control_size(ControlSize::Small)
                .label("Add fallback application")
                .disabled(lock)
                .on_press({
                    let mime = mime.clone();
                    move |s: &mut State| pick(s, mime.clone(), None)
                })
                .build(&k)
                .key(format!("add-{mime}")),
        );
    }
    let strip = el()
        .key("strip")
        .scroll_x()
        .grow(3.0)
        .shrink()
        .min_w(0.0)
        .h(STRIP_HEIGHT)
        .center_y()
        .child(row().gap(4.0).center_y().children(chips));
    let controls = col()
        .key("controls")
        .gap(2.0)
        .child(
            Button::new("Undo")
                .control_size(ControlSize::Small)
                .disabled(lock || !changed)
                .on_press({
                    let mime = mime.clone();
                    move |s: &mut State| undo_choice(s, mime.clone())
                })
                .build(&k),
        )
        .child(
            Button::new("System")
                .control_size(ControlSize::Small)
                .disabled(lock || !(entry.user_override || changed))
                .on_press({
                    let mime = mime.clone();
                    move |s: &mut State| inherit_choice(s, mime.clone())
                })
                .build(&k),
        );
    let background = if changed {
        k.c.bg_accent.alpha(0.22)
    } else if position.is_multiple_of(2) {
        k.c.layer_01
    } else {
        Color::TRANSPARENT
    };
    row()
        .w_full()
        .h(ROW_HEIGHT)
        .padding(8.0)
        .gap(10.0)
        .center_y()
        .fill(background)
        .radius(k.radius.xs)
        .child(label)
        .child(strip)
        .child(controls)
}

fn choice_chip(s: &State, k: &Kit, mime: &str, slot: usize, id: &str) -> El {
    let name = s.app_name(id);
    let icon_app = s.apps.get(id).copied().unwrap_or(0);
    let icon = icon_el(s, icon_app);
    let mime = mime.to_string();
    k.interactive(
        el()
            .key(format!("choice-{slot}"))
            .role("button")
            .label(format!("Change {name}"))
            .focusable(true)
            .h(CHIP_HEIGHT)
            .pl(8.0)
            .pr(10.0)
            .gap(6.0)
            .center_y()
            .radius(k.radius.sm)
            .fill(k.c.layer_02)
            .on_press(move |s: &mut State| pick(s, mime.clone(), Some(slot)))
            .child(icon)
            .child(
                text(name)
                    .font(k.ty.ui_compact)
                    .color(theme_of(s).text)
                    .truncate()
                    .max_w(160.0),
            ),
    )
}

fn footer(s: &State, k: &Kit) -> El {
    let lock = !s.available() || s.data.is_none();
    let dirty = s.dirty();
    let label = |slot: Slot, working: &'static str, done: &'static str, normal: &'static str| -> &'static str {
        if s.busy == Some((Op::Save, slot)) || s.busy == Some((Op::Reset, slot)) {
            working
        } else if s.feedback.is_some_and(|(mark, _)| mark == slot) {
            done
        } else {
            normal
        }
    };
    row()
        .key("footer")
        .h(FOOTER_HEIGHT)
        .w_full()
        .pl(16.0)
        .pr(16.0)
        .gap(8.0)
        .center_y()
        .justify_between()
        .child(
            Button::new(label(Slot::Defaults, "Restoring…", "Restored", "Defaults"))
                .control_size(ControlSize::Regular)
                .disabled(lock)
                .on_press(|s: &mut State| defaults_pressed(s))
                .build(k)
                .key("defaults"),
        )
        .child(
            row()
                .key("footer-actions")
                .gap(8.0)
                .center_y()
                .child(
                    Button::new(label(Slot::Ok, "Saving…", "Saved", "OK"))
                        .variant(ButtonVariant::Submit)
                        .control_size(ControlSize::Regular)
                        .disabled(lock)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| ok_pressed(s, tx))
                        .build(k)
                        .key("ok"),
                )
                .child(
                    Button::new("Cancel")
                        .control_size(ControlSize::Regular)
                        .disabled(lock || !dirty)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| cancel_pressed(s, tx))
                        .build(k)
                        .key("cancel"),
                )
                .child(
                    Button::new(label(Slot::Apply, "Saving…", "Saved", "Apply"))
                        .control_size(ControlSize::Regular)
                        .disabled(lock || !dirty)
                        .on_press(|s: &mut State| apply_pressed(s))
                        .build(k)
                        .key("apply"),
                ),
        )
}

fn dialog_layer(s: &State, k: &Kit, layer: &mut Vec<El>) {
    let Some(dialog) = &s.dialog else { return };
    match dialog {
        Panel::Defaults => {
            Dialog::new("defaults", "Restore system defaults?")
                .description("System-defined defaults will be restored.")
                .fit()
                .open(true)
                .on_close(|s: &mut State| s.dialog = None)
                .child(panel(vec![
                    text(
                        "This clears default-application entries in your user configuration, \
                         including desktop-specific files. Custom added and removed associations \
                         are preserved. Pending edits will be discarded.",
                    )
                    .wrap()
                    .font(k.ty.small)
                    .color(k.c.text_muted),
                    row()
                        .justify_end()
                        .gap(8.0)
                        .child(
                            Button::new("No")
                                .on_press(|s: &mut State| {
                                    interact(s);
                                    s.dialog = None;
                                })
                                .build(k)
                                .key("defaults-no"),
                        )
                        .child(
                            Button::new("Yes")
                                .variant(ButtonVariant::Submit)
                                .on_press(|s: &mut State| {
                                    s.dialog = None;
                                    start_reset(s);
                                })
                                .build(k)
                                .key("defaults-yes"),
                        )

                ]))
                .build(k, layer);
        }
        Panel::Picker { entry, slot } => picker_dialog(s, k, *entry, *slot, layer),
        Panel::Conflict(keys) => {
            let keys = keys.clone();
            Dialog::new("conflict", "Defaults changed outside this app")
                .fit()
                .open(true)
                .on_close(|s: &mut State| s.dialog = None)
                .child(panel(vec![
                    text(
                        "These entries changed since loading. Your edits have not been \
                         written. Overwrite only these entries, or cancel to inspect the \
                         external changes.",
                    )
                    .wrap()
                    .font(k.ty.small)
                    .color(k.c.text_muted),
                    el()
                        .key("conflict-keys")
                        .h(160.0)
                        .w_full()
                        .scroll(ScrollSpec { stick_bottom: false })
                        .child(text(keys.join("\n")).wrap().font(k.ty.mono)),
                    row()
                        .justify_end()
                        .gap(8.0)
                        .child(
                            Button::new("Cancel")
                                .on_press(|s: &mut State| {
                                    interact(s);
                                    s.dialog = None;
                                })
                                .build(k),
                        )
                        .child(
                            Button::new("Overwrite edited entries")
                                .on_press(|s: &mut State| overwrite_pressed(s))
                                .build(k),
                        )

                ]))
                .build(k, layer);
        }
        Panel::Error(error) => {
            let error = error.clone();
            Dialog::new("error", "Operation needs attention")
                .fit()
                .open(true)
                .on_close(|s: &mut State| s.dialog = None)
                .child(panel(vec![
                    el()
                        .key("error-text")
                        .h(240.0)
                        .w_full()
                        .scroll(ScrollSpec { stick_bottom: false })
                        .child(text(error).wrap()),
                    row().justify_end().child(
                        Button::new("OK")
                            .on_press(|s: &mut State| {
                                interact(s);
                                s.dialog = None;
                            })
                            .build(k),
                    ),
                ]))
                .build(k, layer);
        }
    }
}

/// A dialog's body: the panel's own padding, since the header carries the only
/// other inset.
fn panel(children: Vec<El>) -> El {
    col().px(16.0).pb(16.0).gap(12.0).children(children)
}

fn picker_dialog(s: &State, k: &Kit, entry: usize, slot: Option<usize>, layer: &mut Vec<El>) {
    let data = s.data.as_ref().expect("picker requires loaded data");
    let entry_data = &data.entries[entry];
    let mime = entry_data.mime.clone();
    let q = s.picker_search.text.to_lowercase();
    let current = slot.and_then(|i| entry_data.choices.get(i));
    let mut options: Vec<_> = data
        .apps
        .iter()
        .filter(|a| {
            (!entry_data.choices.contains(&a.id) || current == Some(&a.id))
                && (q.is_empty()
                    || a.name.to_lowercase().contains(&q)
                    || a.id.to_lowercase().contains(&q))
        })
        .collect();
    options.sort_by_key(|a| {
        (
            !entry_data.associated.contains(&a.id),
            a.name.to_lowercase(),
            a.id.clone(),
        )
    });
    let mut items = Vec::new();
    for app in options {
        let note = if entry_data.associated.contains(&app.id) {
            ""
        } else {
            " · adds association"
        };
        items.push(app_row(s, k, app, note, &mime, slot));
    }
    let title = if slot.is_some() {
        "Choose application"
    } else {
        "Add fallback application"
    };
    Dialog::new("picker", title)
        .description(entry_data.mime.clone())
        .open(true)
        .on_close(|s: &mut State| {
            s.dialog = None;
            s.picker_search = TextEdit::new("");
        })
        .child(
            col()
                .gap(12.0)
                .px(16.0)
                .pb(16.0)
                .grow(1.0)
                .min_h(0.0)
                .child(
                    el()
                        .key("picker-search")
                        .w_full()
                        .child(
                            TextInput::new(&s.picker_search)
                                .placeholder("Search applications…")
                                .on_edit(|s: &mut State| &mut s.picker_search)
                                .on_change(|s: &mut State, _: &mut Tx| interact(s))
                                .build(k),
                        ),
                )
                .child(
                    el()
                        .key("app-list")
                        .grow(1.0)
                        .min_h(0.0)
                        .scroll(ScrollSpec { stick_bottom: false })
                        .child(col().gap(2.0).children(items)),
                )
                .child(
                    row().justify_end().child(
                        Button::new("Cancel")
                            .on_press(|s: &mut State| {
                                interact(s);
                                s.dialog = None;
                                s.picker_search = TextEdit::new("");
                            })
                            .build(k),
                    ),
                ),
        )
        .build(k, layer);
}

fn app_row(
    s: &State,
    k: &Kit,
    app: &model::AppEntry,
    note: &str,
    mime: &str,
    slot: Option<usize>,
) -> El {
    let name = format!("{}{note}", app.name);
    let icon_app = s.apps.get(&app.id).copied().unwrap_or(0);
    let icon = icon_el(s, icon_app);
    let mime = mime.to_string();
    let id = app.id.clone();
    k.interactive(
        el()
            .key(format!("app-{}", app.id))
            .role("button")
            .label(name.clone())
            .focusable(true)
            .w_full()
            .pl(8.0)
            .pr(8.0)
            .py(6.0)
            .gap(8.0)
            .center_y()
            .radius(k.radius.sm)
            .on_press(move |s: &mut State| choose(s, mime.clone(), slot, id.clone()))
            .child(icon)
            .child(
                col()
                    .grow(1.0)
                    .shrink()
                    .min_w(0.0)
                    .gap(1.0)
                    .child(
                        text(name)
                            .font(k.ty.ui)
                            .color(theme_of(s).text)
                            .truncate()
                            .min_w(0.0),
                    )
                    .child(
                        text(&app.id)
                            .font(k.ty.small)
                            .color(k.c.text_faint)
                            .truncate()
                            .min_w(0.0),
                    ),
            ),
    )
}

fn main() {
    // The window reports this id to the desktop (Wayland app_id, X11 WM_CLASS),
    // matching the installed .desktop entry.
    rune::set_app_id("io.github.MiPSR.SuperSimpleDefaults");
    rune::run(AppSpec::<State> {
        min_size: Some((640.0, 520.0)),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: &[
            ("fixture", |s| install_fixture(s)),
            ("fixture-light", |s| {
                install_fixture(s);
                s.dark = false;
            }),
            ("fixture-conflict", |s| {
                install_fixture(s);
                s.dialog = Some(Panel::Conflict(vec!["text/plain".into(), "image/png".into()]));
            }),
            ("fixture-error", |s| {
                install_fixture(s);
                s.dialog = Some(Panel::Error(
                    "The configuration file could not be written.\nPermission denied".into(),
                ));
            }),
        ],
        ..AppSpec::new("Super Simple Defaults", (1120.0, 760.0), State::default, view)
    });
}

/// A headless snapshot scene: two applications and a couple of MIME types, one
/// with an edited default so chips, icons and the changed-row tint show.
/// The `TempDir` leaks so the snapshot's files outlive the scene call; real
/// configuration is never touched.
fn install_fixture(s: &mut State) {
    let tmp = Box::leak(Box::new(tempfile::tempdir().expect("tempdir")));
    let paths = config::Paths {
        config_home: tmp.path().join("config"),
        data_home: tmp.path().join("data"),
        config_dirs: vec![],
        data_dirs: vec![],
        desktops: vec![],
        locales: vec![],
    };
    std::fs::create_dir_all(paths.config_home.clone()).unwrap();
    let applications = paths.data_home.join("applications");
    std::fs::create_dir_all(applications.clone()).unwrap();
    for (name, title, icon) in [
        ("a", "Archive Manager", "icon-a.svg"),
        ("b", "Text Editor", "icon-b.svg"),
        ("c", "Image Viewer", "icon-c.svg"),
        ("d", "Media Player", "icon-d.svg"),
    ] {
        std::fs::write(
            applications.join(icon),
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\"><rect x=\"3\" y=\"3\" width=\"18\" height=\"18\" rx=\"4\" fill=\"#008080\"/></svg>",
        )
        .unwrap();
        std::fs::write(
            applications.join(format!("{name}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName={title}\nExec=example\nMimeType=text/plain;image/png;\nIcon={}\n",
                applications.join(icon).display()
            ),
        )
        .unwrap();
    }
    std::fs::write(
        paths.target(),
        "[Default Applications]\ntext/plain=a.desktop;\nimage/png=c.desktop;b.desktop;a.desktop;\n",
    )
    .unwrap();
    s.demo = true;
    s.dark = true;
    s.icon_resolver = Arc::new(Mutex::new(icons::Resolver::new(&paths, None, ICON_SCALE as u32)));
    s.install(mime::load(paths).unwrap());
    // An unsaved edit so the changed-row tint and footer buttons show.
    remove_choice(s, "text/plain".into(), 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

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
        // A bare icon name, as a real .desktop entry has, inside an icon-theme
        // directory the resolver indexes.
        std::fs::create_dir_all(paths.data_home.join("icons/hicolor/24x24/apps")).unwrap();
        std::fs::write(
            paths
                .data_home
                .join("icons/hicolor/24x24/apps/example.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\"/>",
        )
        .unwrap();
        for name in ["a", "b"] {
            std::fs::write(
                applications.join(format!("{name}.desktop")),
                format!(
                    "[Desktop Entry]\nType=Application\nName={name}\nExec=example\nMimeType=text/plain;\nIcon=example\n"
                ),
            )
            .unwrap();
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
    fn install_rebuilds_the_icon_resolver_from_the_loaded_paths() {
        let (_tmp, state) = fixture();
        let name = state.data.as_ref().unwrap().apps[0].icon_name.clone();
        let paths = state.icon_resolver.lock().unwrap().resolve(vec![name.clone()]);
        assert!(
            paths.get(&name).is_some_and(|path| path.is_some()),
            "a fresh state's install must wire the resolver to the snapshot's roots"
        );
    }

    #[test]
    fn fallback_controls_add_reorder_remove_and_revert() {
        let (_tmp, mut state) = fixture();
        pick(&mut state, "text/plain".into(), None);
        assert!(matches!(state.dialog, Some(Panel::Picker { .. })));
        choose(&mut state, "text/plain".into(), None, "b.desktop".into());
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["a.desktop", "b.desktop"]
        );
        assert!(state.dirty());
        move_choice(&mut state, "text/plain".into(), 1, true);
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["b.desktop", "a.desktop"]
        );
        remove_choice(&mut state, "text/plain".into(), 0);
        assert!(!state.dirty());
        inherit_choice(&mut state, "text/plain".into());
        assert!(state.dirty());
        assert!(state.data.as_ref().unwrap().entries[0].choices.is_empty());
        undo_choice(&mut state, "text/plain".into());
        assert!(!state.dirty());
    }

    #[test]
    fn picker_does_not_add_duplicate_fallbacks() {
        let (_tmp, mut state) = fixture();
        pick(&mut state, "text/plain".into(), None);
        choose(&mut state, "text/plain".into(), None, "a.desktop".into());
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["a.desktop"]
        );
        assert!(!state.dirty());
    }

    #[test]
    fn save_conflicts_retain_pending_choices_and_require_overwrite() {
        let (_tmp, mut state) = fixture();
        remove_choice(&mut state, "text/plain".into(), 0);
        let pending = model::edits(&state.data.as_ref().unwrap().entries);
        let fresh = state.data.clone().unwrap();
        let mut rt = runtime_of(state);
        rt.update(|s: &mut State, tx| {
            apply_saved(
                s,
                Err(config::SaveError::Conflict(vec!["text/plain".into()])),
                Ok(fresh),
                pending,
                Slot::Apply,
                tx,
            );
        });
        assert!(rt.state.dirty());
        assert!(matches!(rt.state.dialog, Some(Panel::Conflict(_))));
        assert!(rt.state.data.as_ref().unwrap().entries[0].choices.is_empty());
    }

    fn runtime_of(state: State) -> Runtime<State> {
        Runtime::new(state, view, (1120.0, 760.0), Env::new(true, 1.0))
    }

    #[test]
    fn success_labels_revert_only_after_a_second_and_an_interaction() {
        let mut state = State::default();
        state.feedback = Some((Slot::Apply, Instant::now()));
        interact(&mut state);
        assert!(
            state.feedback.is_some(),
            "a fresh success label survives the next interaction"
        );
        state.feedback = Some((Slot::Apply, Instant::now() - FEEDBACK_MINIMUM));
        interact(&mut state);
        assert!(
            state.feedback.is_none(),
            "after a second it reverts on the next interaction"
        );
    }

    #[test]
    fn silent_reload_reapplies_pending_edits() {
        let (_tmp, mut state) = fixture();
        remove_choice(&mut state, "text/plain".into(), 0);
        assert_eq!(model::edits(entries_of(&state)).len(), 1);
        let paths = state.data.as_ref().unwrap().snapshot.paths.clone();
        // Simulate an external edit written behind the app's back.
        std::fs::write(
            paths.target(),
            "[Default Applications]\ntext/plain=b.desktop;\n",
        )
        .unwrap();
        let fresh = mime::load(paths).unwrap();
        assert!(apply_loaded(&mut state, Ok(fresh), true));
        assert!(
            state.data.as_ref().unwrap().entries[0].choices.is_empty(),
            "the user's removal survives the reload"
        );
        assert!(state.dirty());
        // And without pending edits, the fresh data simply shows through.
        let paths = state.data.as_ref().unwrap().snapshot.paths.clone();
        undo_choice(&mut state, "text/plain".into());
        let fresh = mime::load(paths).unwrap();
        assert!(apply_loaded(&mut state, Ok(fresh), true));
        assert_eq!(
            state.data.as_ref().unwrap().entries[0].choices,
            vec!["b.desktop"]
        );
    }

    #[test]
    fn external_file_edits_are_detected() {
        let (_tmp, mut state) = fixture();
        assert!(!external_changes(&state));
        let paths = state.data.as_ref().unwrap().snapshot.paths.clone();
        std::fs::write(
            paths.target(),
            "[Default Applications]\ntext/plain=b.desktop;\n",
        )
        .unwrap();
        assert!(external_changes(&state));
    }

    #[test]
    fn poll_detects_external_edits_and_reloads_keeping_pending_intents() {
        let (_tmp, state) = fixture();
        let mut rt = runtime_of(state);
        // A pending edit the reload must keep.
        rt.update(|s: &mut State, _| remove_choice(s, "text/plain".into(), 0));
        let paths = rt.state.data.as_ref().unwrap().snapshot.paths.clone();
        std::fs::write(
            paths.target(),
            "[Default Applications]\ntext/plain=b.desktop;\n",
        )
        .unwrap();
        // The check is due at 90 seconds.
        rt.update(|s: &mut State, tx| {
            s.watch_due = Some(90.0);
            assert!(poll(s, 100.0, tx));
        });
        assert_eq!(rt.state.attempt, 1, "the reload started");
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            rt.update(|s: &mut State, tx| {
                poll(s, 101.0, tx);
            });
            let fresh = rt
                .state
                .data
                .as_ref()
                .unwrap()
                .entries[0]
                .original
                .contains(&"b.desktop".to_string());
            if fresh {
                break;
            }
        }
        assert!(
            rt.state.data.as_ref().unwrap().entries[0]
                .original
                .contains(&"b.desktop".to_string()),
            "the external edit landed"
        );
        assert!(
            rt.state.data.as_ref().unwrap().entries[0].choices.is_empty(),
            "the pending removal survived the reload"
        );
        assert!(rt.state.dirty());
        assert!(rt.state.status.is_empty(), "the reload was silent");
    }

    #[test]
    fn footer_and_defaults_buttons_gate_on_dirty_and_busy_state() {
        let (_tmp, state) = fixture();
        let mut rt = runtime_of(state);
        // Clean state: Cancel and Apply are no-ops; OK closes without saving.
        rt.update(|s: &mut State, tx| cancel_pressed(s, tx));
        assert!(rt.state.dialog.is_none());
        rt.update(|s: &mut State, _| apply_pressed(s));
        assert!(rt.state.busy.is_none());
        rt.update(|s: &mut State, tx| ok_pressed(s, tx));
        assert!(rt.state.busy.is_none(), "clean state closes without saving");
        // Dirty state: Apply becomes busy and Defaults opens its confirmation.
        rt.update(|s: &mut State, _| remove_choice(s, "text/plain".into(), 0));
        rt.update(|s: &mut State, _| apply_pressed(s));
        assert_eq!(rt.state.busy, Some((Op::Save, Slot::Apply)));
        rt.state.busy = None;
        rt.update(|s: &mut State, _| defaults_pressed(s));
        assert!(matches!(rt.state.dialog, Some(Panel::Defaults)));
        rt.update(|s: &mut State, _| {
            interact(s);
            s.dialog = None;
        });
        assert!(rt.state.dialog.is_none());
        assert!(rt.state.busy.is_none());
        // Yes starts the reset.
        rt.update(|s: &mut State, _| defaults_pressed(s));
        rt.update(|s: &mut State, _| {
            assert!(matches!(s.dialog, Some(Panel::Defaults)));
            s.dialog = None;
            start_reset(s);
        });
        assert_eq!(rt.state.busy, Some((Op::Reset, Slot::Defaults)));
    }

    #[test]
    fn dirty_rows_and_stripes_reflect_in_the_view() {
        let (_tmp, state) = fixture();
        let mut rt = runtime_of(state);
        rt.frame(0.0);
        assert!(rt.find("ok").is_some(), "the footer is laid out");
        assert!(rt.find("mime-text/plain").is_some(), "the row is mounted");
        rt.update(|s: &mut State, _| remove_choice(s, "text/plain".into(), 0));
        rt.frame(1.0 / 60.0);
        let changed = rt
            .elements()
            .iter()
            .any(|e| e.text.as_deref() == Some("Unsaved changes"));
        assert!(changed, "the row shows its unsaved state");
        rt.update(|s: &mut State, _| undo_choice(s, "text/plain".into()));
        rt.frame(1.0 / 60.0);
        let changed = rt
            .elements()
            .iter()
            .any(|e| e.text.as_deref() == Some("Unsaved changes"));
        assert!(!changed, "undoing clears the row's unsaved state");
    }
}
