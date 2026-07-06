# Manual test plan

Run on a real Windows 11 machine after each milestone. Automated tests cover
only pure logic (`cargo test`); everything below is by hand.

Build: `cargo xwin build --release --target x86_64-pc-windows-msvc`, copy
`target/x86_64-pc-windows-msvc/release/win-app-switcher.exe` to the test machine.

## M0 — Skeleton

### Tray icon
1. Run `win-app-switcher.exe`.
   - A "§" icon appears in the tray (may be under the overflow chevron).
   - Hovering it shows the tooltip "win-app-switcher".
   - No console window, no visible window.
2. Right-click the icon → a menu with a single **Quit** item appears.
3. Click elsewhere → menu closes, app keeps running.
4. Right-click → **Quit** → icon disappears, process is gone in Task Manager.

### Single instance
5. Start the exe, then start it a second time.
   - Dialog: "win-app-switcher is already running. Restart it?" Yes/No.
6. Choose **No** → second instance exits, first keeps running (one tray icon).
7. Start it again, choose **Yes** → old instance exits, new one runs.
   Exactly one tray icon; exactly one process in Task Manager.
8. Elevation cross-check: start the first instance as administrator, the second
   normally, choose **Yes** → restart still works (message filter allows it).
9. Debug build: at most one console window at a time. During the restart
   dialog no new console has opened yet; after **Yes** the old console goes
   away with the old instance and a fresh console shows the new build hash.
   Choosing **No** never flashes a console at all.

### Config
9. No `config.toml` next to the exe → starts silently with defaults.
10. Copy `config.example.toml` → `config.toml` unchanged (all comments) → starts silently.
11. Uncomment `#scale = 1.0` and set `scale = 2.0` → starts silently.
12. Write garbage (`theme = `) into `config.toml` → warning dialog naming the
    problem, app still starts and works with defaults.
13. Set `scale = 99` → warning dialog about the range, app still starts.

## M1 — Hook

Use a **debug** build for the event log: `make debug`, then run the exe from
`target/x86_64-pc-windows-msvc/debug/` (double-clicking opens its console).
Every key the hook sees prints with its interpretation, e.g.
`Tab down (vk=0x09 scan=0x0F shift=false): swallow, inject dummy key (Start
menu will not open), post AppNext`; unwatched keys print their vk/scan codes.
Repeat the key tests with the release build to confirm it behaves the same
(minus the log).

### Swallowing & events
1. WIN+TAB → Task View does **not** open; console prints `AppNext`.
   Hold WIN, tap TAB repeatedly → one `AppNext` per tap (key repeat too).
2. WIN+SHIFT+TAB → `AppPrev`.
3. WIN+§ → `WinNext`; WIN+SHIFT+§ → `WinPrev`. Nothing is typed anywhere.
4. Release WIN after any of the above → `Commit`, and the **Start menu does
   not open** (the M1 headline check — try fast taps and slow holds).
5. During a session (WIN held after TAB), press ESC → `Cancel`; releasing WIN
   afterwards prints nothing and Start menu stays closed.
6. During a WIN+TAB session, press Q → `CloseApp`; Windows Search does not open.
7. During a WIN+§ session, press TAB → `AppNext`, no Task View (the session
   switches over to the app switcher).

### Pass-through unaffected
8. WIN alone (tap) → Start menu opens normally.
9. WIN+L locks, WIN+D shows desktop, WIN+E opens Explorer, WIN+R opens Run.
10. Plain TAB, §, Q, ESC in a text editor behave completely normally.

### No stuck keys
11. After a dozen mixed sessions (commit, cancel, quick taps), type in an
    editor: no phantom modifiers — letters are lowercase, TAB indents, no
    stuck WIN (press E: Explorer must NOT open).
12. Quit from the tray → WIN+TAB opens Task View again (hook gone with process).

## M2 — Core switching

No dialogs yet: a switch is visible only as a focus change. Use a debug
build — the console prints `session start: N candidates` on the first
Next/Prev event and `activate candidate i/N` on commit.

### WIN+§ — window switching within the active app
1. Two Notepad windows, focus one. Quick WIN+§ tap → focus flips to the
   other window. Tap again → flips back.
2. Three windows of the same app: hold WIN, tap § twice → releasing WIN
   lands on the third (z-order: each § steps to a less recent window).
3. WIN+SHIFT+§ steps the other way: from the newest straight to the oldest.
4. ESC while WIN still held after § → focus stays where it was.
5. Other apps' windows interleaved in z-order (Notepad, Explorer, Notepad):
   WIN+§ cycles only the foreground app's windows.
6. App with a single window: WIN+§ leaves focus in place, no errors.
7. Minimize one of two Notepad windows: WIN+§ restores and focuses it
   (default `restore_minimized = true`).
8. Set `restore_minimized = false`: WIN+§ now skips the minimized window
   entirely (only visible windows cycle).
