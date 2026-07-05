//! Switcher dialogs: WIN+TAB icon row (M3) and WIN+§ window list (M4).
//!
//! Layout arithmetic is pure and unit-tested on any host; the Windows glue
//! below renders it with Direct2D + DirectWrite and feeds mouse input back
//! to the main thread as the same [`crate::hook::Event`]s the hook posts.
//!
//! The input recipe is AltAppSwitcher's proven one: WS_POPUP topmost tool
//! window, made foreground, mouse captured with SetCapture so a click
//! outside the panel is seen (and cancels). Rendering goes through a
//! layered window (UpdateLayeredWindow + premultiplied DIB): the rounded
//! panel corners are drawn with real alpha, macOS style, on any Windows
//! version. Both dialogs share the window class, renderer and input path;
//! only the layout differs.

/// Unscaled layout constants, in pixels. Icon row:
const ICON: f32 = 64.0;
const CELL: f32 = 84.0; // icon cell = selection square
const PAD: f32 = 24.0; // panel padding around the row
const LABEL_H: f32 = 30.0; // name strip under the icons

/// Window list:
const ROW_H: f32 = 44.0;
const LIST_W: f32 = 560.0;
const LIST_PAD: f32 = 12.0; // panel padding and selection inset
const NAME_W: f32 = 150.0; // app-name column
const LIST_ICON: f32 = 28.0;

/// Panel corner radius, macOS style.
const RADIUS: f32 = 16.0;

#[derive(Clone, Copy)]
pub struct Layout {
    pub n: usize,
    pub scale: f32,
    pub show_name: bool,
}

impl Layout {
    fn label_h(&self) -> f32 {
        if self.show_name {
            LABEL_H
        } else {
            0.0
        }
    }

    /// Panel size in pixels.
    pub fn size(&self) -> (i32, i32) {
        let w = (2.0 * PAD + self.n as f32 * CELL) * self.scale;
        let h = (2.0 * PAD + CELL + self.label_h()) * self.scale;
        (w.round() as i32, h.round() as i32)
    }

    /// Selection square behind icon `i`: [left, top, right, bottom].
    pub fn cell(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let left = (PAD + i as f32 * CELL) * s;
        [left, PAD * s, left + CELL * s, (PAD + CELL) * s]
    }

    /// Icon rect, centered in its cell.
    pub fn icon(&self, i: usize) -> [f32; 4] {
        let inset = (CELL - ICON) / 2.0 * self.scale;
        let [l, t, r, b] = self.cell(i);
        [l + inset, t + inset, r - inset, b - inset]
    }

    /// Name label rect, symmetric around cell `i`'s center so the text sits
    /// exactly under the icon; edge overflow is clipped by the render target.
    pub fn label(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let [l, t, ..] = self.cell(i);
        let center = l + CELL / 2.0 * s;
        let half = 1.5 * CELL * s;
        let top = t + CELL * s;
        [center - half, top, center + half, top + self.label_h() * s]
    }

    /// Icon index for a mouse x, clamped to the row (AAS behavior: anywhere
    /// inside the panel selects the nearest icon).
    pub fn hover(&self, x: i32) -> usize {
        let cell = ((x as f32 / self.scale - PAD) / CELL).floor();
        (cell.max(0.0) as usize).min(self.n.saturating_sub(1))
    }
}

#[derive(Clone, Copy)]
pub struct ListLayout {
    pub n: usize,
    pub scale: f32,
}

impl ListLayout {
    pub fn size(&self) -> (i32, i32) {
        let w = LIST_W * self.scale;
        let h = (2.0 * LIST_PAD + self.n as f32 * ROW_H) * self.scale;
        (w.round() as i32, h.round() as i32)
    }

    /// Selection rect of row `i`.
    pub fn row(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let top = (LIST_PAD + i as f32 * ROW_H) * s;
        [LIST_PAD * s, top, (LIST_W - LIST_PAD) * s, top + ROW_H * s]
    }

