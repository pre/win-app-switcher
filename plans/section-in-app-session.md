# § during a WIN+TAB session opens the selected app's window list

## Goal

While a WIN+TAB app session is active (icon row visible or pending), pressing
`§` switches the session over to the window list of the **currently selected
app** — without releasing WIN. Today `§` in app mode is swallowed and does
nothing (`hook.rs:149`, `event` is `None` when mode is `App`); the user must
release WIN (commits the app switch) and press WIN+§ again.

This is the mirror of the already-existing switch-over in the other
direction: TAB during a WIN+§ session discards the win session and starts an
app session.

## Design

### 1. `hook.rs` — new event, mode transition

- Add `Event::WinList` ("show the selected app's window list") to the enum,
  the `from_wparam` array, and the roundtrip test.
- In `step()`, rewrite the `(Key::Section, false) if s.win_down` arm:

  ```rust
  (Key::Section, false) if s.win_down => {
      let inject_dummy = s.mode == Mode::None;
      let event = Some(match s.mode {
          Mode::App => Event::WinList,          // switch over to the selected app
          _ if shift => Event::WinPrev,
          _ => Event::WinNext,
      });
      s.mode = Mode::Win;
      Actions { swallow: true, event, inject_dummy }
  }
  ```

  `s.mode = Mode::Win` unconditionally: `None → Win` (starts a session, as
  today), `App → Win` (the new conversion), `Win → Win`. SHIFT is ignored for
  the conversion press — it opens the list, it doesn't cycle.

### 2. `apps.rs` — extract `app_windows`

`Session::Win` needs the selected app's windows with the same filtering
`foreground_app_windows` applies (minimized windows skipped unless
`restore_minimized`). Extract the shared part:

```rust
/// All eligible windows of `key` in z-order, minimized ones skipped
/// unless `include_minimized`.
pub fn app_windows(key: &AppKey, include_minimized: bool, all_desktops: bool) -> Vec<HWND> {
    eligible_windows(all_desktops)
        .into_iter()
        .filter(|(w, k)| k == key && (include_minimized || !IsIconic(*w).as_bool()))
        .map(|(w, _)| w)
        .collect()
}
```

`foreground_app_windows` keeps its foreground-resolution loop and delegates
the enumeration to this. Re-enumerating (instead of reusing
`AppGroup::windows`) keeps the "candidates captured at session start"
invariant: the conversion starts a *new* win session, so it snapshots now.

### 3. `main.rs` — `WinList` arm in `on_event`

```rust
WinList => {
    if let Some(Session::App { groups, kb }) = slot {
        let open = crate::ui::is_open();
        // The mouse may have picked an icon in the row.
        let sel = if open { crate::ui::selection() } else { *kb }.min(groups.len() - 1);
        let key = groups[sel].key.clone();
        let windows = crate::apps::app_windows(
            &key,
            cfg.restore_minimized,
            cfg.desktop_filter == DesktopFilter::All,
        );
        // Nothing to list (all windows minimized, restore off): keep the
        // app session; the hook's mode is now Win, same tolerated mismatch
        // as an empty-groups app session today.
        if windows.is_empty() {
            return;
        }
        unsafe {
            let _ = KillTimer(Some(main_hwnd), TIMER_APPROW);
        }
        *slot = Some(Session::Win { key, windows, index: 0 });
        if open {
            // The row is on screen: replace it with the list right away
            // (ui::open refreshes an already-open dialog in place).
            show_window_list(main_hwnd);
        } else {
            // Still inside dialog_delay_ms: keep quick-tap semantics.
            unsafe {
                SetTimer(Some(main_hwnd), TIMER_WINLIST, cfg.dialog_delay_ms, None);
            }
        }
    }
    // No app session (stale event): ignore.
}
```

Selection starts at `index: 0` (the app's topmost window), **not** stepped:
unlike WIN+§ from the foreground, the selected app is usually not the
foreground app, so "top window preselected" is the meaningful default.
Subsequent `§` presses arrive as `WinNext` and cycle as usual.

## Behavior after the change

| Input | Result |
|---|---|
| WIN+TAB … `§` (row visible) | Row is replaced by the selected app's window list immediately |
| WIN+TAB, `§` quickly (row not yet up) | List appears after `dialog_delay_ms`; releasing WIN before that activates the app's top window (quick-tap, same as committing the app) |
| `§` again / `SHIFT+§` / Up/Down | Cycle the list (existing `Mode::Win` handling) |
| `W` | Close selected window (existing) |
| TAB after conversion | Back to a fresh app session (existing switch-over) |
| WIN release / ESC | Commit / cancel (existing) |
| Selected app has only minimized windows, `restore_minimized = false` | `§` does nothing, app session stays |

## Tests

- `hook.rs`: new `section_in_app_session_switches_to_window_list` — WIN down,
  TAB (`AppNext`, mode `App`), `§` → swallowed, `Event::WinList`, no dummy
  injection, mode `Win`; second `§` → `WinNext`; `W` → `CloseWindow`; WIN up
  → `Commit`.
- `event_wparam_roundtrip`: add `WinList`.
- `plans/manual-tests.md`: add a WIN+TAB → `§` scenario (row visible and
  quick-press variants, mouse-hover selection honored).

## Docs

- `README.md` key table: add `§` during a held WIN+TAB session → window list
  of the selected app.

## Out of scope

- No new config. The existing `dialog_delay_ms` governs the pre-dialog delay.
- No reverse "TAB remembers which app was selected" changes — TAB keeps its
  existing fresh-session semantics.
