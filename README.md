# Super Simple Defaults

A small native Wayland app to set default applications for the current user.

It only changes user-level defaults (via `mimeapps.list`), never system-wide settings.

The executable is **`ssd`**. The Nix package also keeps `super-simple-defaults` as a compatibility alias. The package installs a desktop entry and a teal application icon. Currently the supported Nix target is **x86_64-linux**; other platforms are not claimed as tested.

## Run

```sh
nix run github:MiPSR/super-simple-defaults
```

Optional refresh:

```sh
nix run --refresh github:MiPSR/super-simple-defaults
```

![Preview](preview.jpeg)

The preview above shows the original interface; the current interface uses teal accents and ordered fallback controls.

## Editing defaults

- Search by MIME type or description. Click an application to replace it.
- **+** adds another application until there are no unused installed applications left. An empty entry has no application dropdown, only **+**.
- Applications are an **ordered fallback list**, not applications launched simultaneously. Use **↑ / ↓** to reorder and **−** to remove one. Unavailable applications in an existing list remain visible so they can be replaced or removed.
- The application picker puts associated applications first. Choosing another application also creates the required user association when saved.
- **Teal highlighting and a dot** identify pending changes. **Undo** restores a row's loaded state. Reverting all changes disables Save again.
- **System** removes your explicit default for that MIME type when saved. The desktop may still choose an inherited default or an automatically preferred associated application: removing a default does not forbid opening that file type.
- **Save** writes only changed MIME entries. Unrelated defaults, association order, other sections, and comments are preserved. Inherited defaults are not copied wholesale into your user configuration.
- Defaults changed externally for an edited MIME type produce a conflict dialog. You can cancel and reload, or explicitly overwrite only your edited entries. External changes to unrelated entries are retained.
- **Reload** and closing the window ask before discarding pending changes. **Save first** saves before continuing.
- Action buttons briefly say **Saved**, **Reloaded**, or **Done** after success. Feedback clears after four seconds or the next click, scroll, or keyboard interaction. Status text is left of the fixed-width action buttons; full errors appear in a wrapping dialog.

Application discovery, configuration I/O, theme detection, and icon loading happen asynchronously. The interface appears before discovery completes; icons are read and decoded in background batches after defaults arrive. Icon lookups (including missing icons) are cached. The MIME list only builds visible rows plus a small overscan region. The window has a minimum size of 840 × 520 logical pixels.

## Reset scope and file safety

**Reset all** removes only `[Default Applications]` sections from your user `mimeapps.list` and user `*-mimeapps.list` files, including legacy files under `applications/`. Desktop environments can maintain separate default files even if you only use one desktop. Reset includes those user files so they do not silently override the result.

Custom `[Added Associations]`, `[Removed Associations]`, unrelated sections, and comments remain. System files are never edited. Added associations can still influence the desktop's automatic choice after reset.

Normal saves use `$XDG_CONFIG_HOME/mimeapps.list`. For each edited MIME type, conflicting default entries are removed from the current desktop's higher-priority user files and legacy user data files, so the saved choice actually applies. Unrelated entries and other desktops' overrides remain untouched by normal saves.

Configuration lookup follows the freedesktop precedence order and `$XDG_CURRENT_DESKTOP`, including its colon-separated desktop names. Empty or invalid relative XDG paths use standard fallback locations instead of writing into the working directory. An absolute `XDG_DATA_HOME` works without `HOME`; both user locations must be resolvable before editing.

Writes use unique temporary files in the destination directory, preserve existing file permission bits, synchronize file and directory contents, and atomically replace each destination. A user-level lock coordinates instances of this app. Before replacing a file, its contents are checked again for external changes. Other editors do not necessarily honor this lock, so this is optimistic conflict detection, not a guarantee against every possible simultaneous write.

An operation spanning several files cannot replace them all atomically. Partial failures report how many files were updated and refresh the displayed configuration. Save failures retain pending intent for retry where possible. As with any configuration editor, keep backups of important hand-maintained settings.

## Desktop integration and optional fallbacks

No shell command is needed for the primary theme lookup: it reads the desktop portal's appearance setting directly over the session D-Bus. The original optional fallbacks are retained:

| Function | Preferred method and fallbacks |
| --- | --- |
| Color scheme | Direct settings portal → `dbus-send` → `gsettings` → `GTK_THEME` → dark theme |
| Icon theme | Direct settings portal → `gsettings` → `hicolor` fallback theme |
| Reset confirmation | `kdialog` → `zenity` → in-app confirmation |

These commands are **host-provided optional tools**, not bundled by the Nix wrapper. They are alternate backends within each function, not interchangeable across all functions. Missing all of them does not prevent editing defaults: theme detection uses its fallback and reset has an in-app confirmation. A user's No/Cancel in a native reset dialog is final and does not open another dialog.

Theme command fallbacks have timeouts and do not block the UI. Native confirmation processes are awaited asynchronously. Icon lookup respects the selected theme, inheritance and requested size instead of recursively searching every icon directory per application. `NoDisplay` applications remain available because they can legitimately handle files.

The command name `ssd` was not present on the development host when selected; this is not a guarantee that no other project uses the name. Invoke the Nix package directly if another installed command conflicts.

## Development and checks

Use the repository's pinned development shell:

```sh
nix develop
cargo run --bin ssd
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Build and validate the package, desktop entry, and regression tests:

```sh
nix build
nix flake check
```

Tests use isolated temporary fixtures, never your actual defaults. They cover XDG paths, file precedence, fallback lists, desktop IDs, hidden-entry masking, added/removed associations, MIME aliases/inheritance, localized names, escaping, targeted writes, conflicts, reset preservation, file permissions, dirty tracking, icon decoding, and virtual-list bounds. No performance benchmarks are run.

Configuration behavior follows the [MIME Applications specification](https://specifications.freedesktop.org/mime-apps-spec/latest/), [Desktop Entry specification](https://specifications.freedesktop.org/desktop-entry-spec/latest/), and [XDG Base Directory specification](https://specifications.freedesktop.org/basedir-spec/latest/).