    /// Row internals: app name | icon | window title.
    pub fn name(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let [_, t, _, b] = self.row(i);
        [2.0 * LIST_PAD * s, t, (2.0 * LIST_PAD + NAME_W) * s, b]
    }

    pub fn icon(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let [_, t, _, b] = self.row(i);
        let x = (2.0 * LIST_PAD + NAME_W + 8.0) * s;
        let inset = (ROW_H - LIST_ICON) / 2.0 * s;
        [x, t + inset, x + LIST_ICON * s, b - inset]
    }

    pub fn title(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let [_, t, r, b] = self.row(i);
        let x = (2.0 * LIST_PAD + NAME_W + 8.0 + LIST_ICON + 12.0) * s;
        [x, t, r - LIST_PAD * s, b]
    }

    /// Row index for a mouse y, clamped to the list.
    pub fn hover(&self, y: i32) -> usize {
        let row = ((y as f32 / self.scale - LIST_PAD) / ROW_H).floor();
        (row.max(0.0) as usize).min(self.n.saturating_sub(1))
    }
}

/// The two dialog shapes behind one window and renderer.
#[derive(Clone, Copy)]
pub enum Panel {
    Row(Layout),
    List(ListLayout),
}

impl Panel {
    pub fn size(&self) -> (i32, i32) {
        match self {
            Panel::Row(l) => l.size(),
            Panel::List(l) => l.size(),
        }
    }

    pub fn scale(&self) -> f32 {
        match self {
            Panel::Row(l) => l.scale,
            Panel::List(l) => l.scale,
        }
    }

    pub fn hover(&self, x: i32, y: i32) -> usize {
        match self {
            Panel::Row(l) => l.hover(x),
            Panel::List(l) => l.hover(y),
        }
    }

    pub fn inside(&self, x: i32, y: i32) -> bool {
        let (w, h) = self.size();
        x >= 0 && y >= 0 && x < w && y < h
    }
}

#[cfg(windows)]
pub use win::{close, is_open, kb_select, selection, show, show_list};

