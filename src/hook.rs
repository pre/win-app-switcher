//! WH_KEYBOARD_LL state machine (M1).
//!
//! The decision logic is pure and unit-tested on any host; the Windows glue
//! at the bottom feeds it hook events and executes the returned actions.
//!
//! Key facts the logic encodes:
//! - WIN down/up always PASS through, so WIN+L, WIN+D … keep working and the
//!   OS never sees WIN stuck down. Only TAB / § / Q / ESC are swallowed, and
//!   only during a switcher session.
//! - On session start a dummy key (VK 0xFF, unassigned) is injected while WIN
//!   is still down, so Windows sees WIN as a combo, not a tap: the Start menu
//!   never opens on release. Same trick as AltAppSwitcher/PowerToys.
//! - ponytail: no RestoreKey-style stuck-key sweep — WIN passes through both
//!   ways and watched keys are swallowed symmetrically, so nothing can stick
//!   by construction. Port AAS RestoreKey if manual testing proves otherwise.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    None,
    App,
    Win,
}

/// Keys the hook watches; everything else passes through untouched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Win,
    Tab,
    /// The key left of digit 1, identified by scan code 41 (§ on FI/SE, ` on US).
    Section,
    Esc,
    Q,
}

/// Posted to the main thread as the wparam of [`WM_SWITCHER`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    AppNext,
    AppPrev,
    WinNext,
    WinPrev,
    /// WIN released with a session active: activate the selection.
    Commit,
    /// ESC pressed: close the session without switching.
    Cancel,
    /// Q pressed in app mode: close all windows of the selected app.
    CloseApp,
}

