# Rune limitations

This app is built with [Rune](https://github.com/anomalyco/rune) at revision
`eb5ae6c9f77b1a0051d8e1e5ee936ebea086bb90`. The framework works well for
everything the interface needs; a few things behave differently from the
previous iced build, either because Rune has no hook for them yet or because
they were redesigned on purpose. Each is reproduced only by what Rune can do
today, and revisited whenever Rune changes.

## Native close requests cannot be intercepted

`WindowEvent::CloseRequested` goes straight to the close path
(`crates/rune/src/window/events.rs`); `App::on_close` runs *before* the window
closes but cannot veto it, so there is no equivalent of iced's
`exit_on_close_request: false`.

Consequences for this app:

- Pressing the window manager's close button with pending (unsaved) changes
  closes the app and discards them, without a confirmation.
- Confirmation now happens on the footer buttons instead: **OK** saves and
  closes, **Cancel** discards and closes, **Apply** saves without closing.

If a later Rune revision adds a close-request veto, the old
"ask before discarding pending changes" behavior should move back onto the
native close button.

## Success labels are driven by wall-clock time

The one-second floor for the **Saved** / **Restored** labels uses
`std::time::Instant` (wall time) rather than Rune's virtual clock
(`Tx::clock`). In a real window this is exactly the intended behavior; in
headless script runs the virtual clock usually advances faster than wall time,
so a scripted interaction may arrive before the label has "really" been visible
for a second and the revert looks early or late. A future cleanup could carry
the deadline as a `Tx::clock()` deadline (the `rune::machine` style) so headless
tests time it deterministically.

## Scroll-only interaction does not revert success labels

"Next user interaction" is detected through button handlers, text-field change
hooks, key handling (`AppSpec::on_key`) and background clicks. Rune offers no
global input listener, and scroll viewports deliberately report scroll through
live-value hooks that cannot change state (`El::on_scroll_offset`), so
scrolling the list alone does not clear a success label. Clicking or typing
anything does.

## Nix vendoring needs `fetchCargoVendor`

The dependency graph contains `tiny-skia-path 0.11.4` twice — once from
crates.io (via `sctk-adwaita` → `tiny-skia`) and once from Rune's git checkout
(Rune vendors its own patched copy). `cargo vendor` (and therefore nixpkgs'
`importCargoLock`) refuses this duplicate; patching `tiny-skia-path` to Rune's
copy instead breaks `tiny-skia` and `usvg`, which match exhaustively on the
added `ConicTo` variant. `buildRustPackage`'s newer `fetchCargoVendor` handles
it, so the flake pins dependencies with `cargoHash` instead of
`cargoLock.outputHashes`. Bumping Rune requires updating that hash (`nix build`
prints the expected one).

## Debug-only Rune chrome

In debug builds Rune shows its playground bar along the bottom edge of the
window; release builds (including the Nix package) do not. This is Rune's
default behavior, not an app bug.
