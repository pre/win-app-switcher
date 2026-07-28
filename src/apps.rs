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

/// Identity of one app for grouping. A Chrome/Edge PWA tags each of its
/// windows with a per-window AppUserModelID; keying on that AUMID (when
/// present) splits the PWA into its own switcher entry instead of folding it
/// into the browser's `chrome.exe` group. Ordinary windows carry no AUMID and
/// key on the exe path, so a browser's own windows still share one group.
#[derive(Clone)]
pub struct AppKey {
    /// The exe path, always present: the display-name and icon fallback, and
    /// the grouping key when there is no AUMID.
    pub exe: String,
    /// The window's AUMID, set for PWA and packaged (UWP) windows.
    pub aumid: Option<String>,
}

impl AppKey {
    /// The grouping/identity string: the AUMID when the window carries one,
    /// else the exe path.
    pub fn id(&self) -> &str {
        self.aumid.as_deref().unwrap_or(&self.exe)
    }
}

/// Two windows group together iff they share an identity — same AUMID, or
/// (both browser-plain) same exe. The exe field is carried for display only
/// and deliberately excluded from equality.
impl PartialEq for AppKey {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
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

/// Force BGRA pixels into premultiplied alpha when the source is straight
/// (unassociated) alpha. The shell's `GetImage` hands some icons back
/// premultiplied and others straight — packaged-app assets (1Password, Teams)
/// and a few Win32 apps (VS Code) come through straight — but the rest of the
/// pipeline assumes premultiplied. A straight-alpha edge pixel keeps its full-strength color
/// (often near-white antialiasing) under a partial alpha, which then blends as
/// a bright fringe. Valid premultiplied data can never have a color channel
/// exceed its alpha, so a single pixel that does proves the whole image is
/// straight and must be premultiplied; images already premultiplied are left
/// untouched.
pub fn premultiply_bgra(bits: &mut [u8]) {
    let straight = bits
        .chunks_exact(4)
        .any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if !straight {
        return;
    }
    for p in bits.chunks_exact_mut(4) {
        let a = u32::from(p[3]);
        for c in &mut p[..3] {
            // Round-to-nearest premultiply: (channel * alpha) / 255.
            *c = ((u32::from(*c) * a + 127) / 255) as u8;
        }
    }
}

/// Read one attribute out of a single XML start tag, with or without its
/// leading `<`.
///
/// The tag is walked attribute by attribute instead of searched for `name`:
/// every value is consumed as part of the scan, so text inside a value
/// (`Other="foo Id=bar"`) can never be mistaken for the attribute itself, and
/// a namespaced key (`uap10:HostId`) never matches a bare `Id`. A malformed
/// attribute is skipped and the scan continues; only an unterminated quote
/// gives up, because the remainder of the tag is unparseable after it.
fn xml_attr(tag: &str, name: &str) -> Option<String> {
    let rest = tag.strip_prefix('<').unwrap_or(tag);
    let mut rest = &rest[rest.find(char::is_whitespace)?..];
    loop {
        rest = rest.trim_start();
        if rest.is_empty() || rest.starts_with('>') || rest.starts_with("/>") {
            return None;
        }
        let key_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '>')
            .unwrap_or(rest.len());
        let key = &rest[..key_end];
        if key.is_empty() {
            // Stray `=` with no key: drop it so the scan keeps advancing.
            rest = &rest[rest.chars().next()?.len_utf8()..];
            continue;
        }
        rest = rest[key_end..].trim_start();
        let Some(after_eq) = rest.strip_prefix('=') else {
            continue;
        };
        let value = after_eq.trim_start();
        let (found, next) = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let body = &value[quote.len_utf8()..];
                let end = body.find(quote)?;
                (&body[..end], &body[end + quote.len_utf8()..])
            }
            _ => {
                let end = value
                    .find(|c: char| c.is_whitespace() || c == '>')
                    .unwrap_or(value.len());
                (&value[..end], &value[end..])
            }
        };
        if key == name {
            return Some(found.to_string());
        }
        rest = next;
    }
}

fn use_direct_package_logo(aumid: &str) -> bool {
    aumid == "Claude_pzs8sxrjxfjjc!Claude"
}

