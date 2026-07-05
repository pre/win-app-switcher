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

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::group_by_key;
    use windows::core::{BOOL, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, GetClassNameW, GetForegroundWindow, GetParent,
        GetShellWindow, GetWindow, GetWindowLongW, GetWindowThreadProcessId, IsIconic,
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
    /// process executable path — the grouping key.
    pub fn eligible_windows() -> Vec<(HWND, String)> {
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let list = &mut *(lparam.0 as *mut Vec<(HWND, String)>);
            if let Some(exe) = eligible_exe(hwnd) {
                list.push((hwnd, exe));
            }
            true.into()
        }
        let mut list: Vec<(HWND, String)> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut list as *mut _ as isize));
        }
        list
    }

    /// One entry per running app: the topmost window of each executable,
    /// most-recently-used first. Activating it brings the app forward.
    pub fn app_windows() -> Vec<HWND> {
        group_by_key(eligible_windows())
            .into_iter()
            .map(|(_, members)| members[0])
            .collect()
    }

    /// All windows of the foreground app in z-order (foreground first).
    /// With `include_minimized` off, minimized windows are skipped — without
    /// restore-on-activate a minimized window cannot visibly take focus.
    pub fn foreground_app_windows(include_minimized: bool) -> Vec<HWND> {
        unsafe {
            // The foreground window itself may be ineligible (e.g. a child
            // dialog); walk up until an eligible window names the app.
            let mut fg = GetForegroundWindow();
            let exe = loop {
                if fg.0.is_null() {
                    return Vec::new();
                }
                if let Some(exe) = eligible_exe(fg) {
                    break exe;
                }
                fg = GetParent(fg).unwrap_or_default();
            };
            eligible_windows()
                .into_iter()
                .filter(|(w, e)| *e == exe && (include_minimized || !IsIconic(*w).as_bool()))
                .map(|(w, _)| w)
                .collect()
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
    /// in a switcher, `None` otherwise.
    unsafe fn eligible_exe(hwnd: HWND) -> Option<String> {
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
        // ponytail: cloak check doubles as desktop_filter="current"; honoring
        // desktop_filter="all" needs IVirtualDesktopManager, wire it in M5.
        let mut cloaked: u32 = 0;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None;
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
    fn step_index_wraps_both_ways() {
        assert_eq!(step_index(3, 0, true), 1);
        assert_eq!(step_index(3, 2, true), 0);
        assert_eq!(step_index(3, 0, false), 2);
        assert_eq!(step_index(3, 1, false), 0);
        assert_eq!(step_index(1, 0, true), 0);
        assert_eq!(step_index(0, 0, true), 0, "empty list must not divide by zero");
    }
}
