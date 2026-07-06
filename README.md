# Win App Switcher

macOS-style application switching for Windows.

How it differs from the built-in alt-tab:

- **WIN+TAB** shows one icon per running application — alt-tab shows every
  window of everything.
- **WIN+§** (Nordic keyboard) or **WIN+`** (US keyboard) lists all windows
  of the active application. A quick tap switches to the next window
  instantly, with no dialog at all.

<img width="1000" height="718" alt="Win App Switcher Introduction" src="https://github.com/user-attachments/assets/59e1e8b0-fb8a-43ee-a6dc-bcbef6c3192b" />

## Keys

| Input | Action |
|-------|--------|
| `WIN+TAB` / `WIN+SHIFT+TAB` | Next / previous application |
| `WIN+§` (tap) | Switch to the next window of the active app, no UI |
| `WIN+§` (held) | Window list; `§` = next, `SHIFT+§` = previous |
| Arrow keys / mouse hover | Move the selection |
| Release `WIN` | Activate the selection |
| Left click | Activate immediately |
| `WIN+Q` | Close all windows of the selected application |
| `WIN+W` | Close the selected window (in the window list) |
| `ESC` | Cancel |

## Start at login

The switcher works as a normal user, with two degradations (a tray balloon
warns about them at startup): process priority falls from REALTIME to HIGH,
so switching may lag under full CPU load, and WIN shortcuts pass through
while an elevated window (admin terminal, installer) has focus — Windows
hides keystrokes in elevated windows from unelevated programs.

For the full experience, start it elevated at every login without a UAC
prompt using a Task Scheduler logon task. In an elevated prompt:

```
schtasks /Create /TN win-app-switcher /TR "C:\path\to\win-app-switcher.exe" /SC ONLOGON /RL HIGHEST /F
```

Or in the Task Scheduler GUI: Create Task → check **Run with highest
privileges** → Triggers → **At log on**.

## Publishing a release

```
bin/github-release v1.2.3
```

This drafts a GitHub release pinned to HEAD (notes generated from
conventional commits) and dispatches the release workflow, which tests and
builds `win-app-switcher.exe` in the pinned Rust image and attaches it
(+ `.sha256`, `config.example.toml`) to the draft. Wait for the assets to
appear, review the draft in the browser, then press **Publish release** —
publishing creates the git tag.

The workflow uploads assets with the release bot's PAT, so the default
read-only `GITHUB_TOKEN` never needs write access. One-time setup: add the
bot as a collaborator with write access and store its PAT as the
`RELEASE_BOT_TOKEN` Actions secret.

A released exe can be verified independently: check out the tagged commit,
run `make docker-build` (same pinned image as CI), and compare
`dist/win-app-switcher.exe.sha256` against the release asset.