#[cfg(windows)]
mod win {
    use super::{Layout, ListLayout, Panel, RADIUS};
    use crate::config::{Config, DialogMonitor, Theme};
    use std::cell::RefCell;
    use std::sync::Once;
    use windows::core::w;
    use windows::Win32::Foundation::{
        COLORREF, ERROR_SUCCESS, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
    };
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
    };
    use windows::Win32::Graphics::Direct2D::{
        D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory,
        ID2D1SolidColorBrush, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
        D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
        D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
        D2D1_RENDER_TARGET_USAGE_NONE, D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
        DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
        DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_WORD_WRAPPING_NO_WRAP,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetMonitorInfoW,
        MonitorFromPoint, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, MONITORINFO,
        MONITOR_DEFAULTTOPRIMARY,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, LoadCursorW, PostMessageW,
        RegisterClassW, SetWindowPos, ShowWindow, UpdateLayeredWindow, CS_DROPSHADOW, IDC_ARROW,
        SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOW, ULW_ALPHA, WM_LBUTTONUP, WM_MOUSEMOVE, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    struct Palette {
        bg: D2D1_COLOR_F,
        hilite: D2D1_COLOR_F,
        placeholder: D2D1_COLOR_F,
        text: D2D1_COLOR_F,
        dim: D2D1_COLOR_F,
    }

    const fn white(a: f32) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a }
    }
    const fn black(a: f32) -> D2D1_COLOR_F {
        D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a }
    }

    /// Panel backgrounds match the Windows taskbar grays (#202020 / #EEEEEE).
    const DARK: Palette = Palette {
        bg: D2D1_COLOR_F { r: 0.125, g: 0.125, b: 0.125, a: 1.0 },
        hilite: white(0.16),
        placeholder: white(0.08),
        text: white(0.9),
        dim: white(0.6),
    };
    const LIGHT: Palette = Palette {
        bg: D2D1_COLOR_F { r: 0.933, g: 0.933, b: 0.933, a: 1.0 },
        hilite: black(0.12),
        placeholder: black(0.06),
        text: black(0.85),
        dim: black(0.55),
    };

    const CLEAR: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

    fn palette(cfg: &Config) -> &'static Palette {
        let light = match cfg.theme {
            Theme::Light => true,
            Theme::Dark => false,
            Theme::Auto => system_light_theme(),
        };
        if light {
            &LIGHT
        } else {
            &DARK
        }
    }

    /// The taskbar follows SystemUsesLightTheme (not AppsUseLightTheme), and
    /// the panel color is meant to match the taskbar.
    fn system_light_theme() -> bool {
        unsafe {
            let mut value: u32 = 0;
            let mut len = std::mem::size_of::<u32>() as u32;
            RegGetValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
                w!("SystemUsesLightTheme"),
                RRF_RT_REG_DWORD,
                None,
                Some(&mut value as *mut u32 as *mut _),
                Some(&mut len),
            ) == ERROR_SUCCESS
                && value != 0
        }
    }

    const DLG_CLASS: windows::core::PCWSTR = w!("win-app-switcher.dialog");

    struct Entry {
        name: Vec<u16>,
        title: Vec<u16>, // empty in the icon row
        icon: Option<Vec<u8>>,
    }

    struct Dlg {
        hwnd: HWND,
        main_hwnd: HWND,
        entries: Vec<Entry>,
        icon_px: u32,
        panel: Panel,
        pal: &'static Palette,
        kb: usize,
        hover: usize,
        mouse_last: bool,
        last_pt: (i32, i32),
        d2d: Option<D2d>, // dropped on device loss or refresh, rebuilt on render
    }

    /// Direct2D drawing into a premultiplied DIB that UpdateLayeredWindow
    /// pushes to the screen — real alpha at the rounded corners.
    struct D2d {
        rt: ID2D1DCRenderTarget,
        dc: HDC,
        bmp: HBITMAP,
        brush: ID2D1SolidColorBrush,
        text: IDWriteTextFormat,
        bitmaps: Vec<Option<ID2D1Bitmap>>,
    }

    impl Drop for D2d {
        fn drop(&mut self) {
            unsafe {
                let _ = DeleteDC(self.dc);
                let _ = DeleteObject(self.bmp.into());
            }
        }
    }

    thread_local! {
        // Touched only by the main thread (dialog wndproc + session logic).
        static DLG: RefCell<Option<Dlg>> = const { RefCell::new(None) };
    }

    /// Icons are extracted once at ICON size and drawn scaled; one cache
    /// entry per exe serves both dialogs.
    fn icon_px(cfg: &Config) -> u32 {
        (super::ICON * cfg.scale) as u32
    }

    /// App switcher: one icon per app group.
    pub fn show(main_hwnd: HWND, groups: &[crate::apps::AppGroup], kb: usize, cfg: &Config) {
        let px = icon_px(cfg);
        let entries = groups
            .iter()
            .map(|g| Entry {
                name: g.name.encode_utf16().collect(),
                title: Vec::new(),
                icon: crate::apps::icon_bgra(&g.exe, px),
            })
            .collect();
        let panel = Panel::Row(Layout {
            n: groups.len(),
            scale: cfg.scale,
            show_name: cfg.show_selected_name,
        });
        open(main_hwnd, entries, panel, kb, px, cfg);
    }

    /// Window switcher: one row per window of the foreground app.
    pub fn show_list(
        main_hwnd: HWND,
        name: &str,
        exe: &str,
        titles: &[String],
        kb: usize,
        cfg: &Config,
    ) {
        let px = icon_px(cfg);
        let icon = crate::apps::icon_bgra(exe, px);
        let entries = titles
            .iter()
            .map(|t| Entry {
                name: name.encode_utf16().collect(),
                title: t.encode_utf16().collect(),
                icon: icon.clone(),
            })
            .collect();
        let panel = Panel::List(ListLayout {
            n: titles.len(),
            scale: cfg.scale,
        });
        open(main_hwnd, entries, panel, kb, px, cfg);
    }

    /// Open the dialog, or refresh contents and size in place if it is
    /// already open (WIN+Q removes an app group).
    fn open(main_hwnd: HWND, entries: Vec<Entry>, panel: Panel, kb: usize, px: u32, cfg: &Config) {
        let (w, h) = panel.size();
        let work = monitor_work_rect(cfg.dialog_monitor);
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + (work.bottom - work.top - h) / 2;
        let pal = palette(cfg);

        let created = DLG.with_borrow_mut(|slot| {
            if let Some(d) = slot {
                d.entries = entries;
                d.icon_px = px;
                d.panel = panel;
                d.pal = pal;
                d.kb = kb;
                d.hover = kb;
                d.mouse_last = false;
                d.d2d = None; // sizes changed: rebuild the surface on next render
                unsafe {
                    let _ = SetWindowPos(d.hwnd, None, x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
                }
                return None;
            }
            unsafe {
                register_class();
                let hinstance = GetModuleHandleW(None).unwrap_or_default();
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                    DLG_CLASS,
                    w!(""),
                    WS_POPUP,
                    x,
                    y,
                    w,
                    h,
                    None,
                    None,
                    Some(hinstance.into()),
                    None,
                ) else {
                    return None;
                };
                // Remember where the cursor is so the spurious WM_MOUSEMOVE a
                // new window under the cursor receives does not hijack the
                // selection before the mouse actually moves.
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                *slot = Some(Dlg {
                    hwnd,
                    main_hwnd,
                    entries,
                    icon_px: px,
                    panel,
                    pal,
                    kb,
                    hover: kb,
                    mouse_last: false,
                    last_pt: (pt.x - x, pt.y - y),
                    d2d: None,
                });
                Some(hwnd)
            }
        });
        // Content is pushed with UpdateLayeredWindow before the window shows:
        // no background flash, ever.
        render();
        if let Some(hwnd) = created {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                // Foreground via the attach dance: makes later
                // SetForegroundWindow on commit trivially allowed, and mouse
                // capture full-strength.
                crate::apps::activate(hwnd, false);
                SetCapture(hwnd);
            }
        }
    }

    /// Keyboard moved the selection.
    pub fn kb_select(kb: usize) {
        let changed = DLG.with_borrow_mut(|slot| {
            slot.as_mut()
                .map(|d| {
                    d.kb = kb;
                    d.mouse_last = false;
                })
                .is_some()
        });
        if changed {
            render();
        }
    }

    /// Effective selection: whichever of keyboard and mouse moved last.
    pub fn selection() -> usize {
        DLG.with_borrow(|slot| {
            slot.as_ref()
                .map_or(0, |d| if d.mouse_last { d.hover } else { d.kb })
        })
    }

    pub fn is_open() -> bool {
        DLG.with_borrow(|slot| slot.is_some())
    }

    pub fn close() {
        if let Some(d) = DLG.with_borrow_mut(|slot| slot.take()) {
            unsafe {
                let _ = ReleaseCapture();
                let _ = DestroyWindow(d.hwnd);
            }
        }
    }

    /// Draw the current state and push it to the screen.
    fn render() {
        DLG.with_borrow_mut(|slot| {
            let Some(d) = slot.as_mut() else { return };
            unsafe {
                if d.d2d.is_none() {
                    d.d2d = d2d_init(d).ok();
                }
                let sel = if d.mouse_last { d.hover } else { d.kb };
                let Some(x) = &d.d2d else { return };
                if draw(x, &d.panel, &d.entries, sel, d.pal).is_err() {
                    d.d2d = None; // device lost: rebuild on the next render
                    return;
                }
                let (w, h) = d.panel.size();
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                let _ = UpdateLayeredWindow(
                    d.hwnd,
                    None,
                    None,
                    Some(&SIZE { cx: w, cy: h }),
                    Some(x.dc),
                    Some(&POINT { x: 0, y: 0 }),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        });
    }

    unsafe fn register_class() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let wc = WNDCLASSW {
                style: CS_DROPSHADOW,
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                lpszClassName: DLG_CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassW(&wc);
        });
    }

    fn monitor_work_rect(which: DialogMonitor) -> RECT {
        unsafe {
            let mut pt = POINT::default();
            if which == DialogMonitor::Mouse {
                let _ = GetCursorPos(&mut pt);
            }
            let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(monitor, &mut info);
            info.rcWork
        }
    }

    fn mouse_coords(lparam: LPARAM) -> (i32, i32) {
        // Signed: with the mouse captured, coordinates go outside the client.
        ((lparam.0 & 0xFFFF) as u16 as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32)
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_MOUSEMOVE => {
                let (x, y) = mouse_coords(lparam);
                let dirty = DLG.with_borrow_mut(|slot| {
                    let Some(d) = slot.as_mut().filter(|d| d.hwnd == hwnd) else {
                        return false;
                    };
                    if (x, y) == d.last_pt {
                        return false;
                    }
                    d.last_pt = (x, y);
                    if !d.panel.inside(x, y) {
                        return false;
                    }
                    let hover = d.panel.hover(x, y);
                    let changed = !d.mouse_last || d.hover != hover;
                    d.hover = hover;
                    d.mouse_last = true;
                    changed
                });
                if dirty {
                    render();
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_coords(lparam);
                let post = DLG.with_borrow_mut(|slot| {
                    let d = slot.as_mut().filter(|d| d.hwnd == hwnd)?;
                    let event = if d.panel.inside(x, y) {
                        d.hover = d.panel.hover(x, y);
                        d.mouse_last = true;
                        crate::hook::Event::Commit
                    } else {
                        crate::hook::Event::Cancel
                    };
                    Some((d.main_hwnd, event))
                });
                if let Some((main, event)) = post {
                    let _ = PostMessageW(
                        Some(main),
                        crate::hook::WM_SWITCHER,
                        WPARAM(event as usize),
                        LPARAM(0),
                    );
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn d2d_init(d: &Dlg) -> windows::core::Result<D2d> {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let rt = factory.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        })?;
        // ClearType needs an opaque backdrop; grayscale AA works on alpha.
        let _ = rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        let brush = rt.CreateSolidColorBrush(&d.pal.text, None)?;
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let text = dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            14.0 * d.panel.scale(),
            w!("en-us"),
        )?;
        text.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        text.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;

        // The layered-window surface: a premultiplied top-down DIB.
        let (w, h) = d.panel.size();
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) => bmp,
            Err(e) => {
                let _ = DeleteDC(dc);
                return Err(e);
            }
        };
        SelectObject(dc, bmp.into());
        if let Err(e) = rt.BindDC(dc, &RECT { left: 0, top: 0, right: w, bottom: h }) {
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bmp.into());
            return Err(e);
        }

        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        let px = d.icon_px;
        let bitmaps = d
            .entries
            .iter()
            .map(|e| {
                e.icon.as_ref().and_then(|bgra| {
                    rt.CreateBitmap(
                        D2D_SIZE_U { width: px, height: px },
                        Some(bgra.as_ptr() as *const _),
                        px * 4,
                        &props,
                    )
                    .ok()
                })
            })
            .collect();
        Ok(D2d { rt, dc, bmp, brush, text, bitmaps })
    }

    fn rect(r: [f32; 4]) -> D2D_RECT_F {
        D2D_RECT_F { left: r[0], top: r[1], right: r[2], bottom: r[3] }
    }

    fn rounded(r: [f32; 4], radius: f32) -> D2D1_ROUNDED_RECT {
        D2D1_ROUNDED_RECT { rect: rect(r), radiusX: radius, radiusY: radius }
    }

    unsafe fn draw(
        x: &D2d,
        panel: &Panel,
        entries: &[Entry],
        sel: usize,
        pal: &Palette,
    ) -> windows::core::Result<()> {
        let s = panel.scale();
        let (w, h) = panel.size();
        x.rt.BeginDraw();
        x.rt.Clear(Some(&CLEAR));
        // The panel itself: rounded corners with real alpha outside.
        x.brush.SetColor(&pal.bg);
        x.rt.FillRoundedRectangle(
            &rounded([0.0, 0.0, w as f32, h as f32], RADIUS * s),
            &x.brush,
        );
        match panel {
            Panel::Row(l) => draw_row(x, l, entries, sel, pal),
            Panel::List(l) => draw_list(x, l, entries, sel, pal),
        }
        x.rt.EndDraw(None, None)
    }

    unsafe fn draw_row(x: &D2d, l: &Layout, entries: &[Entry], sel: usize, pal: &Palette) {
        let radius = 10.0 * l.scale;
        x.brush.SetColor(&pal.hilite);
        x.rt.FillRoundedRectangle(&rounded(l.cell(sel), radius), &x.brush);
        for (i, bitmap) in x.bitmaps.iter().enumerate() {
            draw_icon(x, bitmap, l.icon(i), radius, pal);
        }
        if l.show_name {
            if let Some(entry) = entries.get(sel) {
                let _ = x.text.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                x.brush.SetColor(&pal.text);
                x.rt.DrawText(
                    &entry.name,
                    &x.text,
                    &rect(l.label(sel)),
                    &x.brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
    }

    unsafe fn draw_list(x: &D2d, l: &ListLayout, entries: &[Entry], sel: usize, pal: &Palette) {
        let radius = 8.0 * l.scale;
        x.brush.SetColor(&pal.hilite);
        x.rt.FillRoundedRectangle(&rounded(l.row(sel), radius), &x.brush);
        let _ = x.text.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING);
        for (i, entry) in entries.iter().enumerate() {
            draw_icon(x, &x.bitmaps[i], l.icon(i), 6.0 * l.scale, pal);
            x.brush.SetColor(&pal.text);
            x.rt.DrawText(
                &entry.name,
                &x.text,
                &rect(l.name(i)),
                &x.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            x.brush.SetColor(&pal.dim);
            x.rt.DrawText(
                &entry.title,
                &x.text,
                &rect(l.title(i)),
                &x.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    unsafe fn draw_icon(
        x: &D2d,
        bitmap: &Option<ID2D1Bitmap>,
        r: [f32; 4],
        radius: f32,
        pal: &Palette,
    ) {
        match bitmap {
            Some(b) => x.rt.DrawBitmap(
                b,
                Some(&rect(r)),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            ),
            None => {
                x.brush.SetColor(&pal.placeholder);
                x.rt.FillRoundedRectangle(&rounded(r, radius), &x.brush);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Layout = Layout { n: 5, scale: 1.0, show_name: true };
    const W: ListLayout = ListLayout { n: 4, scale: 1.0 };

    #[test]
    fn panel_size_scales() {
        let (w, h) = L.size();
        assert_eq!(w, (2.0 * PAD + 5.0 * CELL) as i32);
        assert_eq!(h, (2.0 * PAD + CELL + LABEL_H) as i32);
        let double = Layout { scale: 2.0, ..L };
        assert_eq!(double.size(), (2 * w, 2 * h));
        let bare = Layout { show_name: false, ..L };
        assert_eq!(bare.size().1, (2.0 * PAD + CELL) as i32);
    }

    #[test]
    fn cells_tile_the_row() {
        for i in 0..L.n {
            let [l, t, r, b] = L.cell(i);
            assert_eq!(r - l, CELL);
            assert_eq!(b - t, CELL);
            if i > 0 {
                assert_eq!(l, L.cell(i - 1)[2], "cells adjacent");
            }
        }
        assert_eq!(L.cell(0)[0], PAD);
        assert_eq!(L.cell(L.n - 1)[2] + PAD, L.size().0 as f32);
    }

    #[test]
    fn icon_centered_in_cell() {
        let [cl, ct, cr, cb] = L.cell(2);
        let [il, it, ir, ib] = L.icon(2);
        assert_eq!(ir - il, ICON);
        assert_eq!(il - cl, cr - ir);
        assert_eq!(it - ct, cb - ib);
    }

    #[test]
    fn hover_clamps_to_row() {
        assert_eq!(L.hover(-100), 0);
        assert_eq!(L.hover(0), 0);
        let center0 = (PAD + CELL / 2.0) as i32;
        assert_eq!(L.hover(center0), 0);
        assert_eq!(L.hover(center0 + CELL as i32), 1);
        assert_eq!(L.hover(L.size().0 + 100), L.n - 1);
        let one = Layout { n: 1, ..L };
        assert_eq!(one.hover(9999), 0);
    }

    #[test]
    fn hover_matches_cell_bounds() {
        for i in 0..L.n {
            let [l, _, r, _] = L.cell(i);
            assert_eq!(L.hover(l as i32), i);
            assert_eq!(L.hover(r as i32 - 1), i);
        }
    }

    #[test]
    fn panel_inside_bounds() {
        let p = Panel::Row(L);
        let (w, h) = p.size();
        assert!(p.inside(0, 0));
        assert!(p.inside(w - 1, h - 1));
        assert!(!p.inside(-1, 5));
        assert!(!p.inside(w, 5));
        assert!(!p.inside(5, h));
    }

    #[test]
    fn label_centered_under_its_cell() {
        // Even at the edges the label stays symmetric around the icon; any
        // overflow is clipped, never shifted toward the panel center.
        for i in [0, 2, L.n - 1] {
            let [ll, lt, lr, lb] = L.label(i);
            let [cl, _, cr, cb] = L.cell(i);
            assert_eq!((ll + lr) / 2.0, (cl + cr) / 2.0, "centered under cell {i}");
            assert_eq!(lt, cb, "label starts under the cell");
            assert_eq!(lb - lt, LABEL_H);
        }
        assert!(L.label(0)[0] < 0.0, "edge label may overflow; clipping handles it");
    }

    #[test]
    fn list_rows_tile_the_panel() {
        let (w, h) = W.size();
        assert_eq!(w, LIST_W as i32);
        assert_eq!(h, (2.0 * LIST_PAD + 4.0 * ROW_H) as i32);
        for i in 0..W.n {
            let [l, t, r, b] = W.row(i);
            assert_eq!(b - t, ROW_H);
            assert_eq!(l, LIST_PAD);
            assert_eq!(r, LIST_W - LIST_PAD);
            if i > 0 {
                assert_eq!(t, W.row(i - 1)[3], "rows adjacent");
            }
        }
    }

    #[test]
    fn list_row_internals_ordered_and_contained() {
        let [rl, rt, rr, rb] = W.row(1);
        let name = W.name(1);
        let icon = W.icon(1);
        let title = W.title(1);
        assert!(rl < name[0] && name[2] <= icon[0], "name | icon");
        assert!(icon[2] <= title[0] && title[2] <= rr, "icon | title");
        assert_eq!(icon[2] - icon[0], LIST_ICON);
        assert_eq!(icon[3] - icon[1], LIST_ICON, "icon square, centered");
        for r in [name, icon, title] {
            assert!(r[1] >= rt && r[3] <= rb, "inside the row");
        }
    }

    #[test]
    fn list_hover_clamps() {
        assert_eq!(W.hover(-50), 0);
        assert_eq!(W.hover((LIST_PAD + ROW_H / 2.0) as i32), 0);
        assert_eq!(W.hover((LIST_PAD + 1.5 * ROW_H) as i32), 1);
        assert_eq!(W.hover(W.size().1 + 100), W.n - 1);
    }
}
