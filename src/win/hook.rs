//! Watching for "a drag is going on and Shift just went down", which on GNOME
//! the Shell extension does. Windows has no event for another app's drag, so a
//! low-level mouse hook sees the left button go down and the pointer travel past
//! the drag threshold; Shift and Ctrl are read from the keyboard state.
//!
//! The hook runs on its own thread and only posts messages: Windows silently
//! removes a hook that is slow to return.
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::thread;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, MSG, MSLLHOOKSTRUCT,
    PostMessageW, SM_CXDRAG, SM_CYDRAG, SetTimer, SetWindowsHookExW, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_TIMER,
};

use super::{WM_DRAG_ENDED, WM_SET_TOOLS, WM_SHOW_WHEEL};

static TARGET: AtomicIsize = AtomicIsize::new(0); // the window that gets our messages
static DOWN: AtomicBool = AtomicBool::new(false);
static DOWN_X: AtomicI32 = AtomicI32::new(0);
static DOWN_Y: AtomicI32 = AtomicI32::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static SHOWN: AtomicBool = AtomicBool::new(false);
static TOOLS: AtomicBool = AtomicBool::new(false);

/// Every so often while the button is down: Shift can go down without the pointer moving.
const POLL_MS: u32 = 30;

/// Start the hook thread; it posts WM_SHOW_WHEEL / WM_SET_TOOLS / WM_DRAG_ENDED to `target`.
pub fn start(target: HWND) {
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    thread::spawn(|| unsafe {
        if SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), None, 0).is_err() {
            super::log("could not install the mouse hook: drags won't open the wheel");
            return;
        }
        SetTimer(None, 0, POLL_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER {
                check_keys(None);
            }
            DispatchMessageW(&msg);
        }
    });
}

fn post(msg: u32, wparam: usize, lparam: isize) {
    let target = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
    unsafe {
        let _ = PostMessageW(Some(target), msg, WPARAM(wparam), LPARAM(lparam));
    }
}

fn down(name: i32) -> bool {
    unsafe { GetAsyncKeyState(name) < 0 }
}

/// Mid-drag: Shift opens the wheel at the pointer, Ctrl flips it between formats and tools.
fn check_keys(at: Option<POINT>) {
    if !DRAGGING.load(Ordering::Relaxed) {
        return;
    }
    let tools = down(VK_CONTROL.0 as i32);
    if !SHOWN.load(Ordering::Relaxed) {
        if down(VK_SHIFT.0 as i32) {
            let at = at.unwrap_or_else(|| {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                }
                p
            });
            SHOWN.store(true, Ordering::Relaxed);
            TOOLS.store(tools, Ordering::Relaxed);
            post(WM_SHOW_WHEEL, tools as usize, pack(at));
        }
    } else if TOOLS.swap(tools, Ordering::Relaxed) != tools {
        post(WM_SET_TOOLS, tools as usize, 0);
    }
}

/// A screen point in one LPARAM, x in the low half; both may be negative on a left/top monitor.
pub fn pack(p: POINT) -> isize {
    ((p.y as i64) << 32 | (p.x as u32 as i64)) as isize
}

pub fn unpack(l: isize) -> POINT {
    let l = l as i64;
    POINT { x: l as i32, y: (l >> 32) as i32 }
}

unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let pt = info.pt;
        match wparam.0 as u32 {
            WM_LBUTTONDOWN => {
                DOWN.store(true, Ordering::Relaxed);
                DRAGGING.store(false, Ordering::Relaxed);
                SHOWN.store(false, Ordering::Relaxed);
                DOWN_X.store(pt.x, Ordering::Relaxed);
                DOWN_Y.store(pt.y, Ordering::Relaxed);
            }
            WM_MOUSEMOVE if DOWN.load(Ordering::Relaxed) => {
                if !DRAGGING.load(Ordering::Relaxed) {
                    let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                    let moved = (pt.x - DOWN_X.load(Ordering::Relaxed)).abs() > cx
                        || (pt.y - DOWN_Y.load(Ordering::Relaxed)).abs() > cy;
                    DRAGGING.store(moved, Ordering::Relaxed);
                }
                check_keys(Some(pt));
            }
            WM_LBUTTONUP => {
                DOWN.store(false, Ordering::Relaxed);
                DRAGGING.store(false, Ordering::Relaxed);
                if SHOWN.swap(false, Ordering::Relaxed) {
                    post(WM_DRAG_ENDED, 0, 0);
                }
            }
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
