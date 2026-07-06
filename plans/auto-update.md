# Update notification plan

Decision: no OTA self-update. The tray notifies when a newer GitHub release
exists; clicking opens the release page in the browser. Updating stays a
30-second manual step.

Why not OTA: the exe is unsigned and installs a `WH_KEYBOARD_LL` hook —
a self-replacing binary of that shape is exactly what AV heuristics hunt
for (README already documents false positives). OTA would also need
elevated-restart plumbing through Task Scheduler and rollback paths.
Hundreds of lines vs ~150 for the notification.

## Chosen behavior

- Check on startup and every 24 h while running (machines rarely reboot).
- While a newer version exists: balloon on **every** check (daily nag),
  with a short note that `check_updates = false` in config.toml disables
  checking entirely. A persistent "Update available: vX.Y.Z" row stays in
  the tray menu until updated.
- Clicking the balloon or the menu row opens the release page
  (`ShellExecuteW`).
- The tray tooltip shows the version — the git tag on release builds, the
  git hash on dev builds — and appends "update available: vX.Y.Z" when one
  is known.
- `check_updates = true` by default in config; README mentions the switch.
- Network errors are silent; retry at the next timer tick.

## How the check works (no new dependencies)

`GET https://github.com/pre/win-app-switcher/releases/latest` with
redirects disabled (WinHTTP, `WINHTTP_OPTION_REDIRECT_POLICY_NEVER`);
the `Location` header ends in `/tag/vX.Y.Z`. No JSON parsing, no GitHub
API rate limits. Add the `Win32_Networking_WinHttp` feature to the
existing `windows` crate — no new crate.

The check runs on a spawned `std::thread` and posts its result back to
the tray window with `PostMessage` (`WM_APP+n`). It must never run on the
message-loop thread: a blocked loop stalls the keyboard hook system-wide.

Compare by parsing `vX.Y.Z` into a `(u32, u32, u32)` tuple; notify only
when remote > local (plain inequality would nag after a local dev build
or a re-tag).

## Version stamping (prerequisite)

The binary only knows its git hash today, and CI builds *before* the tag
exists (publishing the draft creates it), so `git describe` can't provide
the release version. Fix: the release workflow already has `TAG` in the
job env — pass it through docker (`Makefile` / `bin/build`) into the
cargo build, and have `build.rs` emit `cargo:rustc-env=RELEASE_TAG=$TAG`
(empty when unset). Dev builds have no `RELEASE_TAG` and skip the check,
logging that to the debug console.

## Touched files

| File | Change |
|------|--------|
| `src/update.rs` (new) | WinHTTP redirect check + version parse/compare, ~80 lines |
| `src/main.rs` | 24 h `SetTimer`, spawn check thread, balloon + menu row, tooltip = tag (release) / hash (dev) + "update available" suffix, `WM_APP` handling, balloon/menu click → `ShellExecuteW` |
| `src/config.rs` | `check_updates: bool`, default `true` |
| `build.rs` | emit `RELEASE_TAG` from env |
| `.github/workflows/release.yml`, `Makefile`/`bin/build` | pass `TAG` into the docker build |
| `Cargo.toml` | add `Win32_Networking_WinHttp` feature |
| `config.example.toml`, `README.md` | document the switch |

## Tests

- Pure-logic unit tests for tag parse + compare (run on any host, per the
  existing convention).
- Manual: build locally with `RELEASE_TAG=v0.0.1`, run, expect the
  balloon and the menu row; add the case to `plans/manual-tests.md`.