fn use_icon_background(source: &str) -> bool {
    source.starts_with("shell:AppsFolder\\Chrome._crx_cadlkdcgmdikeeg.")
}

fn round_icon_background(source: &str) -> bool {
    use_icon_background(source)
        || source == "shell:AppsFolder\\f6cbcda5-b021-4d0e-9fd7-4c5b41ea0aad"
}

fn round_premul_bgra_corners(bits: &mut [u8], px: u32, radius: u32) {
    let px = px as usize;
    let radius = radius.min(px as u32 / 2) as usize;
    debug_assert_eq!(bits.len(), px * px * 4);
    for y in 0..radius {
        for x in 0..radius {
            let dx = radius as f32 - x as f32 - 0.5;
            let dy = radius as f32 - y as f32 - 0.5;
            if dx * dx + dy * dy <= (radius * radius) as f32 {
                continue;
            }
            for (px_x, px_y) in [
                (x, y),
                (px - 1 - x, y),
                (x, px - 1 - y),
                (px - 1 - x, px - 1 - y),
            ] {
                bits[(px_y * px + px_x) * 4..][..4].fill(0);
            }
        }
    }
}

fn manifest_logo_path(manifest: &str, aumid: &str) -> Option<String> {
    let (_, app_id) = aumid.split_once('!')?;
    let mut rest = manifest;
    while let Some(start) = rest.find("<Application ") {
        rest = &rest[start..];
        let tag_end = rest.find('>')?;
        let tag = &rest[..=tag_end];
        if tag[..tag_end].trim_end().ends_with('/') {
            rest = &rest[tag_end + 1..];
            continue;
        }
        let Some(close) = rest[tag_end + 1..].find("</Application>") else {
            break;
        };
        let close = close + tag_end + 1;
        let body = &rest[tag_end + 1..close];
        if xml_attr(tag, "Id").as_deref() == Some(app_id) {
            let visual_start = body.find("VisualElements")?;
            let visual = &body[visual_start..];
            let visual_end = visual.find('>')?;
            return xml_attr(&visual[..=visual_end], "Square150x150Logo");
        }
        rest = &rest[close + "</Application>".len()..];
    }
    None
}

#[cfg(windows)]
pub use win::*;