impl Event {
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub fn from_wparam(w: usize) -> Option<Event> {
        use Event::*;
        [AppNext, AppPrev, WinNext, WinPrev, Commit, Cancel, CloseApp]
            .into_iter()
            .find(|e| *e as usize == w)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct State {
    pub mode: Mode,
    pub win_down: bool,
}

pub const IDLE: State = State {
    mode: Mode::None,
    win_down: false,
};

pub struct Actions {
    pub swallow: bool,
    pub event: Option<Event>,
    pub inject_dummy: bool,
}

const PASS: Actions = Actions {
    swallow: false,
    event: None,
    inject_dummy: false,
};

pub fn step(s: &mut State, key: Key, up: bool, shift: bool) -> Actions {
    match (key, up) {
        (Key::Win, false) => {
            s.win_down = true;
            PASS
        }
        (Key::Win, true) => {
            s.win_down = false;
            let event = (s.mode != Mode::None).then_some(Event::Commit);
            s.mode = Mode::None;
            Actions { event, ..PASS }
        }
        (Key::Tab, false) if s.win_down => {
            let inject_dummy = s.mode == Mode::None;
            if s.mode == Mode::None {
                s.mode = Mode::App;
            }
            // Tab during a WIN+§ session: swallowed, ignored.
            let event = (s.mode == Mode::App)
                .then(|| if shift { Event::AppPrev } else { Event::AppNext });
            Actions {
                swallow: true,
                event,
                inject_dummy,
            }
        }
        (Key::Section, false) if s.win_down => {
            let inject_dummy = s.mode == Mode::None;
            if s.mode == Mode::None {
                s.mode = Mode::Win;
            }
            let event = (s.mode == Mode::Win)
                .then(|| if shift { Event::WinPrev } else { Event::WinNext });
            Actions {
                swallow: true,
                event,
                inject_dummy,
            }
        }
        (Key::Tab | Key::Section, true) if s.mode != Mode::None => Actions {
            swallow: true,
            ..PASS
        },
        (Key::Esc, false) if s.mode != Mode::None => {
            s.mode = Mode::None;
            Actions {
                swallow: true,
                event: Some(Event::Cancel),
                inject_dummy: false,
            }
        }
        // Swallow Q both ways during any session so WIN+Q never opens Search.
        (Key::Q, up) if s.mode != Mode::None => Actions {
            swallow: true,
            event: (!up && s.mode == Mode::App).then_some(Event::CloseApp),
            inject_dummy: false,
        },
        _ => PASS,
    }
}

#[cfg(windows)]
pub use win::{start, WM_SWITCHER};

#[cfg(windows)]
mod win {
    use super::{step, Key, State, IDLE};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::Mutex;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_ESCAPE, VK_LWIN, VK_Q, VK_RWIN,
        VK_SHIFT, VK_TAB,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, PostMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT,
        LLKHF_INJECTED, LLKHF_UP, MSG, WH_KEYBOARD_LL, WM_APP,
    };

    pub const WM_SWITCHER: u32 = WM_APP + 2;

    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
    // Only the hook thread ever locks this, so it never contends.
    static STATE: Mutex<State> = Mutex::new(IDLE);

    /// Install the hook on its own message-loop thread. The callback must
    /// stay fast (Windows silently drops slow hooks), hence TIME_CRITICAL
    /// and nothing but state transitions + PostMessage in it.
    pub fn start(hwnd: HWND) {
        MAIN_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        std::thread::spawn(|| unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            if SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), None, 0).is_err() {
                crate::win::alert(
                    "Failed to install the keyboard hook; switching will not work.",
                    crate::win::Severity::Warning,
                );
                return;
            }
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        });
    }

    fn map_key(vk: u32, scan: u32) -> Option<Key> {
        if scan == 41 {
            return Some(Key::Section);
        }
        match VIRTUAL_KEY(vk as u16) {
            VK_LWIN | VK_RWIN => Some(Key::Win),
            VK_TAB => Some(Key::Tab),
            VK_ESCAPE => Some(Key::Esc),
            VK_Q => Some(Key::Q),
            _ => None,
        }
    }

    unsafe extern "system" fn kb_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if ncode == HC_ACTION as i32 {
            let kb = *(lparam.0 as *const KBDLLHOOKSTRUCT);
            // Skip injected events (our own dummy key) to avoid feedback loops.
            if !kb.flags.contains(LLKHF_INJECTED) {
                if let Some(key) = map_key(kb.vkCode, kb.scanCode) {
                    let up = kb.flags.contains(LLKHF_UP);
                    let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                    let actions = step(&mut STATE.lock().unwrap(), key, up, shift);
                    if actions.inject_dummy {
                        inject_dummy();
                    }
                    if let Some(ev) = actions.event {
                        let hwnd = HWND(MAIN_HWND.load(Ordering::Relaxed) as *mut _);
                        let _ =
                            PostMessageW(Some(hwnd), WM_SWITCHER, WPARAM(ev as usize), LPARAM(0));
                    }
                    if actions.swallow {
                        return LRESULT(1);
                    }
                }
            }
        }
        CallNextHookEx(None, ncode, wparam, lparam)
    }

    /// Press+release VK 0xFF (unassigned) so Windows sees WIN as part of a
    /// combo instead of a tap. No application reacts to 0xFF.
    unsafe fn inject_dummy() {
        let key = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0xFF),
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    ..Default::default()
                },
            },
        };
        let _ = SendInput(&[key(false), key(true)], std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win_tab_session() {
        let mut s = IDLE;
        let a = step(&mut s, Key::Win, false, false);
        assert!(!a.swallow && a.event.is_none());

        let a = step(&mut s, Key::Tab, false, false);
        assert!(a.swallow && a.inject_dummy);
        assert_eq!(a.event, Some(Event::AppNext));

        let a = step(&mut s, Key::Tab, false, true);
        assert_eq!(a.event, Some(Event::AppPrev));
        assert!(!a.inject_dummy, "dummy key only once per session");

        let a = step(&mut s, Key::Tab, true, false);
        assert!(a.swallow && a.event.is_none());

        let a = step(&mut s, Key::Win, true, false);
        assert!(!a.swallow, "WIN up must reach the OS");
        assert_eq!(a.event, Some(Event::Commit));
        assert_eq!(s.mode, Mode::None);
    }

    #[test]
    fn win_section_session() {
        let mut s = IDLE;
        step(&mut s, Key::Win, false, false);
        let a = step(&mut s, Key::Section, false, false);
        assert!(a.swallow && a.inject_dummy);
        assert_eq!(a.event, Some(Event::WinNext));
        let a = step(&mut s, Key::Section, false, true);
        assert_eq!(a.event, Some(Event::WinPrev));
        // Tab during a window session is swallowed but does nothing.
        let a = step(&mut s, Key::Tab, false, false);
        assert!(a.swallow && a.event.is_none());
        assert_eq!(s.mode, Mode::Win);
    }

    #[test]
    fn keys_without_win_pass_through() {
        let mut s = IDLE;
        for key in [Key::Tab, Key::Section, Key::Esc, Key::Q] {
            let a = step(&mut s, key, false, false);
            assert!(!a.swallow && a.event.is_none());
            assert_eq!(s.mode, Mode::None);
        }
    }

    #[test]
    fn bare_win_tap_stays_untouched() {
        // No session → both WIN events pass, Start menu behaves normally.
        let mut s = IDLE;
        assert!(!step(&mut s, Key::Win, false, false).swallow);
        let a = step(&mut s, Key::Win, true, false);
        assert!(!a.swallow && a.event.is_none());
    }

    #[test]
    fn esc_cancels_and_win_up_does_not_commit() {
        let mut s = IDLE;
        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Tab, false, false);
        let a = step(&mut s, Key::Esc, false, false);
        assert!(a.swallow);
        assert_eq!(a.event, Some(Event::Cancel));
        assert_eq!(s.mode, Mode::None);
        // WIN still held: a new session can start, with a fresh dummy key.
        let a = step(&mut s, Key::Tab, false, false);
        assert!(a.inject_dummy);
        assert_eq!(a.event, Some(Event::AppNext));
        step(&mut s, Key::Esc, false, false);
        let a = step(&mut s, Key::Win, true, false);
        assert!(a.event.is_none(), "cancelled session must not commit");
    }

    #[test]
    fn q_closes_app_only_in_app_mode() {
        let mut s = IDLE;
        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Section, false, false);
        let a = step(&mut s, Key::Q, false, false);
        assert!(a.swallow && a.event.is_none(), "swallowed but no-op in win mode");
        step(&mut s, Key::Win, true, false);

        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Tab, false, false);
        let a = step(&mut s, Key::Q, false, false);
        assert_eq!(a.event, Some(Event::CloseApp));
        let a = step(&mut s, Key::Q, true, false);
        assert!(a.swallow && a.event.is_none());
    }

    #[test]
    fn event_wparam_roundtrip() {
        use Event::*;
        for ev in [AppNext, AppPrev, WinNext, WinPrev, Commit, Cancel, CloseApp] {
            assert_eq!(Event::from_wparam(ev as usize), Some(ev));
        }
        assert_eq!(Event::from_wparam(999), None);
    }
}
