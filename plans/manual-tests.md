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
7. During a WIN+§ session, press TAB → swallowed, no event, no Task View.

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

## M3 — App switcher UI (later)
## M4 — Window switcher UI (later)
## M5 — Polish (later)