9. A dialog in focus (e.g. Notepad's Save As): WIN+§ still cycles the
   app's windows (owner walk works).

### WIN+TAB — app switching (no UI yet)
10. Quick WIN+TAB tap → focus jumps to the previously used app; tapping
    again toggles between the two most recent apps (macOS behavior).
11. Hold WIN, TAB TAB → third most recent app on release.
12. WIN+SHIFT+TAB as the first press → least recently used app (wrap).
13. Grouping: two Notepad windows + Explorer → WIN+TAB toggles
    Notepad↔Explorer as apps; the debug log candidate count equals the
    number of distinct apps, not windows.
14. UWP apps (e.g. Calculator, Settings) appear as their own apps, not as
    a shared "ApplicationFrameHost" group: with both open, WIN+TAB
    reaches each separately.
15. Elevated app (admin PowerShell) is listed and switchable when the
    switcher runs elevated.
16. ESC during a WIN+TAB session → focus unchanged.
17. WIN+Q during a session does nothing yet (M3) and Windows Search does
    not open.

## M3 — App switcher UI

### Dialog basics
1. Several apps open: WIN+TAB → a rounded panel (taskbar gray: light when
   Windows is in light mode, dark in dark mode) appears centered on the
   monitor under the mouse, one icon per app, most-recently-used first.
   The **second** icon is highlighted and its app name is shown underneath.
   Corners are visibly rounded with a soft edge (drawn with alpha, not the
   Win11 DWM rounding), no drop shadow, and the panel has a thin darkened
   outline like the standard alt-tab dialog.
2. Quick WIN+TAB tap still switches instantly to the previous app (the
   dialog may only flash briefly).
3. Hold WIN: TAB advances the highlight, SHIFT+TAB goes back, Right/Left
   arrows likewise; the selection wraps at both ends. The name label follows
   the selection.
4. Releasing WIN activates the highlighted app; the dialog closes.
5. ESC closes the dialog, focus stays where it was.
6. The dialog itself never appears as an entry in the icon row.
7. Icons look correct for Win32 apps; an app without an extractable icon
   shows a dim placeholder square (UWP icons: see M5).

### Mouse
8. Hovering an icon moves the highlight to it; releasing WIN then activates
   the hovered app.
9. Hover an icon, then press TAB: the highlight jumps from the keyboard
   position (mouse and keyboard selections are independent, macOS style);
   releasing WIN activates the keyboard selection. Moving the mouse again
   takes the highlight back.
10. Left click on an icon activates that app immediately.
11. Click outside the panel → dialog closes, no switch (releasing WIN
    afterwards does nothing).
12. The dialog opening under a still mouse does **not** steal the selection
    (a spurious hover only counts after real movement).

### WIN+Q
13. WIN+TAB, select an app with multiple windows, press Q while WIN held:
    all of that app's windows close, its icon leaves the row, the dialog
    stays open and shrinks; the highlight lands on the next app.
14. Q on the last remaining app closes the dialog.
15. An app with unsaved changes shows its own "save?" prompt instead of
    dying silently (WM_CLOSE, not termination).

### Config
16. `scale = 2.0` → everything (panel, icons, text) doubles.
17. `show_selected_name = false` → no name strip, panel is shorter.
18. `dialog_monitor = "primary"` → dialog always opens on the primary
    monitor regardless of the mouse.
19. `theme = "dark"` / `"light"` force the palettes; default `auto` follows
    the Windows system (taskbar) light/dark setting. Toggle Windows dark
    mode: the next dialog picks up the new color without a restart.

## M4 — Window switcher UI

1. Focus an app with 3+ windows. WIN+§, keep WIN held: after ~150 ms a
   vertical list appears, one row per window: app name | icon | window
   title. The second row is highlighted (the first § already stepped).
2. Quick WIN+§ tap (release before the delay) → window switches with **no**
   dialog, as in M2.
3. § steps down, SHIFT+§ up, Down/Up arrows likewise; wrap-around at both
   ends; releasing WIN activates the highlighted row.
4. ESC closes the list, focus stays where it was.
5. Mouse: hover highlights a row, releasing WIN activates it; left click
   activates immediately; click outside cancels.
6. TAB while the list is open (WIN held) → the list closes and the app
   switcher dialog opens (win session discarded, nothing activated).
   Same works before the delay: WIN+§ quickly followed by TAB.
7. `dialog_delay_ms = 1000` → the list appears only after one second;
   `dialog_delay_ms = 0` → practically immediately.
8. Single-window app: quick tap leaves focus in place; holding WIN shows a
   one-row list.
9. Left/Right arrows during the list pass through (WIN+Left snap fires);
   Up/Down during the app switcher pass through (WIN+Up maximize fires).

### WIN+W
10. W while the list is open closes only the highlighted window: its row
    leaves the list, the list shrinks, the app keeps running and the
    session stays open. W on the last row closes the list.
11. Holding W down closes only one window (autorepeat is ignored); a
    release + fresh press closes the next one.
12. W in the WIN+TAB app switcher does nothing (and the widgets pane does
    not open).

## M5 — Polish

### Desktop filter (needs 2+ virtual desktops)
1. Default (`desktop_filter = "current"`): a window on another virtual
   desktop appears in neither WIN+TAB nor the WIN+§ list.
2. `desktop_filter = "all"`: apps on other desktops appear in WIN+TAB and
   the WIN+§ list includes the app's windows on other desktops; activating
   one switches to that desktop.
3. With `"all"`, a suspended UWP app (opened, then minimized long ago) does
   **not** appear as a ghost entry.

### UWP icons
4. Open Settings and Calculator: both show their real icons in the WIN+TAB
   row and in the WIN+§ list header — not the dim placeholder.

### Unelevated degradation
5. Start the exe normally (no admin): a tray balloon warns once about
   running without administrator rights; switching still works.
6. Focus an elevated window (admin terminal): WIN+TAB does nothing while it
   has focus — expected — and works again when focus moves elsewhere.
7. Start elevated: no balloon.

### Task Scheduler autostart
8. The README `schtasks` recipe: after logging out and in, the switcher is
   running elevated (Task Manager shows "elevated: yes") with no UAC prompt
   and no balloon.

### Per-monitor DPI
9. On a display scaled >100% (e.g. 150%), both dialogs are sharp — text
   and icons show no bitmap-stretch blur — and visually the same size as
   on a 100% display.
10. Mixed-DPI monitors, `dialog_monitor = "mouse"`: the dialog opens
    correctly sized and centered on whichever monitor the mouse is on,
    both ways; mouse hover still tracks the icons/rows accurately.
