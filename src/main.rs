// Debug builds keep the console so hook events are visible with println!.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
// Pure state machine is cross-platform for `cargo test`; only tests use it off-Windows.
#[cfg_attr(not(windows), allow(dead_code))]
mod hook;

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
    use crate::config::Config;
    use anyhow::{ensure, Context, Result};
    use std::sync::atomic::{AtomicU32, Ordering};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        COLORREF, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT,
        WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject,
        DrawTextW, GdiFlush, SelectObject, SetBkMode, SetTextColor, ANTIALIASED_QUALITY,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
        DIB_RGB_COLORS, DT_CENTER, DT_SINGLELINE, DT_VCENTER, OUT_DEFAULT_PRECIS, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::{
        CreateMutexW, GetCurrentProcess, SetPriorityClass, WaitForSingleObject,
        REALTIME_PRIORITY_CLASS,
    };
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, ChangeWindowMessageFilterEx, CreateIconIndirect, CreatePopupMenu,
        CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
        GetCursorPos, GetMessageW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW,
        RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, HICON,
        HWND_BROADCAST, ICONINFO, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_ICONWARNING, MB_OK,
        MB_YESNO, MF_STRING, MSG, MSGFLT_ALLOW, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROY, WM_RBUTTONUP, WNDCLASSW,
    };

    const CLASS_NAME: PCWSTR = w!("win-app-switcher.main");
    const MUTEX_NAME: PCWSTR = w!("Global\\win-app-switcher-single-instance");
    const WM_TRAY: u32 = WM_APP + 1;
    const TRAY_ID: u32 = 1;
    const CMD_QUIT: usize = 1;

    /// Broadcast "please exit" message id, registered before the window exists.
    static EXIT_MSG: AtomicU32 = AtomicU32::new(0);

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

        // Switching must stay fast under full CPU load. REALTIME needs
        // elevation; unelevated the OS silently grants HIGH instead.
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), REALTIME_PRIORITY_CLASS);
        }

        // M0 only proves the config loads; later milestones consume it.
        let _config = match config_path().and_then(|p| Config::load(&p)) {
            Ok(c) => c,
            Err(e) => {
                alert(
                    &format!("{e:#}\n\nContinuing with default settings."),
                    Severity::Warning,
                );
                Config::default()
            }
        };

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
                hIcon: tray_icon().context("tray icon")?,
                ..Default::default()
            };
            for (dst, src) in nid.szTip.iter_mut().zip("win-app-switcher".encode_utf16()) {
                *dst = src;
            }
            Shell_NotifyIconW(NIM_ADD, &nid)
                .ok()
                .context("Shell_NotifyIconW")?;

            crate::hook::start(hwnd);

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
            // M1: the hook's events arrive here; M2 starts acting on them.
            m if m == crate::hook::WM_SWITCHER => {
                #[cfg(debug_assertions)]
                println!(
                    "main thread received: {:?}",
                    crate::hook::Event::from_wparam(wparam.0)
                );
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

    unsafe fn show_tray_menu(hwnd: HWND) {
        let Ok(menu) = CreatePopupMenu() else { return };
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
        if cmd.0 as usize == CMD_QUIT {
            let _ = DestroyWindow(hwnd);
        }
    }

    /// Draw "§" into a 32×32 ARGB bitmap and wrap it in an HICON.
    /// Runtime GDI drawing avoids shipping and embedding an .ico resource.
    unsafe fn tray_icon() -> Result<HICON> {
        const SIZE: i32 = 32;
        let dc = CreateCompatibleDC(None);
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: SIZE,
                biHeight: -SIZE, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color =
            CreateDIBSection(Some(dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0).context("DIB")?;
        // White canvas; the glyph is drawn black and the coverage becomes alpha below.
        std::ptr::write_bytes(bits as *mut u8, 0xFF, (SIZE * SIZE) as usize * 4);
        let old_bmp = SelectObject(dc, color.into());
        let font = CreateFontW(
            -(SIZE - 4),
            0,
            0,
            0,
            600, // semibold
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        );
        let old_font = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(0));
        SetBkMode(dc, TRANSPARENT);
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: SIZE,
            bottom: SIZE,
        };
        let mut glyph: Vec<u16> = "§".encode_utf16().collect();
        DrawTextW(dc, &mut glyph, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        let _ = GdiFlush();
        // Alpha = glyph coverage (black on white), premultiplied. Black glyph:
        // stands out on the gray/light taskbar.
        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (SIZE * SIZE) as usize);
        for p in px.iter_mut() {
            let a = 0xFF - (*p & 0xFF);
            *p = a << 24;
        }
        SelectObject(dc, old_font);
        SelectObject(dc, old_bmp);
        let _ = DeleteObject(font.into());
        let mask = CreateBitmap(SIZE, SIZE, 1, 1, None);
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
