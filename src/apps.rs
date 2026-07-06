//! Window enumeration, grouping and activation (M2).
//!
//! The Windows half ports AltAppSwitcher's proven pieces: the
//! `IsEligibleWindow` filter, the UWP `FindActualPID` child-window walk and
//! the `AttachThreadInput` activation dance with its hung-foreground guard.
//! The pure functions at the top are unit-tested on any host.

/// Group items by key, preserving first-seen order of groups and members.
/// Fed with windows in z-order this yields apps most-recently-used first,
/// each group's first member being the app's topmost window.
pub fn group_by_key<T, K: PartialEq>(items: Vec<(T, K)>) -> Vec<(K, Vec<T>)> {
    let mut groups: Vec<(K, Vec<T>)> = Vec::new();
    for (item, key) in items {
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(item),
            None => groups.push((key, vec![item])),
        }
    }
    groups
}

/// Selection index moved one step forward or backward, wrapping at the ends.
pub fn step_index(len: usize, index: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    (index + if forward { 1 } else { len - 1 }) % len
}

/// Area-average resample of a square premultiplied-BGRA image. Averaging in
/// premultiplied space keeps fully transparent pixels black, so icon edges
/// blend cleanly at any background — unlike the shell's own scaler, which
/// resamples in straight alpha and bleeds the (usually white) color stored
/// under transparent pixels into the edges.
pub fn downscale_premul_bgra(src: &[u8], src_px: u32, dst_px: u32) -> Vec<u8> {
    let (s, d) = (src_px as usize, dst_px as usize);
    let ratio = s as f32 / d as f32;
    let mut out = vec![0u8; d * d * 4];
    for dy in 0..d {
        for dx in 0..d {
            // Source box covered by this destination pixel, with fractional
            // edge weights so non-integer ratios stay artifact-free.
            let (x0, x1) = (dx as f32 * ratio, (dx + 1) as f32 * ratio);
            let (y0, y1) = (dy as f32 * ratio, (dy + 1) as f32 * ratio);
            let mut acc = [0.0f32; 4];
            let mut area = 0.0f32;
            for sy in y0.floor() as usize..(y1.ceil() as usize).min(s) {
                let wy = (y1.min(sy as f32 + 1.0) - y0.max(sy as f32)).max(0.0);
                for sx in x0.floor() as usize..(x1.ceil() as usize).min(s) {
                    let w = wy * (x1.min(sx as f32 + 1.0) - x0.max(sx as f32)).max(0.0);
                    let p = &src[(sy * s + sx) * 4..][..4];
                    for c in 0..4 {
                        acc[c] += w * f32::from(p[c]);
                    }
                    area += w;
                }
            }
            let o = &mut out[(dy * d + dx) * 4..][..4];
            for c in 0..4 {
                o[c] = (acc[c] / area).round() as u8;
            }
        }
    }
    out
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::group_by_key;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use windows::core::{w, BOOL, PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_SUCCESS, HWND, LPARAM, LRESULT, SIZE, WPARAM,
    };
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::Win32::Storage::Packaging::Appx::GetApplicationUserModelId;
    use windows::Win32::System::Com::{CoCreateInstance, IBindCtx, CLSCTX_ALL};
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Shell::{
        IShellItemImageFactory, IVirtualDesktopManager, SHCreateItemFromParsingName,
        VirtualDesktopManager, SIIGBF, SIIGBF_ICONONLY, SIIGBF_SCALEUP,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GetClassNameW, GetForegroundWindow, GetParent,
        GetShellWindow, GetWindow, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId,
        IsIconic,
        IsWindowVisible, SendMessageTimeoutW, SetForegroundWindow, ShowWindowAsync, GWL_EXSTYLE,
        GW_OWNER, SMTO_ABORTIFHUNG, SW_RESTORE, WINDOW_EX_STYLE, WM_NULL, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };

    /// Ported from AltAppSwitcher: window classes that pass the style checks
    /// but must never appear in a switcher.
    const CLASS_BLOCKLIST: [&str; 7] = [
        "Shell_TrayWnd",
        "DV2ControlHost",
        "MsgrIMEWindowClass",
        "SysShadow",
        "Button",
        "Windows.UI.Core.CoreWindow",
        "Dwm",
    ];

    /// Eligible top-level windows in z-order (topmost first), each with its
    /// process executable path — the grouping key. With `all_desktops`,
    /// windows on other virtual desktops are included too.
    pub fn eligible_windows(all_desktops: bool) -> Vec<(HWND, String)> {
        struct Ctx {
            list: Vec<(HWND, String)>,
            vdm: Option<IVirtualDesktopManager>,
        }
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam.0 as *mut Ctx);
            if let Some(exe) = eligible_exe(hwnd, ctx.vdm.as_ref()) {
                ctx.list.push((hwnd, exe));
            }
            true.into()
        }
        let mut ctx = Ctx {
            list: Vec::new(),
            // If the manager cannot be created, "all" degrades to "current".
            vdm: all_desktops
                .then(|| unsafe { CoCreateInstance(&VirtualDesktopManager, None, CLSCTX_ALL).ok() })
                .flatten(),
        };
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut ctx as *mut _ as isize));
        }
        ctx.list
    }

    /// One running application for the app switcher: display name, icon key
    /// and all its windows in z-order (members[0] is the app's topmost).
    pub struct AppGroup {
        pub name: String,
        /// Shell parsing name for [`icon_bgra`].
        pub icon: String,
        pub windows: Vec<HWND>,
    }

    /// One group per running app, most-recently-used first.
    pub fn app_groups(all_desktops: bool) -> Vec<AppGroup> {
        group_by_key(eligible_windows(all_desktops))
            .into_iter()
            .map(|(exe, windows)| AppGroup {
                name: display_name(&exe),
                icon: icon_source(windows[0], &exe),
                windows,
            })
            .collect()
    }

    /// Shell parsing name whose icon represents the app: packaged (UWP)
    /// processes resolve through their `shell:AppsFolder` entry — their exe
    /// under WindowsApps yields a blank icon — all others through the exe
    /// path itself.
    pub fn icon_source(hwnd: HWND, exe: &str) -> String {
        unsafe { app_user_model_id(hwnd) }
            .map(|id| format!("shell:AppsFolder\\{id}"))
            .unwrap_or_else(|| exe.to_string())
    }

    /// AUMID of the process owning the window; `None` for plain Win32 apps.
    unsafe fn app_user_model_id(hwnd: HWND) -> Option<String> {
        let proc = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            window_pid(hwnd, &class_name(hwnd)),
        )
        .ok()?;
        let mut buf = [0u16; 130]; // APPLICATION_USER_MODEL_ID_MAX_LENGTH
        let mut len = buf.len() as u32;
        let res = GetApplicationUserModelId(proc, &mut len, Some(PWSTR(buf.as_mut_ptr())));
        let _ = CloseHandle(proc);
        // len counts the terminating NUL.
        (res == ERROR_SUCCESS && len > 1).then(|| String::from_utf16_lossy(&buf[..len as usize - 1]))
    }

    /// FileDescription from the exe's version resource ("Visual Studio Code"),
    /// falling back to the file stem ("Code").
    pub fn display_name(exe: &str) -> String {
        unsafe { file_description(exe) }.unwrap_or_else(|| {
            std::path::Path::new(exe)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| exe.to_string())
        })
    }

    unsafe fn file_description(exe: &str) -> Option<String> {
        let wide: Vec<u16> = exe.encode_utf16().chain([0]).collect();
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, data.as_mut_ptr() as *mut _)
            .ok()?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        // First entry of the translation table; exes normally have exactly one.
        if !VerQueryValueW(
            data.as_ptr() as *const _,
            w!("\\VarFileInfo\\Translation"),
            &mut ptr,
            &mut len,
        )
        .as_bool()
            || len < 4
        {
            return None;
        }
        let lang = *(ptr as *const u16);
        let codepage = *(ptr as *const u16).add(1);
        let query: Vec<u16> =
            format!("\\StringFileInfo\\{lang:04X}{codepage:04X}\\FileDescription")
                .encode_utf16()
                .chain([0])
                .collect();
        if !VerQueryValueW(data.as_ptr() as *const _, PCWSTR(query.as_ptr()), &mut ptr, &mut len)
            .as_bool()
            || len == 0
        {
            return None;
        }
        let chars = std::slice::from_raw_parts(ptr as *const u16, len as usize);
        let name = String::from_utf16_lossy(chars)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Premultiplied BGRA pixels (px × px) of the shell icon behind a parsing
    /// name (exe path or `shell:AppsFolder` entry, see [`icon_source`]),
    /// cached for the process lifetime — cold extraction can take ~100 ms.
    pub fn icon_bgra(exe: &str, px: u32) -> Option<Vec<u8>> {
        thread_local! {
            static CACHE: RefCell<HashMap<String, Option<Vec<u8>>>> =
                RefCell::new(HashMap::new());
        }
        CACHE.with_borrow_mut(|cache| {
            cache
                .entry(exe.to_string())
                .or_insert_with(|| unsafe { load_icon_bgra(exe, px) })
                .clone()
        })
    }

    unsafe fn load_icon_bgra(exe: &str, px: u32) -> Option<Vec<u8>> {
        let wide: Vec<u16> = exe.encode_utf16().chain([0]).collect();
        let item: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>).ok()?;
        // Ask for the 256 px frame — the icon's native high-res image, so the
        // shell does no resampling of its own — and downscale to `px` here in
        // premultiplied space (see downscale_premul_bgra); asking the shell
        // for `px` directly leaves a white fringe around the edges. SCALEUP
        // stretches icons that lack a 256 frame instead of padding them.
        let src = px.max(256);
        let cx = src as i32;
        // GetImage output is AlphaBlend-ready: 32-bit premultiplied BGRA.
        let bitmap = item
            .GetImage(SIZE { cx, cy: cx }, SIIGBF(SIIGBF_ICONONLY.0 | SIIGBF_SCALEUP.0))
            .ok()?;
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: cx,
                biHeight: -cx, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = vec![0u8; (src * src * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            src,
            Some(bits.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        let _ = DeleteObject(bitmap.into());
        (lines != 0).then(|| {
            if src == px {
                bits
            } else {
                super::downscale_premul_bgra(&bits, src, px)
            }
        })
    }

    /// The foreground app's exe path and all its windows in z-order
    /// (foreground first). With `include_minimized` off, minimized windows
    /// are skipped — without restore-on-activate a minimized window cannot
    /// visibly take focus.
    pub fn foreground_app_windows(include_minimized: bool, all_desktops: bool) -> (String, Vec<HWND>) {
        unsafe {
            // The foreground window itself may be ineligible (e.g. a child
            // dialog); walk up until an eligible window names the app.
            // The foreground window is on the current desktop by definition.
            let mut fg = GetForegroundWindow();
            let exe = loop {
                if fg.0.is_null() {
                    return (String::new(), Vec::new());
                }
                if let Some(exe) = eligible_exe(fg, None) {
                    break exe;
                }
                fg = GetParent(fg).unwrap_or_default();
            };
            let windows = eligible_windows(all_desktops)
                .into_iter()
                .filter(|(w, e)| *e == exe && (include_minimized || !IsIconic(*w).as_bool()))
                .map(|(w, _)| w)
                .collect();
            (exe, windows)
        }
    }

    pub fn window_title(hwnd: HWND) -> String {
        unsafe {
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf) as usize;
            String::from_utf16_lossy(&buf[..n])
        }
    }

    /// Bring a window to the foreground, optionally restoring it first.
    pub fn activate(hwnd: HWND, restore_minimized: bool) {
        unsafe {
            if restore_minimized && IsIconic(hwnd).as_bool() {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            }
            // A background process may not steal focus; attaching input to
            // the foreground thread grants SetForegroundWindow the right.
            let attached = attach_to_foreground();
            let _ = SetForegroundWindow(hwnd);
            if let Some(tid) = attached {
                let _ = AttachThreadInput(GetCurrentThreadId(), tid, false);
            }
        }
    }

    /// AttachThreadInput to the foreground window's thread, guarded against
    /// hung foreground windows: attaching to a hung thread would hang us too,
    /// so probe with a 100 ms SendMessageTimeout first (as AAS does).
    unsafe fn attach_to_foreground() -> Option<u32> {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return None;
        }
        let probe =
            SendMessageTimeoutW(fg, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 100, None);
        if probe == LRESULT(0) {
            return None;
        }
        let tid = GetWindowThreadProcessId(fg, None);
        let cur = GetCurrentThreadId();
        if tid == 0 || tid == cur {
            return None;
        }
        AttachThreadInput(cur, tid, true).as_bool().then_some(tid)
    }

    /// Port of AAS `IsEligibleWindow`: `Some(exe path)` if the window belongs
    /// in a switcher, `None` otherwise. A desktop manager (`vdm`) keeps
    /// windows that are cloaked only because they live on another virtual
    /// desktop (desktop_filter = "all").
    unsafe fn eligible_exe(hwnd: HWND, vdm: Option<&IVirtualDesktopManager>) -> Option<String> {
        if hwnd == GetShellWindow() {
            return None; // the desktop
        }
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
        if ex.contains(WS_EX_TOOLWINDOW) {
            return None;
        }
        if ex.contains(WS_EX_TOPMOST) && !ex.contains(WS_EX_APPWINDOW) {
            return None;
        }
        // Owned windows (dialogs etc.) are represented by their owner.
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| o != hwnd);
        if owned && !ex.contains(WS_EX_APPWINDOW) {
            return None;
        }
        let class = class_name(hwnd);
        if CLASS_BLOCKLIST.contains(&class.as_str()) {
            return None;
        }
        // Cloaked = not really on screen: suspended UWP hosts and windows on
        // other virtual desktops.
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            // Cloaked on the current desktop (suspended UWP host) is never
            // eligible; on another desktop it is, when the filter is "all".
            let other_desktop = vdm.is_some_and(|v| {
                v.IsWindowOnCurrentVirtualDesktop(hwnd)
                    .map(|on| !on.as_bool())
                    .unwrap_or(false)
            });
            if !other_desktop {
                return None;
            }
        }
        exe_path(window_pid(hwnd, &class))
    }

    unsafe fn class_name(hwnd: HWND) -> String {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..n])
    }

    /// PID owning the window. UWP apps live inside an ApplicationFrameWindow
    /// host (ApplicationFrameHost.exe); the real app is the child window with
    /// a different PID (port of AAS `FindActualPID`).
    unsafe fn window_pid(hwnd: HWND, class: &str) -> u32 {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if class == "ApplicationFrameWindow" {
            struct Find {
                host: u32,
                child: u32,
            }
            unsafe extern "system" fn walk(hwnd: HWND, lparam: LPARAM) -> BOOL {
                let find = &mut *(lparam.0 as *mut Find);
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if pid != find.host {
                    find.child = pid;
                    return false.into();
                }
                true.into()
            }
            let mut find = Find { host: pid, child: 0 };
            let _ = EnumChildWindows(Some(hwnd), Some(walk), LPARAM(&mut find as *mut _ as isize));
            if find.child != 0 {
                return find.child;
            }
        }
        pid
    }

    /// Full executable path of a process; works across integrity levels
    /// (elevated targets) with only PROCESS_QUERY_LIMITED_INFORMATION.
    unsafe fn exe_path(pid: u32) -> Option<String> {
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let res =
            QueryFullProcessImageNameW(proc, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(proc);
        res.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_preserves_z_order_of_groups_and_members() {
        let groups = group_by_key(vec![(1, "a"), (2, "b"), (3, "a"), (4, "c"), (5, "b")]);
        assert_eq!(
            groups,
            vec![("a", vec![1, 3]), ("b", vec![2, 5]), ("c", vec![4])]
        );
    }

    #[test]
    fn grouping_empty_input() {
        assert!(group_by_key::<i32, &str>(vec![]).is_empty());
    }

    #[test]
    fn downscale_averages_in_premultiplied_space() {
        // 4×4, top half opaque white, bottom half fully transparent (all
        // zero, as premultiplied data is). One output pixel = plain average:
        // 50% alpha with matching color — no brightening at the edge.
        let mut src = vec![0u8; 4 * 4 * 4];
        src[..4 * 2 * 4].fill(255);
        assert_eq!(downscale_premul_bgra(&src, 4, 1), vec![128, 128, 128, 128]);
    }

    #[test]
    fn downscale_handles_fractional_ratio_and_identity() {
        // Uniform image stays uniform through fractional 5→2 boxes.
        let gray = vec![100u8; 5 * 5 * 4];
        assert_eq!(downscale_premul_bgra(&gray, 5, 2), vec![100u8; 2 * 2 * 4]);
        // Ratio 1 reproduces the input exactly.
        let ramp: Vec<u8> = (0..3 * 3 * 4).map(|i| i as u8).collect();
        assert_eq!(downscale_premul_bgra(&ramp, 3, 3), ramp);
    }

    #[test]
    fn step_index_wraps_both_ways() {
        assert_eq!(step_index(3, 0, true), 1);
        assert_eq!(step_index(3, 2, true), 0);
        assert_eq!(step_index(3, 0, false), 2);
        assert_eq!(step_index(3, 1, false), 0);
        assert_eq!(step_index(1, 0, true), 0);
        assert_eq!(step_index(0, 0, true), 0, "empty list must not divide by zero");
    }
}