#[cfg(windows)]
mod win {
    use super::{
        group_by_key, manifest_logo_path, round_icon_background, round_premul_bgra_corners,
        use_direct_package_logo, use_icon_background, AppKey,
    };
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
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
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, CLSCTX_ALL};
    use windows::Win32::System::Threading::{
        AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, SHGetPropertyStoreForWindow,
    };
    use windows::Win32::UI::Shell::{
        IShellItem, IShellItemImageFactory, IVirtualDesktopManager, SHCreateItemFromParsingName,
        VirtualDesktopManager, SIGDN_NORMALDISPLAY, SIIGBF, SIIGBF_ICONBACKGROUND, SIIGBF_ICONONLY,
        SIIGBF_SCALEUP, SIIGBF_THUMBNAILONLY,
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
    /// grouping key ([`AppKey`]: AUMID when the window tags one, else the exe
    /// path). With `all_desktops`, windows on other virtual desktops are
    /// included too.
    pub fn eligible_windows(all_desktops: bool) -> Vec<(HWND, AppKey)> {
        struct Ctx {
            list: Vec<(HWND, AppKey)>,
            vdm: Option<IVirtualDesktopManager>,
        }
        unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let ctx = &mut *(lparam.0 as *mut Ctx);
            if let Some(key) = eligible_key(hwnd, ctx.vdm.as_ref()) {
                ctx.list.push((hwnd, key));
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
            .map(|(key, windows)| AppGroup {
                name: app_name(&key),
                icon: icon_source(&key),
                windows,
            })
            .collect()
    }

    /// Display name for a group: a PWA/UWP app resolves its AUMID to the real
    /// app name ("ChatGPT"), so it doesn't read as "Google Chrome". Plain
    /// windows fall back to the exe's FileDescription.
    pub fn app_name(key: &AppKey) -> String {
        key.aumid
            .as_deref()
            .and_then(|id| unsafe { aumid_display_name(id) })
            .unwrap_or_else(|| display_name(&key.exe))
    }

    /// Shell parsing name whose icon represents the app. Claude's packaged app
    /// uses its original 150×150 manifest logo because AppsFolder renders it
    /// visibly soft. Other packaged apps and Chrome/Edge PWAs keep using
    /// AppsFolder, which selects their unplated/app-list artwork correctly.
    /// Plain Win32 apps use the exe's embedded icon.
    pub fn icon_source(key: &AppKey) -> String {
        key.aumid
            .as_deref()
            .filter(|id| use_direct_package_logo(id))
            .and_then(|id| packaged_logo_source(&key.exe, id))
            .or_else(|| {
                key.aumid
                    .as_deref()
                    .map(|id| format!("shell:AppsFolder\\{id}"))
            })
            .unwrap_or_else(|| key.exe.clone())
    }

    fn packaged_logo_source(exe: &str, aumid: &str) -> Option<String> {
        let exe = std::path::Path::new(exe);
        for dir in exe.ancestors().skip(1) {
            let manifest_path = dir.join("AppxManifest.xml");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = std::fs::read_to_string(manifest_path).ok()?;
            let relative = manifest_logo_path(&manifest, aumid)?;
            let logo = dir.join(relative.replace('\\', std::path::MAIN_SEPARATOR_STR));
            if logo.is_file() {
                #[cfg(debug_assertions)]
                println!("package logo: {}", logo.display());
                return Some(logo.to_string_lossy().into_owned());
            }
            return None;
        }
        None
    }

    /// The `shell:AppsFolder\{aumid}` shell item, or `None` when the AUMID
    /// names no registered app. Only installed PWAs and UWP apps have an
    /// AppsFolder entry; a plain browser window's own AUMID resolves to
    /// nothing, so this is also how we tell a real app AUMID from Chrome's
    /// per-window browser AUMID.
    unsafe fn aumid_shell_item(aumid: &str) -> Option<IShellItem> {
        let parsing: Vec<u16> = format!("shell:AppsFolder\\{aumid}")
            .encode_utf16()
            .chain([0])
            .collect();
        SHCreateItemFromParsingName(PCWSTR(parsing.as_ptr()), None::<&IBindCtx>).ok()
    }

    /// The shell display name of `shell:AppsFolder\{aumid}` — the app's real
    /// name, e.g. "ChatGPT" for a Chrome PWA. `None` if the AUMID doesn't
    /// resolve to a shell item.
    unsafe fn aumid_display_name(aumid: &str) -> Option<String> {
        let item = aumid_shell_item(aumid)?;
        let name = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        text.filter(|t| !t.is_empty())
    }

    /// AppUserModelID identifying the app behind a window, or `None` for a
    /// plain Win32 app. Two sources, in order: the owning process's packaged
    /// identity (UWP), then the window's own property store — Chrome and Edge
    /// stamp each PWA window with `PKEY_AppUserModel_ID`, which is how the
    /// taskbar pins and icons a PWA apart from the browser.
    ///
    /// Chrome and Edge also stamp their *ordinary* browser windows with a
    /// per-profile AUMID that names no AppsFolder app; accepting it would key
    /// the browser group on a `shell:AppsFolder` path that resolves to a blank
    /// icon. So an AUMID counts only when it resolves to a real AppsFolder app;
    /// otherwise the window falls back to grouping and iconing by its exe.
    unsafe fn window_aumid(hwnd: HWND) -> Option<String> {
        let aumid = process_aumid(hwnd).or_else(|| window_prop_aumid(hwnd))?;
        aumid_shell_item(&aumid).map(|_| aumid)
    }

    /// AUMID from the owning process's packaged identity; `None` for
    /// unpackaged processes (plain Win32, Chrome, Electron apps).
    unsafe fn process_aumid(hwnd: HWND) -> Option<String> {
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

    /// AUMID stamped on the window's property store under
    /// `PKEY_AppUserModel_ID`. Chrome and Edge set this per PWA window so the
    /// shell treats each PWA as its own app; ordinary browser windows leave it
    /// unset, which surfaces here as an empty string and yields `None`. The
    /// returned PROPVARIANT frees itself on drop; the alloc'd string is ours
    /// to free with CoTaskMemFree.
    unsafe fn window_prop_aumid(hwnd: HWND) -> Option<String> {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let id = PropVariantToStringAlloc(&value).ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text.filter(|t| !t.is_empty())
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
        // Some resources (Spotify) report a len past the value's own NUL, so
        // the slice runs into the next key's bytes. Stop at the first NUL
        // rather than trusting len, then trim.
        let name = String::from_utf16_lossy(chars);
        let name = name.split('\0').next().unwrap_or("").trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Premultiplied BGRA pixels (px × px) of the shell image behind a parsing
    /// name (exe path, package PNG or `shell:AppsFolder` entry). The 256 px
    /// master is cached for the process lifetime and downscaled per request.
    pub fn icon_bgra(source: &str, px: u32) -> Option<Vec<u8>> {
        const MASTER: u32 = 256;
        const RETRY_FAILED: Duration = Duration::from_secs(60);
        thread_local! {
            // Ok = image pixels, Err = when extraction last failed.
            static CACHE: RefCell<HashMap<String, Result<Vec<u8>, Instant>>> =
                RefCell::new(HashMap::new());
        }
        let master = CACHE.with_borrow_mut(|cache| {
            match cache.get(source) {
                Some(Ok(v)) => return Some(v.clone()),
                Some(Err(t)) if t.elapsed() < RETRY_FAILED => return None,
                _ => {}
            }
            let loaded = unsafe { load_icon_bgra(source, MASTER) };
            cache.insert(source.to_string(), loaded.clone().ok_or_else(Instant::now));
            loaded
        })?;
        Some(if px == MASTER {
            master
        } else {
            super::downscale_premul_bgra(&master, MASTER, px)
        })
    }

    unsafe fn load_icon_bgra(source: &str, px: u32) -> Option<Vec<u8>> {
        let wide: Vec<u16> = source.encode_utf16().chain([0]).collect();
        let item: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>).ok()?;
        let cx = px as i32;
        let kind = if std::path::Path::new(source)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            SIIGBF_THUMBNAILONLY.0
        } else {
            SIIGBF_ICONONLY.0
        };
        let background = if use_icon_background(source) {
            SIIGBF_ICONBACKGROUND.0
        } else {
            0
        };
        let bitmap = item
            .GetImage(
                SIZE { cx, cy: cx },
                SIIGBF(kind | background | SIIGBF_SCALEUP.0),
            )
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
        let mut bits = vec![0u8; (px * px * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            px,
            Some(bits.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );
        let _ = DeleteDC(dc);
        let _ = DeleteObject(bitmap.into());
        if lines == 0 {
            return None;
        }
        // Some icons arrive in straight alpha despite the premultiplied
        // contract the rest of the pipeline relies on; correct them here so
        // edges don't blend as a bright fringe.
        super::premultiply_bgra(&mut bits);
        if round_icon_background(source) {
            round_premul_bgra_corners(&mut bits, px, px / 5);
        }
        Some(bits)
    }

    /// The foreground app's grouping key and all its windows in z-order
    /// (foreground first). Keyed by [`AppKey`] so a focused PWA scopes to its
    /// own windows, not to every Chrome window. With `include_minimized` off,
    /// minimized windows are skipped — without restore-on-activate a minimized
    /// window cannot visibly take focus.
    pub fn foreground_app_windows(
        include_minimized: bool,
        all_desktops: bool,
    ) -> (Option<AppKey>, Vec<HWND>) {
        unsafe {
            // The foreground window itself may be ineligible (e.g. a child
            // dialog); walk up until an eligible window names the app.
            // The foreground window is on the current desktop by definition.
            let mut fg = GetForegroundWindow();
            let key = loop {
                if fg.0.is_null() {
                    return (None, Vec::new());
                }
                if let Some(key) = eligible_key(fg, None) {
                    break key;
                }
                fg = GetParent(fg).unwrap_or_default();
            };
            let windows = eligible_windows(all_desktops)
                .into_iter()
                .filter(|(w, k)| *k == key && (include_minimized || !IsIconic(*w).as_bool()))
                .map(|(w, _)| w)
                .collect();
            (Some(key), windows)
        }
    }

    pub fn window_title(hwnd: HWND) -> String {
        unsafe {
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, &mut buf) as usize;
            String::from_utf16_lossy(&buf[..n])
        }
    }

    /// Immersive shell overlays — the open Start menu, search and taskbar
    /// jump lists / context menus — live in z-order bands above every
    /// WS_EX_TOPMOST window, so no z-order or foreground trick can put our
    /// dialog over them. Like any menu they dismiss on ESC, so when one
    /// holds the foreground, send it an injected ESC (which our own hook
    /// passes through) before taking over.
    unsafe fn dismiss_shell_overlay() {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return;
        }
        let class = class_name(fg);
        let overlay = match class.as_str() {
            // Win11 taskbar flyouts, the taskbar's XAML host and the
            // taskbar's own Win32 menu loop.
            "Xaml_WindowedPopupClass" | "XamlExplorerHostIslandWindow" | "Shell_TrayWnd" => true,
            // Start menu, search and Win10 shell flyouts host their UI in a
            // CoreWindow of a dedicated shell process.
            "Windows.UI.Core.CoreWindow" => {
                let mut pid = 0u32;
                GetWindowThreadProcessId(fg, Some(&mut pid));
                let name = exe_path(pid)
                    .map(|p| p.rsplit('\\').next().unwrap_or("").to_ascii_lowercase())
                    .unwrap_or_default();
                #[cfg(debug_assertions)]
                println!("foreground CoreWindow host: {name}");
                matches!(
                    name.as_str(),
                    "startmenuexperiencehost.exe"
                        | "searchhost.exe"
                        | "searchapp.exe"
                        | "searchui.exe"
                        | "shellexperiencehost.exe"
                )
            }
            _ => false,
        };
        #[cfg(debug_assertions)]
        println!(
            "foreground '{class}': {}",
            if overlay { "shell overlay, inject ESC to dismiss" } else { "not a shell overlay" }
        );
        if overlay {
            crate::hook::inject_key(VK_ESCAPE.0);
        }
    }

    /// Bring a window to the foreground, optionally restoring it first.
    pub fn activate(hwnd: HWND, restore_minimized: bool) {
        unsafe {
            dismiss_shell_overlay();
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
            // The attach dance fails against immersive shell surfaces (open
            // Start menu, search, taskbar jump lists), which also live in a
            // z-band above any WS_EX_TOPMOST window, so a failed activation
            // leaves the dialog drawn underneath them. Injecting a key makes
            // this process the last input sender, which re-arms
            // SetForegroundWindow; once we are foreground the shell surface
            // dismisses itself.
            if GetForegroundWindow() != hwnd {
                crate::hook::inject_dummy();
                let _ = SetForegroundWindow(hwnd);
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

    /// Port of AAS `IsEligibleWindow`: `Some(AppKey)` if the window belongs in
    /// a switcher, `None` otherwise. The key carries the exe path plus the
    /// window's AUMID (when it tags one), so PWAs group apart from the
    /// browser. A desktop manager (`vdm`) keeps windows that are cloaked only
    /// because they live on another virtual desktop (desktop_filter = "all").
    unsafe fn eligible_key(hwnd: HWND, vdm: Option<&IVirtualDesktopManager>) -> Option<AppKey> {
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
        let exe = exe_path(window_pid(hwnd, &class))?;
        Some(AppKey { exe, aumid: window_aumid(hwnd) })
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

    fn key(exe: &str, aumid: Option<&str>) -> AppKey {
        AppKey { exe: exe.to_string(), aumid: aumid.map(str::to_string) }
    }

    #[test]
    fn pwas_split_from_browser_by_aumid() {
        // Two chrome.exe windows tagged with different PWA AUMIDs, plus a
        // plain browser window (no AUMID). Each PWA is its own group; the
        // plain window keys on the exe.
        let chatgpt = key("chrome.exe", Some("Chrome._crx_chatgpt"));
        let outlook = key("chrome.exe", Some("Chrome._crx_outlook"));
        let browser = key("chrome.exe", None);
        let groups = group_by_key(vec![
            (1, chatgpt.clone()),
            (2, outlook.clone()),
            (3, browser.clone()),
        ]);
        let ids: Vec<&str> = groups.iter().map(|(k, _)| k.id()).collect();
        assert_eq!(ids, vec!["Chrome._crx_chatgpt", "Chrome._crx_outlook", "chrome.exe"]);
        assert!(groups.iter().all(|(_, members)| members.len() == 1));
    }

    #[test]
    fn same_aumid_windows_merge_across_exe() {
        // Same PWA identity groups its windows together; the exe field is
        // display-only and excluded from equality.
        let a = key("chrome.exe", Some("Chrome._crx_chatgpt"));
        let b = key("chrome.exe", Some("Chrome._crx_chatgpt"));
        let groups = group_by_key(vec![(1, a), (2, b)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1, vec![1, 2]);
    }

    #[test]
    fn plain_windows_group_by_exe() {
        // No AUMID: ordinary browsing stays one group per exe.
        let groups = group_by_key(vec![
            (1, key("chrome.exe", None)),
            (2, key("code.exe", None)),
            (3, key("chrome.exe", None)),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].1, vec![1, 3]);
        assert_eq!(groups[1].1, vec![2]);
    }

    #[test]
    fn rounded_background_is_limited_to_square_pwa_icons() {
        assert!(round_icon_background(
            "shell:AppsFolder\\Chrome._crx_cadlkdcgmdikeeg.UserData.Profile1"
        ));
        assert!(round_icon_background(
            "shell:AppsFolder\\f6cbcda5-b021-4d0e-9fd7-4c5b41ea0aad"
        ));
        assert!(!round_icon_background(
            "shell:AppsFolder\\91750D7E.Slack_8she8kybcnzg4!Slack"
        ));
    }

    #[test]
    fn rounded_background_mask_clears_only_corner_pixels() {
        let mut bits = vec![255u8; 8 * 8 * 4];
        round_premul_bgra_corners(&mut bits, 8, 2);

        for (x, y) in [(0, 0), (7, 0), (0, 7), (7, 7)] {
            assert_eq!(&bits[(y * 8 + x) * 4..][..4], &[0, 0, 0, 0]);
        }
        assert_eq!(&bits[(0 * 8 + 1) * 4..][..4], &[255, 255, 255, 255]);
        assert_eq!(&bits[(4 * 8 + 4) * 4..][..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn icon_background_is_limited_to_chatgpt_pwa() {
        assert!(use_icon_background(
            "shell:AppsFolder\\Chrome._crx_cadlkdcgmdikeeg.UserData.Profile1"
        ));
        assert!(!use_icon_background(
            "shell:AppsFolder\\91750D7E.Slack_8she8kybcnzg4!Slack"
        ));
        assert!(!use_icon_background(
            "shell:AppsFolder\\Chrome._crx_other.UserData.Profile1"
        ));
    }

    #[test]
    fn direct_package_logo_is_limited_to_known_blurry_apps() {
        assert!(use_direct_package_logo("Claude_pzs8sxrjxfjjc!Claude"));
        assert!(!use_direct_package_logo("91750D7E.Slack_8she8kybcnzg4!Slack"));
        assert!(!use_direct_package_logo("MSTeams_8wekyb3d8bbwe!MSTeams"));
    }

    #[test]
    fn xml_attr_ignores_name_inside_an_attribute_value() {
        // "Id=bar" sits inside another attribute's value, preceded by a space:
        // only the real Id attribute may match.
        let tag = r#"<Application Other="foo Id=bar" Id="real">"#;
        assert_eq!(xml_attr(tag, "Id").as_deref(), Some("real"));
        assert_eq!(xml_attr(r#"<Application Other="foo Id=bar">"#, "Id"), None);
    }

    #[test]
    fn xml_attr_requires_a_whole_key_match() {
        let tag = r#"<Application uap10:HostId="Hosted" Id="Claude">"#;
        assert_eq!(xml_attr(tag, "Id").as_deref(), Some("Claude"));
        assert_eq!(xml_attr(tag, "HostId"), None);
        assert_eq!(xml_attr(tag, "uap10:HostId").as_deref(), Some("Hosted"));
    }

    #[test]
    fn xml_attr_continues_past_malformed_attributes() {
        // Unquoted value, valueless attribute and a stray '=' each skip ahead
        // rather than abandoning the scan.
        let tag = r#"<Application Broken=bar Flag Other = = Id="Claude">"#;
        assert_eq!(xml_attr(tag, "Id").as_deref(), Some("Claude"));
        assert_eq!(xml_attr(tag, "Broken").as_deref(), Some("bar"));
    }

    #[test]
    fn xml_attr_handles_quoting_and_tag_shapes() {
        // Single quotes, a self-closing tag and a leading '<' that the
        // VisualElements caller strips off before slicing.
        let tag = r"uap:VisualElements Square150x150Logo='Assets\Claude.png' />";
        assert_eq!(
            xml_attr(tag, "Square150x150Logo").as_deref(),
            Some(r"Assets\Claude.png")
        );
        // An unterminated quote leaves nothing parseable behind it.
        assert_eq!(xml_attr(r#"<Application Id="Claude>"#, "Id"), None);
        // A tag with no attributes at all.
        assert_eq!(xml_attr("<Applications>", "Id"), None);
    }

    #[test]
    fn manifest_logo_selects_matching_packaged_application() {
        let manifest = r#"
            <Package xmlns:uap="http://schemas.microsoft.com/appx/manifest/uap/windows10">
              <Applications>
                <Application Id="Other">
                  <uap:VisualElements Square150x150Logo="Assets\Other.png" />
                </Application>
                <Application uap10:HostId="Hosted" Id="Claude">
                  <uap:VisualElements
                    Square44x44Logo="Assets\Small.png"
                    Square150x150Logo="Assets\Claude.png" />
                </Application>
              </Applications>
            </Package>
        "#;

        assert_eq!(
            manifest_logo_path(manifest, "Package_family!Other"),
            Some(r"Assets\Other.png".into())
        );
        assert_eq!(
            manifest_logo_path(manifest, "Claude_pzs8sxrjxfjjc!Claude"),
            Some(r"Assets\Claude.png".into())
        );
    }

    #[test]
    fn manifest_logo_rejects_non_packaged_and_unknown_apps() {
        let manifest = r#"<Application Id="Claude"><uap:VisualElements Square150x150Logo="Assets\Claude.png" /></Application>"#;

        assert_eq!(manifest_logo_path(manifest, "not-an-aumid"), None);
        assert_eq!(
            manifest_logo_path(manifest, "Claude_pzs8sxrjxfjjc!Other"),
            None
        );
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
    fn premultiply_corrects_straight_alpha() {
        // A straight-alpha edge pixel: white antialiasing under 50% alpha.
        // B,G,R (255) exceed alpha (128), which is impossible for premultiplied
        // data, so the whole image is premultiplied down to 128.
        let mut bits = vec![255, 255, 255, 128];
        premultiply_bgra(&mut bits);
        assert_eq!(bits, vec![128, 128, 128, 128]);
    }

    #[test]
    fn premultiply_leaves_premultiplied_untouched() {
        // Already premultiplied (every channel <= alpha): a no-op.
        let orig = vec![10, 20, 30, 40, 0, 0, 0, 0, 200, 200, 200, 255];
        let mut bits = orig.clone();
        premultiply_bgra(&mut bits);
        assert_eq!(bits, orig);
    }

    #[test]
    fn premultiply_rounds_and_zeroes_transparent() {
        // Fully transparent straight pixel drops its color; a partial pixel
        // rounds to nearest. The first pixel (color > alpha) marks it straight.
        let mut bits = vec![255, 255, 255, 0, 100, 200, 50, 128];
        premultiply_bgra(&mut bits);
        // (100*128+127)/255 = 50, (200*128+127)/255 = 100, (50*128+127)/255 = 25.
        assert_eq!(bits, vec![0, 0, 0, 0, 50, 100, 25, 128]);
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
