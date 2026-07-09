# Win App Switcher

macOS-style application switching for Windows.

How it differs from the built-in alt-tab:

- **WIN+TAB** shows one icon per running application — alt-tab shows every
  window of everything.
- **WIN+§** (Nordic keyboard) or **WIN+`** (US keyboard) lists all windows
  of the active application. A quick tap switches to the next window
  instantly, with no dialog at all.

<img width="1000" height="718" alt="Win App Switcher Introduction" src="https://github.com/user-attachments/assets/59e1e8b0-fb8a-43ee-a6dc-bcbef6c3192b" />

**Installation:** Copy `win-app-switcher.exe` from the
[latest release](../../releases/latest) to
`C:\Program Files\win-app-switcher`.

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

Run `win-app-switcher.exe` as an administrator — it fails to switch to
windows started from e.g. WSL unless it runs elevated.

> **Not in the Administrators group?** The logon task below cannot elevate
> a standard account, no matter how it is configured — use the
> [startup shortcut](#standard-user-accounts) instead.

To start it elevated at every login without a UAC prompt, create a Task
Scheduler logon task in an elevated cmd prompt (press WIN, type `cmd`,
choose **Run as administrator** — not PowerShell, the quoting below is
cmd-specific):

```
schtasks /Create /TN win-app-switcher /TR "\"C:\Program Files\win-app-switcher\win-app-switcher.exe\"" /SC ONLOGON /RL HIGHEST /F
```

Or in the Task Scheduler GUI: Create Task → check **Run with highest
privileges** → Triggers → **At log on**.

The switcher also works as a normal user, with degradations (a tray balloon
warns at startup): windows of elevated processes (and e.g. WSL-started
windows) cannot be activated, process priority falls from REALTIME to HIGH
so switching may lag under full CPU load, and WIN shortcuts pass through
while an elevated window has focus.

To remove the logon task, see [Uninstall](#uninstall).

### Standard user accounts

**Run with highest privileges** only elevates accounts in the
Administrators group; for a standard account the task silently starts an
unelevated copy. UAC offers no silent elevation for standard accounts — the
closest thing is a Startup shortcut that asks for administrator credentials
at every logon:

1. Delete the logon task if one exists (elevated prompt:
   `schtasks /Delete /TN win-app-switcher /F`) — otherwise its unelevated
   copy races the shortcut's copy at every logon.
2. Download `install-startup-shortcut.cmd` from the
   [latest release](../../releases/latest) and double-click it as the
   account that logs in (no elevation needed). It creates
   `win-app-switcher.lnk` in the Startup folder.

At every logon UAC then asks for administrator credentials and the switcher
starts elevated; **Cancel** starts it unelevated with the degradations
above. To undo, delete `win-app-switcher.lnk` from the Startup folder
(`WIN+R`, `shell:startup`).

### If it still starts without administrator rights

The "Running without administrator rights" balloon after a reboot means an
unelevated copy started. To find out why:

1. Confirm what is running: Task Manager → **Details** tab → right-click a
   column header → **Select columns** → **Elevated** shows whether
   `win-app-switcher.exe` is elevated.

2. Check that the task ran at your logon and requests elevation:

   ```
   schtasks /Query /TN win-app-switcher /V /FO LIST
   schtasks /Query /TN win-app-switcher /XML | findstr RunLevel
   ```

   In the first output **Last Run Time** should match your last logon and
   **Last Result** should be `0`. The second must print
   `<RunLevel>HighestAvailable</RunLevel>` — `LeastPrivilege` means the
   task was created without **Run with highest privileges**; delete and
   recreate it as above.

3. Test the task alone: exit the switcher from the tray, run
   `schtasks /Run /TN win-app-switcher`. If no warning balloon appears now,
   the task is fine — something else starts an unelevated copy earlier at
   logon and the task's copy exits (only one instance runs). Remove the
   extra entry: Task Manager → **Startup apps**, and the Startup folder
   (`WIN+R`, `shell:startup`).

4. **Run with highest privileges** only elevates accounts in the
   Administrators group — `net localgroup administrators` must list your
   account. Check `whoami /groups` in a normal window: an elevated prompt
   opened with another account's credentials reports that account's groups,
   not yours. If your account cannot be added to the group, use the
   [startup shortcut](#standard-user-accounts).

## Uninstall

Run [`scripts/uninstall.cmd`](scripts/uninstall.cmd) from an elevated
prompt. It exits the switcher, deletes the logon task, the install folder,
and the current account's [startup shortcut](#standard-user-accounts).

If the startup shortcut was created by a different account, delete
`win-app-switcher.lnk` from the Startup folder (`WIN+R`, `shell:startup`)
as that account.

The switcher writes nothing outside its install folder — no registry
entries, no other files.

## Updates

The switcher checks GitHub for a newer release at startup and daily, and
notifies with a tray balloon and an "Update available" tray-menu row.
Clicking either opens the release page — updating is a manual download,
there is no self-update. Set `check_updates = false` in `config.toml` to
disable the check.

## Antivirus false positives

The switcher installs a low-level keyboard hook (`WH_KEYBOARD_LL`) — the
same API keyloggers use — so heuristic antivirus scanners occasionally
flag the unsigned exe. This is a known issue for every switcher built this
way (AltAppSwitcher included), not a sign of anything malicious. If it
happens, add an exclusion for the exe, or build from source and verify the
binary against the release as described below.

## Similar projects

- [AltAppSwitcher](https://github.com/hdlx/AltAppSwitcher) — an
  application-centric alt-tab switcher for Windows, written in C. Thanks to
  it for the inspiration behind this project.

## Publishing a release

```
bin/release-github v1.2.3
```

This drafts a GitHub release pinned to HEAD (notes generated from
conventional commits) and dispatches the release workflow, which tests and
builds `win-app-switcher.exe` in the pinned Rust image and attaches it
(+ `.sha256`, `config.example.toml`, `install-startup-shortcut.cmd`,
`uninstall.cmd`) to the draft. Wait for the assets to
appear, review the draft in the browser, then press **Publish release** —
publishing creates the git tag.

The workflow uploads assets with the release bot's PAT, so the default
read-only `GITHUB_TOKEN` never needs write access. One-time setup: add the
bot as a collaborator with write access and store its PAT as the
`RELEASE_BOT_TOKEN` Actions secret.

A released exe can be verified independently: check out the tagged commit,
run `TAG=vX.Y.Z make docker-build` (same pinned image and version stamp as
CI), and compare `dist/win-app-switcher.exe.sha256` against the release
asset.
