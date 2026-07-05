//! App switcher dialog (M3): WIN+TAB icon row.
//!
//! Layout arithmetic is pure and unit-tested on any host; the Windows glue
//! below renders it with Direct2D + DirectWrite and feeds mouse input back
//! to the main thread as the same [`crate::hook::Event`]s the hook posts.
//!
//! The window recipe is AltAppSwitcher's proven one: WS_POPUP topmost tool
//! window, made foreground, mouse captured with SetCapture so a click
//! outside the panel is seen (and cancels), Win11 rounded corners via
//! DwmSetWindowAttribute.

/// Unscaled layout constants, in pixels.
const ICON: f32 = 64.0;
const CELL: f32 = 84.0; // icon cell = selection square
const PAD: f32 = 16.0; // panel padding around the row
const LABEL_H: f32 = 30.0; // name strip under the icons

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

    /// Name label rect: centered under cell `i`, clamped to the panel.
    pub fn label(&self, i: usize) -> [f32; 4] {
        let s = self.scale;
        let (w, h) = self.size();
        let [l, t, ..] = self.cell(i);
        let center = l + CELL / 2.0 * s;
        let half = 1.5 * CELL * s;
        let top = t + CELL * s;
        [
            (center - half).max(0.0),
            top,
            (center + half).min(w as f32),
            (h as f32).min(top + self.label_h() * s),
        ]
    }

    /// Icon index for a mouse position, clamped to the row (AAS behavior:
    /// anywhere inside the panel selects the nearest icon).
    pub fn hover(&self, x: i32) -> usize {
        let cell = ((x as f32 / self.scale - PAD) / CELL).floor();
        (cell.max(0.0) as usize).min(self.n.saturating_sub(1))
    }

    pub fn inside(&self, x: i32, y: i32) -> bool {
        let (w, h) = self.size();
        x >= 0 && y >= 0 && x < w && y < h
    }
}

#[cfg(windows)]
pub use win::{close, kb_select, selection, show};

