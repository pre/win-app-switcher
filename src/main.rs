// No automatic console: debug builds allocate theirs after the single-
// instance gate (see run()), so a restart never shows two consoles at once.
#![cfg_attr(windows, windows_subsystem = "windows")]

// Pure logic in these modules is cross-platform for `cargo test`;
// only tests use it off-Windows.
#[cfg_attr(not(windows), allow(dead_code))]
mod apps;
mod config;
#[cfg_attr(not(windows), allow(dead_code))]
mod hook;
#[cfg_attr(not(windows), allow(dead_code))]
mod ui;
#[cfg_attr(not(windows), allow(dead_code))]
mod update;

#[cfg(not(windows))]
fn main() {
    eprintln!("win-app-switcher only runs on Windows; `cargo test` covers the pure logic.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::run() {
        win::alert(&format!("{e:#}"), win::Severity::Error);
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use crate::config::{Config, DesktopFilter};
    use anyhow::{ensure, Context, Result};
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, OnceLock};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, COLORREF, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT,
        POINT, RECT, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
        DrawTextW, GdiFlush, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
        DIB_RGB_COLORS, DT_NOCLIP, DT_SINGLELINE, HBITMAP, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, OpenProcessToken, SetPriorityClass, WaitForSingleObject,
        REALTIME_PRIORITY_CLASS,
    };
    use windows::Win32::UI::HiDpi::{
        GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::Shell::{
        ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP,
        NIIF_LARGE_ICON, NIIF_USER, NIIF_WARNING, NIM_ADD, NIM_DELETE, NIM_MODIFY,
        NIN_BALLOONUSERCLICK, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, ChangeWindowMessageFilterEx, CreateIconIndirect, CreatePopupMenu,
        CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
        DestroyIcon, GetCursorPos, GetMessageW, KillTimer, MessageBoxW,
        PostMessageW, PostQuitMessage,
        RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, SM_CXICON,
        SM_CXSMICON, SYSTEM_METRICS_INDEX,
        TrackPopupMenu,
        TranslateMessage, HICON, HWND_BROADCAST, ICONINFO, IDYES, MB_ICONERROR, MB_ICONQUESTION,
        MB_ICONWARNING, MB_OK, MB_YESNO, MF_STRING, MSG, MSGFLT_ALLOW, SW_SHOWNORMAL,
        TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
        WM_CLOSE, WM_DESTROY, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
    };

    const CLASS_NAME: PCWSTR = w!("win-app-switcher.main");
    const MUTEX_NAME: PCWSTR = w!("Global\\win-app-switcher-single-instance");
    const WM_TRAY: u32 = WM_APP + 1;
    const TRAY_ID: u32 = 1;
    const CMD_QUIT: usize = 1;
    const CMD_UPDATE: usize = 2;
    /// Timer that opens the WIN+§ list dialog after `dialog_delay_ms`.
    const TIMER_WINLIST: usize = 1;
    /// Daily update-check timer (machines rarely reboot).
    const TIMER_UPDATE: usize = 2;
    /// Posted by the update-check thread when a newer release exists.
    const WM_UPDATE: u32 = WM_APP + 3;
    /// The tag CI stamped into this build; "" on dev builds (no update check).
    const RELEASE_TAG: &str = env!("RELEASE_TAG");
    const RELEASES_URL: PCWSTR = w!("https://github.com/pre/win-app-switcher/releases/latest");

    /// Broadcast "please exit" message id, registered before the window exists.
    static EXIT_MSG: AtomicU32 = AtomicU32::new(0);
    static CONFIG: OnceLock<Config> = OnceLock::new();
    /// Newer release tag found by the check thread; read on the UI thread.
    static UPDATE_TAG: Mutex<Option<String>> = Mutex::new(None);

    /// A switcher session: from the first Next/Prev event to Commit/Cancel.
    /// Candidates are captured once at session start, in z-order.
    enum Session {
        /// WIN+TAB: one group per app, shown in the switcher dialog. `kb` is
        /// the keyboard selection; the mouse selection lives in [`crate::ui`].
        App {
            groups: Vec<crate::apps::AppGroup>,
            kb: usize,
        },
        /// WIN+§: the foreground app's windows. A quick tap switches with no
        /// UI; the list dialog appears if WIN is held past `dialog_delay_ms`.
        Win {
            exe: String,
            windows: Vec<HWND>,
            index: usize,
        },
    }

    thread_local! {
        // Touched only by the main thread's wndproc.
        static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
    }

    pub enum Severity {
        Warning,
        Error,
    }

    pub fn alert(text: &str, severity: Severity) {
        let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
        let style = match severity {
            Severity::Warning => MB_OK | MB_ICONWARNING,
            Severity::Error => MB_OK | MB_ICONERROR,
        };
        unsafe {
            MessageBoxW(None, PCWSTR(wide.as_ptr()), w!("win-app-switcher"), style);
        }
    }

    pub fn run() -> Result<()> {
        let exit_msg = unsafe { RegisterWindowMessageW(w!("win-app-switcher.exit")) };
        ensure!(exit_msg != 0, "RegisterWindowMessageW failed");
        EXIT_MSG.store(exit_msg, Ordering::Relaxed);

        // Single instance: the mutex is held for the whole process lifetime and
        // released by the OS on exit.
        let mutex = unsafe { CreateMutexW(None, true, MUTEX_NAME) }.context("CreateMutexW")?;
        if unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
            let choice = unsafe {
                MessageBoxW(
                    None,
                    w!("win-app-switcher is already running.\n\nRestart it?"),
                    w!("win-app-switcher"),
                    MB_YESNO | MB_ICONQUESTION,
                )
            };
            if choice != IDYES {
                return Ok(());
            }
            unsafe {
                let _ = PostMessageW(Some(HWND_BROADCAST), exit_msg, WPARAM(0), LPARAM(0));
                let wait = WaitForSingleObject(mutex, 5000);
                ensure!(
                    wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED,
                    "the running instance did not exit within 5 seconds"
                );
            }
        }

        // The hook-event log console, only now that this instance is the
        // survivor: the replaced instance's console is already gone, so at
        // most one console is ever on screen.
        #[cfg(debug_assertions)]
        unsafe {
            let _ = windows::Win32::System::Console::AllocConsole();
            println!("win-app-switcher build {}", env!("GIT_HASH"));
        }

        // Switching must stay fast under full CPU load. REALTIME needs
        // elevation; unelevated the OS silently grants HIGH instead.
        unsafe {
            // Per-monitor DPI awareness (AAS declares PerMonitorV2 in its
            // manifest): without it DWM bitmap-stretches the dialog on
            // scaled displays, blurring text and icons.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let _ = SetPriorityClass(GetCurrentProcess(), REALTIME_PRIORITY_CLASS);
            // Shell icon extraction (IShellItemImageFactory) needs COM.
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .context("CoInitializeEx")?;
        }

        let config = match config_path().and_then(|p| Config::load(&p)) {
            Ok(c) => c,
            Err(e) => {
                alert(
                    &format!("{e:#}\n\nContinuing with default settings."),
                    Severity::Warning,
                );
                Config::default()
            }
        };
        let _ = CONFIG.set(config);

        unsafe {
            let hinstance: HINSTANCE = GetModuleHandleW(None).context("GetModuleHandleW")?.into();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            ensure!(RegisterClassW(&wc) != 0, "RegisterClassW failed");
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                w!("win-app-switcher"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hinstance),
                None,
            )
            .context("CreateWindowExW")?;
            // Let a lower-integrity second instance's restart request through UIPI
            // (we normally run elevated, the restarter may not be).
            let _ = ChangeWindowMessageFilterEx(hwnd, exit_msg, MSGFLT_ALLOW, None);

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAY,
                // Drawn at the taskbar's DPI-scaled small-icon size: shown
                // 1:1, no shell rescale to soften the glyph.
                hIcon: tray_icon(icon_size(hwnd, SM_CXSMICON)).context("tray icon")?,
                ..Default::default()
            };
            copy_wstr(&mut nid.szTip, &format!("win-app-switcher {}", version()));
            Shell_NotifyIconW(NIM_ADD, &nid)
                .ok()
                .context("Shell_NotifyIconW")?;

            // Unelevated everything still works, just degraded: priority fell
            // back to HIGH above, and WIN shortcuts pass through while an
            // elevated window has focus. Warn once, don't block.
            if !is_elevated() {
                let mut nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: TRAY_ID,
                    uFlags: NIF_INFO,
                    dwInfoFlags: NIIF_WARNING,
                    ..Default::default()
                };
                copy_wstr(&mut nid.szInfoTitle, "Running without administrator rights");
                copy_wstr(
                    &mut nid.szInfo,
                    "Apps launched from WSL are not included unless \
                     win-app-switcher runs as an administrator. See also README \
                     \u{201c}Start at login\u{201d}.",
                );
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }

            crate::hook::start(hwnd);

            if RELEASE_TAG.is_empty() {
                #[cfg(debug_assertions)]
                println!("update check skipped: dev build (no RELEASE_TAG)");
            } else if CONFIG.get().is_some_and(|c| c.check_updates) {
                spawn_update_check(hwnd);
                SetTimer(Some(hwnd), TIMER_UPDATE, 24 * 60 * 60 * 1000, None);
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }

    fn config_path() -> Result<std::path::PathBuf> {
        let exe = std::env::current_exe().context("current_exe")?;
        Ok(exe.with_file_name("config.toml"))
    }

    /// Copy into a fixed-size UTF-16 field, truncating, keeping a final NUL.
    fn copy_wstr(dst: &mut [u16], src: &str) {
        let n = dst.len() - 1;
        for (dst, src) in dst[..n].iter_mut().zip(src.encode_utf16()) {
            *dst = src;
        }
    }

    /// TokenElevation of our own token. On query failure claim elevated:
    /// better to skip the warning than to nag falsely.
    fn is_elevated() -> bool {
        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return true;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let res = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut _ as *mut _),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            );
            let _ = CloseHandle(token);
            res.is_err() || elevation.TokenIsElevated != 0
        }
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAY if lparam.0 as u32 == WM_RBUTTONUP => {
                show_tray_menu(hwnd);
                LRESULT(0)
            }
            WM_TRAY if lparam.0 as u32 == NIN_BALLOONUSERCLICK => {
                // Only the update balloon leads anywhere; the unelevated
                // warning balloon just dismisses.
                if UPDATE_TAG.lock().unwrap().is_some() {
                    open_releases_page();
                }
                LRESULT(0)
            }
            m if m == crate::hook::WM_SWITCHER => {
                if let Some(ev) = crate::hook::Event::from_wparam(wparam.0) {
                    on_event(hwnd, ev);
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_WINLIST => {
                let _ = KillTimer(Some(hwnd), TIMER_WINLIST);
                show_window_list(hwnd);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_UPDATE => {
                spawn_update_check(hwnd);
                LRESULT(0)
            }
            m if m == WM_UPDATE => {
                notify_update(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: hwnd,
                    uID: TRAY_ID,
                    ..Default::default()
                };
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m == EXIT_MSG.load(Ordering::Relaxed) => {
                // Another instance asked us to exit so it can take over.
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Session logic: WIN+TAB drives the app switcher dialog (M3), WIN+§
    /// cycles the foreground app's windows with no UI (M2; dialog in M4).
    /// The dialog's mouse input arrives here too — its wndproc posts the same
    /// Commit/Cancel events the hook does.
    fn on_event(main_hwnd: HWND, ev: crate::hook::Event) {
        use crate::hook::Event::*;
        let cfg = CONFIG.get().cloned().unwrap_or_default();
        SESSION.with_borrow_mut(|slot| match ev {
            AppNext | AppPrev => {
                let forward = ev == AppNext;
                // TAB mid WIN+§ session switches to the app switcher: the
                // win session is discarded, nothing activated.
                if matches!(slot, Some(Session::Win { .. })) {
                    unsafe {
                        let _ = KillTimer(Some(main_hwnd), TIMER_WINLIST);
                    }
                    *slot = None;
                    crate::ui::close();
                }
                match slot {
                    None => {
                        let all = cfg.desktop_filter == DesktopFilter::All;
                        let groups = crate::apps::app_groups(all);
                        if groups.is_empty() {
                            return;
                        }
                        #[cfg(debug_assertions)]
                        println!("app session: {} apps", groups.len());
                        // First press lands on the second app: releasing
                        // instantly switches to the previous app (macOS).
                        let kb = crate::apps::step_index(groups.len(), 0, forward);
                        crate::ui::show(main_hwnd, &groups, kb, &cfg);
                        *slot = Some(Session::App { groups, kb });
                    }
                    Some(Session::App { groups, kb }) => {
                        *kb = crate::apps::step_index(groups.len(), *kb, forward);
                        crate::ui::kb_select(*kb);
                    }
                    Some(Session::Win { .. }) => unreachable!("discarded above"),
                }
            }
            WinNext | WinPrev => {
                if slot.is_none() {
                    let (exe, windows) = crate::apps::foreground_app_windows(
                        cfg.restore_minimized,
                        cfg.desktop_filter == DesktopFilter::All,
                    );
                    #[cfg(debug_assertions)]
                    println!("win session: {} candidates", windows.len());
                    // The list dialog appears only if WIN is still held after
                    // the delay — a quick tap switches with no UI.
                    unsafe {
                        SetTimer(Some(main_hwnd), TIMER_WINLIST, cfg.dialog_delay_ms, None);
                    }
                    *slot = Some(Session::Win { exe, windows, index: 0 });
                }
                if let Some(Session::Win { windows, index, .. }) = slot {
                    *index =
                        crate::apps::step_index(windows.len(), *index, matches!(ev, WinNext));
                    // The dialog follows once open; a no-op before that.
                    crate::ui::kb_select(*index);
                }
            }
            Commit => {
                unsafe {
                    let _ = KillTimer(Some(main_hwnd), TIMER_WINLIST);
                }
                match slot.take() {
                    Some(Session::App { groups, .. }) => {
                        let sel = crate::ui::selection();
                        if let Some(group) = groups.get(sel) {
                            #[cfg(debug_assertions)]
                            println!("activate app {}/{} ({})", sel + 1, groups.len(), group.name);
                            // Activate first: the dialog holds the foreground,
                            // so SetForegroundWindow here is trivially allowed.
                            crate::apps::activate(group.windows[0], cfg.restore_minimized);
                        }
                        crate::ui::close();
                    }
                    Some(Session::Win { windows, index, .. }) => {
                        // The mouse may have picked a row in the list dialog.
                        let sel = if crate::ui::is_open() {
                            crate::ui::selection()
                        } else {
                            index
                        };
                        if let Some(&hwnd) = windows.get(sel) {
                            #[cfg(debug_assertions)]
                            println!("activate window {}/{} ({hwnd:?})", sel + 1, windows.len());
                            crate::apps::activate(hwnd, cfg.restore_minimized);
                        }
                        crate::ui::close();
                    }
                    None => {}
                }
            }
            Cancel => {
                unsafe {
                    let _ = KillTimer(Some(main_hwnd), TIMER_WINLIST);
                }
                slot.take();
                crate::ui::close();
            }
            // WIN+Q: close every window of the selected app, drop its icon
            // from the row, keep the session going (macOS Cmd+Q behavior).
            CloseApp => {
                if let Some(Session::App { groups, kb }) = slot {
                    let sel = crate::ui::selection().min(groups.len() - 1);
                    for hwnd in groups.remove(sel).windows {
                        unsafe {
                            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                    if groups.is_empty() {
                        *slot = None;
                        crate::ui::close();
                    } else {
                        *kb = sel.min(groups.len() - 1);
                        crate::ui::show(main_hwnd, groups, *kb, &cfg);
                    }
                }
            }
            // W in the window list: close only the selected window, keep the
            // app and the session going (macOS Cmd+W behavior).
            CloseWindow => {
                if let Some(Session::Win { exe, windows, index }) = slot {
                    if windows.is_empty() {
                        return;
                    }
                    let sel = if crate::ui::is_open() {
                        crate::ui::selection()
                    } else {
                        *index
                    }
                    .min(windows.len() - 1);
                    let hwnd = windows.remove(sel);
                    unsafe {
                        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    if windows.is_empty() {
                        unsafe {
                            let _ = KillTimer(Some(main_hwnd), TIMER_WINLIST);
                        }
                        *slot = None;
                        crate::ui::close();
                    } else {
                        *index = sel.min(windows.len() - 1);
                        if crate::ui::is_open() {
                            let name = crate::apps::display_name(exe);
                            let icon = crate::apps::icon_source(windows[0], exe);
                            let titles: Vec<String> =
                                windows.iter().map(|&w| crate::apps::window_title(w)).collect();
                            crate::ui::show_list(main_hwnd, &name, &icon, &titles, *index, &cfg);
                        }
                    }
                }
            }
        });
    }

    /// `dialog_delay_ms` elapsed with WIN still held: open the window list.
    fn show_window_list(main_hwnd: HWND) {
        let cfg = CONFIG.get().cloned().unwrap_or_default();
        SESSION.with_borrow(|slot| {
            if let Some(Session::Win { exe, windows, index }) = slot {
                if windows.is_empty() {
                    return;
                }
                let name = crate::apps::display_name(exe);
                let icon = crate::apps::icon_source(windows[0], exe);
                let titles: Vec<String> =
                    windows.iter().map(|&w| crate::apps::window_title(w)).collect();
                crate::ui::show_list(main_hwnd, &name, &icon, &titles, *index, &cfg);
            }
        });
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
        if let Some(tag) = UPDATE_TAG.lock().unwrap().as_deref() {
            let wide: Vec<u16> =
                format!("Update available: {tag}").encode_utf16().chain([0]).collect();
            let _ = AppendMenuW(menu, MF_STRING, CMD_UPDATE, PCWSTR(wide.as_ptr()));
        }
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required for the menu to dismiss when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            CMD_QUIT => {
                let _ = DestroyWindow(hwnd);
            }
            CMD_UPDATE => open_releases_page(),
            _ => {}
        }
    }

    /// The release tag on CI builds, the git hash on dev builds.
    fn version() -> &'static str {
        if RELEASE_TAG.is_empty() {
            env!("GIT_HASH")
        } else {
            RELEASE_TAG
        }
    }

    /// Check GitHub for a newer release off-thread — the message loop must
    /// never block (a stalled loop stalls the keyboard hook system-wide).
    /// A newer tag lands in [`UPDATE_TAG`] and is announced via [`WM_UPDATE`];
    /// network errors are silent, retried at the next timer tick.
    fn spawn_update_check(hwnd: HWND) {
        let hwnd = hwnd.0 as isize; // HWND is not Send
        std::thread::spawn(move || {
            if let Some(tag) = crate::update::latest_release_tag() {
                if crate::update::is_newer(&tag, RELEASE_TAG) {
                    *UPDATE_TAG.lock().unwrap() = Some(tag);
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(hwnd as *mut _)),
                            WM_UPDATE,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            }
        });
    }

    /// Balloon + tooltip for the newer release; repeats on every daily check
    /// until updated. The menu row comes from [`UPDATE_TAG`] when built.
    fn notify_update(hwnd: HWND) {
        let Some(tag) = UPDATE_TAG.lock().unwrap().clone() else { return };
        #[cfg(debug_assertions)]
        println!("update available: {tag}");
        // Without an explicit balloon icon the toast stretches the small
        // tray icon to toast size, blurring the glyph — redraw it large.
        let balloon = unsafe { tray_icon(icon_size(hwnd, SM_CXICON)).ok() };
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_TIP | NIF_INFO,
            dwInfoFlags: NIIF_USER | NIIF_LARGE_ICON,
            hBalloonIcon: balloon.unwrap_or_default(),
            ..Default::default()
        };
        copy_wstr(
            &mut nid.szTip,
            &format!("win-app-switcher {} — update available: {tag}", version()),
        );
        copy_wstr(&mut nid.szInfoTitle, &format!("Update available: {tag}"));
        copy_wstr(
            &mut nid.szInfo,
            "Click to open the release page. Set check_updates = false in \
             config.toml to disable this check.",
        );
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            // The shell copies the icon during the call.
            if let Some(icon) = balloon {
                let _ = DestroyIcon(icon);
            }
        }
    }

    fn open_releases_page() {
        unsafe {
            ShellExecuteW(None, w!("open"), RELEASES_URL, None, None, SW_SHOWNORMAL);
        }
    }

    /// Icon metric scaled to the taskbar's DPI: plain GetSystemMetrics is
    /// not DPI-aware in a PMv2 process (returns 96-dpi values), so a 150%
    /// display would get a 16 px icon stretched to 24 px by the shell.
    unsafe fn icon_size(hwnd: HWND, metric: SYSTEM_METRICS_INDEX) -> i32 {
        let dpi = GetDpiForWindow(hwnd);
        let size = GetSystemMetricsForDpi(metric, dpi);
        #[cfg(debug_assertions)]
        println!("icon: {size}px at {dpi} dpi");
        size
    }

    /// size×size white 32-bit top-down DIB; the glyph is drawn black on it
    /// and the coverage becomes alpha.
    unsafe fn white_canvas(dc: HDC, size: i32) -> Result<(HBITMAP, *mut u32)> {
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp =
            CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).context("DIB")?;
        std::ptr::write_bytes(bits as *mut u8, 0xFF, (size * size) as usize * 4);
        Ok((bmp, bits as *mut u32))
    }

    /// Paint "⌘" black at `em` px with its line box starting at (dx, dy):
    /// left/top-aligned and unclipped, so the ink position and extent scale
    /// linearly with `em` and can be predicted from a measurement pass.
    unsafe fn draw_glyph(dc: HDC, em: i32, dx: i32, dy: i32) {
        let font = CreateFontW(
            -em,
            0,
            0,
            0,
            // Regular: Segoe UI Symbol has no bold face, and GDI's
            // synthetic bold smears small glyphs.
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            // Segoe UI proper has no U+2318; don't rely on font linking.
            w!("Segoe UI Symbol"),
        );
        let old_font = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(0));
        SetBkMode(dc, TRANSPARENT);
        let mut rect = RECT {
            left: dx,
            top: dy,
            right: dx + em * 2,
            bottom: dy + em * 2,
        };
        let mut glyph: Vec<u16> = "⌘".encode_utf16().collect();
        DrawTextW(dc, &mut glyph, &mut rect, DT_SINGLELINE | DT_NOCLIP);
        let _ = GdiFlush();
        SelectObject(dc, old_font);
        let _ = DeleteObject(font.into());
    }

    /// Ink bounding box (left, top, right, bottom; inclusive) of black
    /// coverage on a white width×width canvas. None if nothing was drawn.
    fn ink_bbox(px: &[u32], width: i32) -> Option<(i32, i32, i32, i32)> {
        let (mut l, mut t, mut r, mut b) = (i32::MAX, i32::MAX, -1, -1);
        for (i, p) in px.iter().enumerate() {
            if *p & 0xFF != 0xFF {
                let (x, y) = (i as i32 % width, i as i32 / width);
                l = l.min(x);
                t = t.min(y);
                r = r.max(x);
                b = b.max(y);
            }
        }
        (r >= 0).then_some((l, t, r, b))
    }

    /// Draw "⌘" into a size×size ARGB bitmap and wrap it in an HICON.
    /// Runtime GDI drawing avoids shipping and embedding an .ico resource.
    /// A glyph's ink is a font-specific fraction of the em, so draw once on
    /// a scratch canvas to measure it, then redraw scaled and centered so
    /// the ink fills the icon box.
    unsafe fn tray_icon(size: i32) -> Result<HICON> {
        let size = size.max(16);
        let dc = CreateCompatibleDC(None);

        // Measurement pass at em = size on a 2×2-em scratch canvas — the
        // line box exceeds the em, and clipped ink would skew the numbers.
        let (scratch, sbits) = white_canvas(dc, size * 2)?;
        let old_bmp = SelectObject(dc, scratch.into());
        draw_glyph(dc, size, 0, 0);
        let spx = std::slice::from_raw_parts(sbits, ((size * 2) * (size * 2)) as usize);
        let (l, t, r, b) = ink_bbox(spx, size * 2).context("blank glyph")?;

        // Scale the em so the ink fills the box less a 1 px margin per side
        // (absorbs hinting jitter), then center the predicted ink box.
        let ink = (r - l + 1).max(b - t + 1);
        let em = size * (size - 2) / ink;
        let dx = (size - (r - l + 1) * em / size) / 2 - l * em / size;
        let dy = (size - (b - t + 1) * em / size) / 2 - t * em / size;

        let (color, bits) = white_canvas(dc, size)?;
        SelectObject(dc, color.into());
        let _ = DeleteObject(scratch.into());
        draw_glyph(dc, em, dx, dy);

        // Alpha = glyph coverage (black on white), premultiplied. Black glyph:
        // stands out on the gray/light taskbar.
        let px = std::slice::from_raw_parts_mut(bits, (size * size) as usize);
        for p in px.iter_mut() {
            let a = 0xFF - (*p & 0xFF);
            *p = a << 24;
        }
        SelectObject(dc, old_bmp);
        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(mask.into());
        let _ = DeleteObject(color.into());
        let _ = DeleteDC(dc);
        Ok(icon.context("CreateIconIndirect")?)
    }
}
