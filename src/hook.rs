//! WH_KEYBOARD_LL state machine (M1).
//!
//! The decision logic is pure and unit-tested on any host; the Windows glue
//! at the bottom feeds it hook events and executes the returned actions.
//!
//! Key facts the logic encodes:
//! - WIN down/up always PASS through, so WIN+L, WIN+D … keep working and the
//!   OS never sees WIN stuck down. Only TAB / § / Q / W / ESC / arrows are
//!   swallowed, and only during a switcher session.
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
    W,
    Left,
    Right,
    Up,
    Down,
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
    /// W pressed in win mode: close only the selected window.
    CloseWindow,
}

impl Event {
    pub fn from_wparam(w: usize) -> Option<Event> {
        use Event::*;
        [AppNext, AppPrev, WinNext, WinPrev, Commit, Cancel, CloseApp, CloseWindow]
            .into_iter()
            .find(|e| *e as usize == w)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct State {
    pub mode: Mode,
    pub win_down: bool,
    /// Q/W held down — their autorepeat must not close one thing per repeat.
    pub q_down: bool,
    pub w_down: bool,
}

pub const IDLE: State = State {
    mode: Mode::None,
    win_down: false,
    q_down: false,
    w_down: false,
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
    // Tracked across sessions: a Q/W already held when a session starts (or
    // held over a session boundary) is not a fresh press either.
    let was_down = match key {
        Key::Q => std::mem::replace(&mut s.q_down, !up),
        Key::W => std::mem::replace(&mut s.w_down, !up),
        _ => false,
    };
    let repeat = !up && was_down;
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
            // TAB always means the app switcher: it starts a session, and
            // mid WIN+§ session it switches over (win session discarded).
            let inject_dummy = s.mode == Mode::None;
            s.mode = Mode::App;
            Actions {
                swallow: true,
                event: Some(if shift { Event::AppPrev } else { Event::AppNext }),
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
        // Swallow Q/W both ways during any session (WIN+Q would open Search,
        // WIN+W the widgets pane). One close per physical press: autorepeat
        // closes nothing more until a key-up is seen.
        (Key::Q, up) if s.mode != Mode::None => Actions {
            swallow: true,
            event: (!up && !repeat && s.mode == Mode::App).then_some(Event::CloseApp),
            inject_dummy: false,
        },
        (Key::W, up) if s.mode != Mode::None => Actions {
            swallow: true,
            event: (!up && !repeat && s.mode == Mode::Win).then_some(Event::CloseWindow),
            inject_dummy: false,
        },
        // Arrows move the selection (Left/Right in the app switcher, Up/Down
        // in the window list); outside a session they pass through so
        // WIN+arrow window snapping keeps working.
        (Key::Left | Key::Right, up) if s.mode == Mode::App => Actions {
            swallow: true,
            event: (!up).then(|| {
                if key == Key::Left {
                    Event::AppPrev
                } else {
                    Event::AppNext
                }
            }),
            inject_dummy: false,
        },
        (Key::Up | Key::Down, up) if s.mode == Mode::Win => Actions {
            swallow: true,
            event: (!up).then(|| {
                if key == Key::Up {
                    Event::WinPrev
                } else {
                    Event::WinNext
                }
            }),
            inject_dummy: false,
        },
        _ => PASS,
    }
}

#[cfg(windows)]
pub use win::{inject_dummy, inject_key, start, WM_SWITCHER};

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
        KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN,
        VK_Q, VK_RIGHT, VK_RWIN, VK_SHIFT, VK_TAB, VK_UP, VK_W,
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
            VK_W => Some(Key::W),
            VK_LEFT => Some(Key::Left),
            VK_RIGHT => Some(Key::Right),
            VK_UP => Some(Key::Up),
            VK_DOWN => Some(Key::Down),
            _ => None,
        }
    }

    unsafe extern "system" fn kb_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if ncode == HC_ACTION as i32 {
            let kb = *(lparam.0 as *const KBDLLHOOKSTRUCT);
            // Skip injected events (our own dummy key) to avoid feedback loops.
            if !kb.flags.contains(LLKHF_INJECTED) {
                let up = kb.flags.contains(LLKHF_UP);
                if let Some(key) = map_key(kb.vkCode, kb.scanCode) {
                    let shift = (GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                    let actions = step(&mut STATE.lock().unwrap(), key, up, shift);
                    #[cfg(debug_assertions)]
                    {
                        let mut verdict =
                            if actions.swallow { "swallow" } else { "pass through" }.to_string();
                        if actions.inject_dummy {
                            verdict += ", inject dummy key (Start menu will not open)";
                        }
                        if let Some(ev) = actions.event {
                            verdict += &format!(", post {ev:?}");
                        }
                        println!(
                            "{key:?} {} (vk=0x{:02X} scan=0x{:02X} shift={shift}): {verdict}",
                            if up { "up" } else { "down" },
                            kb.vkCode,
                            kb.scanCode
                        );
                    }
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
                } else {
                    #[cfg(debug_assertions)]
                    println!(
                        "unwatched key {} (vk=0x{:02X} scan=0x{:02X}): pass through",
                        if up { "up" } else { "down" },
                        kb.vkCode,
                        kb.scanCode
                    );
                }
            }
        }
        CallNextHookEx(None, ncode, wparam, lparam)
    }

    /// Press+release `vk`. Injected events are skipped by our own hook, so
    /// they always reach the foreground application untouched.
    pub unsafe fn inject_key(vk: u16) {
        let key = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    ..Default::default()
                },
            },
        };
        let _ = SendInput(&[key(false), key(true)], std::mem::size_of::<INPUT>() as i32);
    }

    /// Press+release VK 0xFF (unassigned) so Windows sees WIN as part of a
    /// combo instead of a tap. No application reacts to 0xFF. Also doubles
    /// as the SetForegroundWindow unlock in [`crate::apps::activate`]: the
    /// last-input-sender process may take the foreground.
    pub unsafe fn inject_dummy() {
        inject_key(0xFF);
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
        // Tab during a window session switches over to the app switcher.
        let a = step(&mut s, Key::Tab, false, false);
        assert!(a.swallow);
        assert_eq!(a.event, Some(Event::AppNext));
        assert!(!a.inject_dummy, "dummy key already injected by this session");
        assert_eq!(s.mode, Mode::App);
    }

    #[test]
    fn arrows_move_selection_only_in_app_mode() {
        let mut s = IDLE;
        step(&mut s, Key::Win, false, false);
        // No session yet: WIN+arrow snapping must keep working.
        assert!(!step(&mut s, Key::Right, false, false).swallow);
        step(&mut s, Key::Tab, false, false);
        let a = step(&mut s, Key::Right, false, false);
        assert!(a.swallow);
        assert_eq!(a.event, Some(Event::AppNext));
        let a = step(&mut s, Key::Left, false, false);
        assert_eq!(a.event, Some(Event::AppPrev));
        let a = step(&mut s, Key::Left, true, false);
        assert!(a.swallow && a.event.is_none(), "arrow key-up swallowed silently");
        step(&mut s, Key::Win, true, false);

        // Window mode: Left/Right pass through, Up/Down move the selection.
        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Section, false, false);
        assert!(!step(&mut s, Key::Right, false, false).swallow);
        let a = step(&mut s, Key::Down, false, false);
        assert!(a.swallow);
        assert_eq!(a.event, Some(Event::WinNext));
        let a = step(&mut s, Key::Up, false, false);
        assert_eq!(a.event, Some(Event::WinPrev));
        // ...and Up/Down pass through in app mode (WIN+Up maximize works).
        step(&mut s, Key::Tab, false, false);
        assert!(!step(&mut s, Key::Up, false, false).swallow);
    }

    #[test]
    fn keys_without_win_pass_through() {
        let mut s = IDLE;
        for key in [
            Key::Tab,
            Key::Section,
            Key::Esc,
            Key::Q,
            Key::W,
            Key::Left,
            Key::Right,
            Key::Up,
            Key::Down,
        ] {
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
        step(&mut s, Key::Q, true, false);
        step(&mut s, Key::Win, true, false);

        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Tab, false, false);
        let a = step(&mut s, Key::Q, false, false);
        assert_eq!(a.event, Some(Event::CloseApp));
        let a = step(&mut s, Key::Q, true, false);
        assert!(a.swallow && a.event.is_none());
    }

    #[test]
    fn q_autorepeat_closes_only_one_app() {
        let mut s = IDLE;
        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Tab, false, false);
        assert_eq!(step(&mut s, Key::Q, false, false).event, Some(Event::CloseApp));
        // Held Q autorepeats: swallowed, but nothing more closes.
        let a = step(&mut s, Key::Q, false, false);
        assert!(a.swallow && a.event.is_none());
        // Release and press again: the next app can be closed.
        step(&mut s, Key::Q, true, false);
        assert_eq!(step(&mut s, Key::Q, false, false).event, Some(Event::CloseApp));

        // Q kept held over a session boundary is not a fresh press either.
        step(&mut s, Key::Win, true, false);
        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Tab, false, false);
        let a = step(&mut s, Key::Q, false, false);
        assert!(a.swallow && a.event.is_none());
        step(&mut s, Key::Q, true, false);
        assert_eq!(step(&mut s, Key::Q, false, false).event, Some(Event::CloseApp));
    }

    #[test]
    fn w_closes_one_window_only_in_win_mode() {
        let mut s = IDLE;
        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Tab, false, false);
        let a = step(&mut s, Key::W, false, false);
        assert!(a.swallow && a.event.is_none(), "swallowed but no-op in app mode");
        step(&mut s, Key::W, true, false);
        step(&mut s, Key::Win, true, false);

        step(&mut s, Key::Win, false, false);
        step(&mut s, Key::Section, false, false);
        let a = step(&mut s, Key::W, false, false);
        assert!(a.swallow);
        assert_eq!(a.event, Some(Event::CloseWindow));
        // Held W autorepeats: swallowed, but no second window closes.
        let a = step(&mut s, Key::W, false, false);
        assert!(a.swallow && a.event.is_none());
        let a = step(&mut s, Key::W, true, false);
        assert!(a.swallow && a.event.is_none());
        // A fresh press closes the next one.
        assert_eq!(
            step(&mut s, Key::W, false, false).event,
            Some(Event::CloseWindow)
        );
    }

    #[test]
    fn event_wparam_roundtrip() {
        use Event::*;
        for ev in [
            AppNext, AppPrev, WinNext, WinPrev, Commit, Cancel, CloseApp, CloseWindow,
        ] {
            assert_eq!(Event::from_wparam(ev as usize), Some(ev));
        }
        assert_eq!(Event::from_wparam(999), None);
    }
}