#[cfg(windows)]
mod win {
    use super::Layout;
    use crate::config::{Config, DialogMonitor};
    use std::cell::RefCell;
    use std::sync::Once;
    use windows::core::w;
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
    };
    use windows::Win32::Graphics::Direct2D::{
        D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget,
        ID2D1SolidColorBrush, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES,
        D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
        D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
        D2D1_ROUNDED_RECT,
    };
    use windows::Win32::Graphics::DirectWrite::{
        DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
        DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
        DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
        DWRITE_WORD_WRAPPING_NO_WRAP,
    };
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, EndPaint, GetMonitorInfoW, InvalidateRect, MonitorFromPoint,
        UpdateWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY, PAINTSTRUCT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, LoadCursorW, PostMessageW,
        RegisterClassW, SetWindowPos, ShowWindow, IDC_ARROW, SWP_NOACTIVATE, SWP_NOZORDER,
        SW_SHOW, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_POPUP,
    };

    // ponytail: dark palette only; theme auto/light/dark wiring is M5.
    const BG: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.118, g: 0.118, b: 0.125, a: 1.0 };
    const HILITE: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.16 };
    const PLACEHOLDER: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.08 };
    const TEXT: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.9 };

    const DLG_CLASS: windows::core::PCWSTR = w!("win-app-switcher.dialog");

    struct Dlg {
        hwnd: HWND,
        main_hwnd: HWND,
        names: Vec<Vec<u16>>,
        icons: Vec<Option<Vec<u8>>>,
        icon_px: u32,
        layout: Layout,
        kb: usize,
        hover: usize,
        mouse_last: bool,
        last_pt: (i32, i32),
        d2d: Option<D2d>, // dropped on device loss or refresh, rebuilt on paint
    }

    struct D2d {
        rt: ID2D1HwndRenderTarget,
        brush: ID2D1SolidColorBrush,
        text: IDWriteTextFormat,
        bitmaps: Vec<Option<ID2D1Bitmap>>,
    }

    thread_local! {
        // Touched only by the main thread (dialog wndproc + session logic).
        static DLG: RefCell<Option<Dlg>> = const { RefCell::new(None) };
    }

    /// Open the dialog, or refresh contents in place if it is already open
    /// (WIN+Q removes an app group). `kb` is the keyboard selection.
    pub fn show(main_hwnd: HWND, groups: &[crate::apps::AppGroup], kb: usize, cfg: &Config) {
        let layout = Layout {
            n: groups.len(),
            scale: cfg.scale,
            show_name: cfg.show_selected_name,
        };
        let icon_px = (super::ICON * cfg.scale) as u32;
        let names = groups.iter().map(|g| g.name.encode_utf16().collect()).collect();
        let icons = groups.iter().map(|g| crate::apps::icon_bgra(&g.exe, icon_px)).collect();
        let (w, h) = layout.size();
        let work = monitor_work_rect(cfg.dialog_monitor);
        let x = work.left + (work.right - work.left - w) / 2;
        let y = work.top + (work.bottom - work.top - h) / 2;

        let created = DLG.with_borrow_mut(|slot| {
            if let Some(d) = slot {
                d.names = names;
                d.icons = icons;
                d.icon_px = icon_px;
                d.layout = layout;
                d.kb = kb;
                d.hover = kb;
                d.mouse_last = false;
                d.d2d = None; // sizes changed: rebuild the render target on next paint
                unsafe {
                    let _ = SetWindowPos(d.hwnd, None, x, y, w, h, SWP_NOACTIVATE | SWP_NOZORDER);
                    let _ = InvalidateRect(Some(d.hwnd), None, false);
                }
                return None;
            }
            unsafe {
                register_class();
                let hinstance = GetModuleHandleW(None).unwrap_or_default();
                let Ok(hwnd) = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
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
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &DWMWCP_ROUND as *const _ as *const _,
                    std::mem::size_of_val(&DWMWCP_ROUND) as u32,
                );
                // Remember where the cursor is so the spurious WM_MOUSEMOVE a
                // new window under the cursor receives does not hijack the
                // selection before the mouse actually moves.
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                *slot = Some(Dlg {
                    hwnd,
                    main_hwnd,
                    names,
                    icons,
                    icon_px,
                    layout,
                    kb,
                    hover: kb,
                    mouse_last: false,
                    last_pt: (pt.x - x, pt.y - y),
                    d2d: None,
                });
                Some(hwnd)
            }
        });
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
        let hwnd = DLG.with_borrow_mut(|slot| {
            slot.as_mut().map(|d| {
                d.kb = kb;
                d.mouse_last = false;
                d.hwnd
            })
        });
        if let Some(hwnd) = hwnd {
            redraw(hwnd);
        }
    }

    /// Effective selection: whichever of keyboard and mouse moved last.
    pub fn selection() -> usize {
        DLG.with_borrow(|slot| {
            slot.as_ref()
                .map_or(0, |d| if d.mouse_last { d.hover } else { d.kb })
        })
    }

    pub fn close() {
        if let Some(d) = DLG.with_borrow_mut(|slot| slot.take()) {
            unsafe {
                let _ = ReleaseCapture();
                let _ = DestroyWindow(d.hwnd);
            }
        }
    }

    fn redraw(hwnd: HWND) {
        unsafe {
            let _ = InvalidateRect(Some(hwnd), None, false);
            let _ = UpdateWindow(hwnd);
        }
    }

    unsafe fn register_class() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                lpszClassName: DLG_CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                // Class brush in the panel color: no white flash before the
                // first Direct2D frame.
                hbrBackground: CreateSolidBrush(COLORREF(0x0020_1E1E)),
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
                    if !d.layout.inside(x, y) {
                        return false;
                    }
                    let hover = d.layout.hover(x);
                    let changed = !d.mouse_last || d.hover != hover;
                    d.hover = hover;
                    d.mouse_last = true;
                    changed
                });
                if dirty {
                    redraw(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_coords(lparam);
                let post = DLG.with_borrow_mut(|slot| {
                    let d = slot.as_mut().filter(|d| d.hwnd == hwnd)?;
                    let event = if d.layout.inside(x, y) {
                        d.hover = d.layout.hover(x);
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
            WM_PAINT => {
                paint(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn paint(hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let _ = BeginPaint(hwnd, &mut ps);
        DLG.with_borrow_mut(|slot| {
            if let Some(d) = slot.as_mut().filter(|d| d.hwnd == hwnd) {
                if d.d2d.is_none() {
                    d.d2d = d2d_init(d).ok();
                }
                let sel = if d.mouse_last { d.hover } else { d.kb };
                if let Some(x) = &d.d2d {
                    if draw(x, &d.layout, &d.names, sel).is_err() {
                        d.d2d = None; // device lost: rebuild on the next frame
                    }
                }
            }
        });
        let _ = EndPaint(hwnd, &ps);
    }

    unsafe fn d2d_init(d: &Dlg) -> windows::core::Result<D2d> {
        let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let (w, h) = d.layout.size();
        let rt = factory.CreateHwndRenderTarget(
            &D2D1_RENDER_TARGET_PROPERTIES::default(),
            &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd: d.hwnd,
                pixelSize: D2D_SIZE_U { width: w as u32, height: h as u32 },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            },
        )?;
        let brush = rt.CreateSolidColorBrush(&TEXT, None)?;
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        let text = dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None::<&IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            14.0 * d.layout.scale,
            w!("en-us"),
        )?;
        text.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        text.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        text.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
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
            .icons
            .iter()
            .map(|icon| {
                icon.as_ref().and_then(|bgra| {
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
        Ok(D2d { rt, brush, text, bitmaps })
    }

    fn rect(r: [f32; 4]) -> D2D_RECT_F {
        D2D_RECT_F { left: r[0], top: r[1], right: r[2], bottom: r[3] }
    }

    unsafe fn draw(
        x: &D2d,
        layout: &Layout,
        names: &[Vec<u16>],
        sel: usize,
    ) -> windows::core::Result<()> {
        let radius = 10.0 * layout.scale;
        x.rt.BeginDraw();
        x.rt.Clear(Some(&BG));
        x.brush.SetColor(&HILITE);
        x.rt.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT { rect: rect(layout.cell(sel)), radiusX: radius, radiusY: radius },
            &x.brush,
        );
        for (i, bitmap) in x.bitmaps.iter().enumerate() {
            match bitmap {
                Some(b) => x.rt.DrawBitmap(
                    b,
                    Some(&rect(layout.icon(i))),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                ),
                None => {
                    x.brush.SetColor(&PLACEHOLDER);
                    x.rt.FillRoundedRectangle(
                        &D2D1_ROUNDED_RECT {
                            rect: rect(layout.icon(i)),
                            radiusX: radius,
                            radiusY: radius,
                        },
                        &x.brush,
                    );
                }
            }
        }
        if layout.show_name {
            if let Some(name) = names.get(sel) {
                x.brush.SetColor(&TEXT);
                x.rt.DrawText(
                    name,
                    &x.text,
                    &rect(layout.label(sel)),
                    &x.brush,
                    D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
        x.rt.EndDraw(None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Layout = Layout { n: 5, scale: 1.0, show_name: true };

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
    fn inside_panel_bounds() {
        let (w, h) = L.size();
        assert!(L.inside(0, 0));
        assert!(L.inside(w - 1, h - 1));
        assert!(!L.inside(-1, 5));
        assert!(!L.inside(w, 5));
        assert!(!L.inside(5, h));
    }

    #[test]
    fn label_clamped_to_panel() {
        let (w, _) = L.size();
        let first = L.label(0);
        assert_eq!(first[0], 0.0);
        let last = L.label(L.n - 1);
        assert_eq!(last[2], w as f32);
        let mid = L.label(2);
        let center = (mid[0] + mid[2]) / 2.0;
        let [cl, .., cr, _] = L.cell(2);
        assert_eq!(center, (cl + cr) / 2.0, "label centered under its cell");
        assert_eq!(mid[3] - mid[1], LABEL_H);
    }
}
