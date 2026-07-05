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

## M1 — Hook (later)
## M2 — Core switching (later)
## M3 — App switcher UI (later)
## M4 — Window switcher UI (later)
## M5 — Polish (later)
