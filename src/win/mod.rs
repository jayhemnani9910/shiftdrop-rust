//! The Windows app: the wheel and the status pop-up as native windows.
//!
//! What GNOME's Shell extension does on Linux (noticing the drag and Shift,
//! placing the wheel) is done here by `hook`; the wheel is an ordinary OLE drop
//! target (`drop`), and both windows are drawn by `paint`. Everything after the
//! pick (planning, running, the summary) is shared with Linux in `session`.
mod drop;
mod hook;
mod paint;

use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Instant;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{IDropTarget, OleInitialize, RegisterDragDrop};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, HWND_MESSAGE, HWND_TOPMOST, IDC_ARROW, KillTimer,
    LoadCursorW, MSG, PostMessageW, RegisterClassExW, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNORMAL, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetTimer, SetWindowPos, ShowWindow,
    SystemParametersInfoW, TranslateMessage, WM_APP, WM_LBUTTONUP, WM_MOUSEMOVE, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use shiftdrop::formats::name;
use shiftdrop::jobs::sweep_leftovers;
use shiftdrop::ring::{self, SIZE};
use shiftdrop::session::{self, BUSY, Choice, MOVE_ONLY, Wheel};
use shiftdrop::sys;
use shiftdrop::tools::Params;

const WM_SHOW_WHEEL: u32 = WM_APP + 1; // from the hook: wparam = Ctrl held, lparam = point
const WM_SET_TOOLS: u32 = WM_APP + 2; // from the hook: wparam = Ctrl held
const WM_DRAG_ENDED: u32 = WM_APP + 3; // from the hook: left button released
const WM_WORK: u32 = WM_APP + 4; // from a work thread: something in the channel
const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_HIDE_WHEEL: usize = 1;
const TIMER_LINGER: usize = 2;
const SUCCESS_LINGER_MS: u32 = 4000;

/// From the work thread to the window.
enum Update {
    Progress { done: usize, total: usize, fraction: f64 },
    Done { outputs: Vec<PathBuf>, errors: Vec<String> },
}

/// What the status pop-up shows.
struct Status {
    title: String,
    detail: String,
    failed: bool,
    progress: Option<f64>, // Some while working
    outputs: Vec<PathBuf>,
    hover: Option<usize>,
    hovered: bool,
    lingering: bool,
}

struct App {
    main: HWND, // message-only: the hook and work threads post here
    wheel_hwnd: HWND,
    status_hwnd: HWND,
    painter: paint::Painter,
    wheel: Wheel,
    visible: bool,   // drawn; until a drag with files enters, the wheel only catches it
    dropping: bool,  // a drop landed, so the drag's end mustn't hide anything
    origin: POINT,   // wheel's top-left, screen px
    scale: f32,      // wheel's monitor, px per logical px
    busy: bool,
    cancel: Arc<AtomicBool>,
    title: String,
    updates: Option<Receiver<Update>>,
    status: Status,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app. Window messages and COM calls come in one at a time on
/// this thread, so the borrow is never held twice.
fn with_app<T>(f: impl FnOnce(&mut App) -> T) -> Option<T> {
    APP.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn log_path() -> PathBuf {
    sys::cache_dir().join("shiftdrop").join("shiftdrop.log")
}

/// One line in %LOCALAPPDATA%\shiftdrop\shiftdrop.log (cleared at each start), for finding out what a drag did.
pub fn log(message: &str) {
    let secs = STARTED.get_or_init(Instant::now).elapsed().as_secs_f64();
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(file, "{secs:8.2} {message}");
    }
}

pub fn main() -> ExitCode {
    let _ = fs::create_dir_all(log_path().parent().unwrap());
    let _ = fs::write(log_path(), "");
    STARTED.get_or_init(Instant::now);
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log(&format!("could not start: {e}"));
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        OleInitialize(None)?; // drag and drop needs OLE on this thread
    }
    for leftover in sweep_leftovers() {
        log(&format!("removed half-written {} from a job that was killed", leftover.display()));
    }
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None)?.into() };
    let main = window(instance, w!("ShiftDropMain"), Some(main_proc), true)?;
    let wheel_hwnd = window(instance, w!("ShiftDropWheel"), Some(wheel_proc), false)?;
    let status_hwnd = window(instance, w!("ShiftDropStatus"), Some(status_proc), false)?;
    let target: IDropTarget = drop::DropTarget.into();
    unsafe { RegisterDragDrop(wheel_hwnd, &target)? };

    APP.with(|cell| {
        *cell.borrow_mut() = Some(App {
            main,
            wheel_hwnd,
            status_hwnd,
            painter: paint::Painter::new()?,
            wheel: Wheel::default(),
            visible: false,
            dropping: false,
            origin: POINT::default(),
            scale: 1.0,
            busy: false,
            cancel: Arc::default(),
            title: String::new(),
            updates: None,
            status: Status { title: String::new(), detail: String::new(), failed: false, progress: None,
                             outputs: vec![], hover: None, hovered: false, lingering: false },
        });
        Ok::<(), windows::core::Error>(())
    })?;
    hook::start(main);
    log("ready: drag a file and hold Shift");

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

type Proc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// A window of class `class`: message-only, or a borderless always-on-top
/// layered popup that never takes focus (the wheel and the pop-up).
fn window(instance: HINSTANCE, class: PCWSTR, proc_: Option<Proc>, message_only: bool) -> Result<HWND> {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: proc_,
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        if message_only {
            CreateWindowExW(Default::default(), class, w!("ShiftDrop"), Default::default(), 0, 0, 0, 0,
                            Some(HWND_MESSAGE), None, Some(instance), None)
        } else {
            CreateWindowExW(WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, class, w!("ShiftDrop"),
                            WS_POPUP, 0, 0, 1, 1, None, None, Some(instance), None)
        }
    }
}

/// px per logical px on the monitor showing `at`.
fn scale_at(at: POINT, primary: bool) -> f32 {
    unsafe {
        let monitor = MonitorFromPoint(at, if primary { MONITOR_DEFAULTTOPRIMARY } else { MONITOR_DEFAULTTONEAREST });
        let (mut x, mut y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        x as f32 / 96.0
    }
}

fn bring_up(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0,
                             SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

fn hide(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

// --- the wheel -----------------------------------------------------------------

impl App {
    fn paint_wheel(&self) {
        let (center, subtitle) = self.wheel.center();
        let labels = self.wheel.labels();
        let side = SIZE as f32;
        let result = self.painter.present(self.wheel_hwnd, self.origin, side, side, self.scale, |rt, p| {
            paint::wheel(rt, p, &labels, self.wheel.hover, &center, &subtitle, self.visible)
        });
        if let Err(e) = result {
            log(&format!("drawing the wheel failed: {e}"));
        }
    }

    /// Screen px -> the wheel's logical px.
    fn local(&self, (x, y): (i32, i32)) -> (f64, f64) {
        (((x - self.origin.x) as f32 / self.scale) as f64, ((y - self.origin.y) as f32 / self.scale) as f64)
    }

    fn set_hover(&mut self, at: (i32, i32)) {
        let (x, y) = self.local(at);
        let hover = ring::hit(self.wheel.items.len(), x, y, SIZE);
        if hover != self.wheel.hover {
            self.wheel.hover = hover;
            self.paint_wheel();
        }
    }

    fn show_wheel(&mut self, tools: bool, at: POINT) {
        log(&format!("show-wheel tools={tools} at {},{}", at.x, at.y));
        self.wheel = Wheel::new(vec![], tools, false, vec![]);
        self.visible = false;
        self.dropping = false;
        self.scale = scale_at(at, false);
        let half = (SIZE as f32 * self.scale / 2.0) as i32;
        self.origin = POINT { x: at.x - half, y: at.y - half };
        self.paint_wheel();
        bring_up(self.wheel_hwnd);
    }

    fn drag_ended(&mut self) {
        // the drop may still be on its way, as on GNOME
        log(&format!("drag-ended (dropped on wheel: {})", self.dropping));
        unsafe { SetTimer(Some(self.main), TIMER_HIDE_WHEEL, 400, None) };
    }

    fn hide_unless_dropped(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.main), TIMER_HIDE_WHEEL);
        }
        if !self.dropping && !self.wheel.picking {
            hide(self.wheel_hwnd);
            self.visible = false;
        }
    }
}

fn drag_enter(files: Vec<PathBuf>, at: (i32, i32)) {
    with_app(|app| {
        if files.is_empty() {
            // text, a browser's virtual file: get out of the way, so it can land on what's underneath
            hide(app.wheel_hwnd);
            return;
        }
        let names: Vec<String> = files.iter().take(5).map(|f| name(f)).collect();
        app.wheel.files = files;
        app.wheel.refresh();
        log(&format!("drag carries {} file(s): {names:?} -> {} choices", app.wheel.files.len(), app.wheel.items.len()));
        app.visible = true;
        app.wheel.hover = None;
        app.set_hover(at);
        app.paint_wheel();
    });
}

/// Whether the drag over the wheel carries files.
fn drag_over(at: (i32, i32)) -> bool {
    with_app(|app| {
        if app.visible {
            app.set_hover(at);
        }
        app.visible
    }).unwrap_or(false)
}

fn drag_leave() {
    with_app(|app| {
        if app.wheel.hover.take().is_some() {
            app.paint_wheel();
        }
    });
}

/// The drop on the wheel. Returns whether it was taken (as a copy).
fn dropped(files: Vec<PathBuf>, at: (i32, i32), copy: bool) -> bool {
    with_app(|app| {
        log(&format!("drop at {},{} copy allowed={copy}", at.0, at.1));
        app.dropping = true;
        hide(app.wheel_hwnd);
        app.visible = false;
        if !copy {
            app.finished(vec![], vec![MOVE_ONLY.into()]);
            return false;
        }
        if files.is_empty() {
            return false;
        }
        if files != app.wheel.files {
            app.wheel.files = files;
            app.wheel.refresh();
        }
        let (x, y) = app.local(at);
        let Some(index) = ring::hit(app.wheel.items.len(), x, y, SIZE) else {
            return false; // dropped on the centre: cancel
        };
        let choice = app.wheel.items[index].1;
        app.start(app.wheel.files.clone(), choice);
        true
    }).unwrap_or(false)
}

// --- running work ----------------------------------------------------------------

impl App {
    fn start(&mut self, files: Vec<PathBuf>, choice: Choice) {
        match choice {
            Choice::Tool(tool) if tool.asks.is_some() => {
                self.finished(vec![], vec![format!("{} isn't on Windows yet.", tool.title)]);
            }
            _ => self.run_in_background(files, choice, Params::default()),
        }
    }

    fn run_in_background(&mut self, files: Vec<PathBuf>, choice: Choice, params: Params) {
        if self.busy {
            self.finished(vec![], vec![BUSY.into()]);
            return;
        }
        self.busy = true;
        self.cancel = Arc::new(AtomicBool::new(false));
        self.title = choice.title();
        self.working(0, files.len(), 0.0);

        let (send, updates): (Sender<Update>, Receiver<Update>) = channel();
        self.updates = Some(updates);
        let cancel = self.cancel.clone();
        let unreadable = self.wheel.unreadable.clone();
        let main = self.main.0 as isize; // HWND isn't Send; the number is
        thread::spawn(move || {
            let wake = || unsafe {
                let _ = PostMessageW(Some(HWND(main as *mut _)), WM_WORK, WPARAM(0), LPARAM(0));
            };
            let progress = |done, total, fraction| {
                let _ = send.send(Update::Progress { done, total, fraction });
                wake();
            };
            let (outputs, errors) = session::work(&files, choice, &params, &cancel, &unreadable, &progress, &log);
            let _ = send.send(Update::Done { outputs, errors });
            wake();
        });
    }

    fn drain_updates(&mut self) {
        let Some(updates) = self.updates.take() else { return };
        let mut done = None;
        let mut last = None;
        while let Ok(update) = updates.try_recv() {
            match update {
                Update::Progress { done, total, fraction } => last = Some((done, total, fraction)),
                Update::Done { outputs, errors } => done = Some((outputs, errors)),
            }
        }
        if let Some((outputs, errors)) = done {
            self.busy = false;
            self.finished(outputs, errors);
        } else {
            if let Some((done, total, fraction)) = last {
                self.working(done, total, fraction);
            }
            self.updates = Some(updates);
        }
    }
}

// --- the status pop-up -----------------------------------------------------------

impl App {
    fn status_buttons(&self) -> Vec<&'static str> {
        let s = &self.status;
        if s.progress.is_some() {
            return vec!["Cancel"];
        }
        let mut buttons = vec![];
        if s.outputs.len() == 1 {
            buttons.push("Open");
        }
        if !s.outputs.is_empty() {
            buttons.push("Show in folder");
        }
        buttons.push("Close");
        buttons
    }

    fn status_layout(&self) -> Option<paint::StatusLayout> {
        let s = &self.status;
        paint::status_layout(&self.painter, &s.title, &s.detail, s.progress.is_some(), &self.status_buttons()).ok()
    }

    fn paint_status(&self) {
        let Some(layout) = self.status_layout() else { return };
        // top centre of the main screen's work area, like the GNOME extension places it
        let mut area = RECT::default();
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut area as *mut RECT as *mut _),
                                          SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        let scale = scale_at(POINT { x: area.left, y: area.top }, true);
        let width = (paint::STATUS_WIDTH * scale) as i32;
        let origin = POINT { x: area.left + (area.right - area.left - width) / 2, y: area.top + (12.0 * scale) as i32 };
        let s = &self.status;
        let view = paint::StatusView {
            title: &s.title,
            detail: &s.detail,
            failed: s.failed,
            progress: s.progress.unwrap_or(0.0) as f32,
            hover: s.hover,
        };
        let result = self.painter.present(self.status_hwnd, origin, paint::STATUS_WIDTH, layout.height, scale, |rt, p| {
            paint::status(rt, p, &layout, &view)
        });
        if let Err(e) = result {
            log(&format!("drawing the pop-up failed: {e}"));
        }
        bring_up(self.status_hwnd);
    }

    fn working(&mut self, done: usize, total: usize, fraction: f64) {
        self.stop_linger();
        let cancelling = self.cancel.load(Ordering::Relaxed);
        self.status.title = if cancelling { "Cancelling…".into() } else { self.title.clone() };
        self.status.detail = if total > 1 { format!("{} of {total}", done + 1) } else { String::new() };
        self.status.failed = false;
        self.status.progress = Some(fraction);
        self.paint_status();
    }

    fn finished(&mut self, outputs: Vec<PathBuf>, errors: Vec<String>) {
        let summary = session::summary(&outputs, &errors);
        self.status.title = summary.title;
        self.status.detail = summary.detail;
        self.status.failed = summary.failed;
        self.status.progress = None;
        self.status.outputs = outputs;
        self.status.hover = None;
        self.paint_status();
        if !summary.failed {
            self.linger(SUCCESS_LINGER_MS);
        }
    }

    /// Hide the pop-up after `ms`, unless the pointer is on it by then.
    fn linger(&mut self, ms: u32) {
        self.status.lingering = true;
        unsafe { SetTimer(Some(self.status_hwnd), TIMER_LINGER, ms, None) };
    }

    fn stop_linger(&mut self) {
        if self.status.lingering {
            self.status.lingering = false;
            unsafe {
                let _ = KillTimer(Some(self.status_hwnd), TIMER_LINGER);
            }
        }
    }

    fn status_button_at(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = ((lparam.0 & 0xffff) as i16 as f32, ((lparam.0 >> 16) & 0xffff) as i16 as f32);
        let mut area = RECT::default();
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut area as *mut RECT as *mut _),
                                          SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        let scale = scale_at(POINT { x: area.left, y: area.top }, true);
        let (x, y) = (x / scale, y / scale);
        self.status_layout()?.buttons.iter()
            .position(|(_, r)| x >= r.left && x <= r.right && y >= r.top && y <= r.bottom)
    }

    fn status_click(&mut self, button: &str) {
        match button {
            "Cancel" => {
                self.cancel.store(true, Ordering::Relaxed);
                self.status.title = "Cancelling…".into();
                self.paint_status();
            }
            "Open" => {
                if let Some(first) = self.status.outputs.first() {
                    unsafe {
                        ShellExecuteW(None, w!("open"), &HSTRING::from(first.as_os_str()), None, None, SW_SHOWNORMAL);
                    }
                }
                hide(self.status_hwnd);
            }
            "Show in folder" => {
                if let Some(first) = self.status.outputs.first() {
                    use std::os::windows::process::CommandExt;
                    // explorer wants /select,"path" as one raw argument
                    let _ = std::process::Command::new("explorer.exe")
                        .raw_arg(format!("/select,\"{}\"", first.display()))
                        .spawn();
                }
                hide(self.status_hwnd);
            }
            _ => hide(self.status_hwnd),
        }
    }
}

// --- window procedures ------------------------------------------------------------

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SHOW_WHEEL => {
            with_app(|app| app.show_wheel(wparam.0 != 0, hook::unpack(lparam.0)));
        }
        WM_SET_TOOLS => {
            with_app(|app| {
                app.wheel.set_tools(wparam.0 != 0);
                if app.visible {
                    app.paint_wheel();
                }
            });
        }
        WM_DRAG_ENDED => {
            with_app(|app| app.drag_ended());
        }
        WM_TIMER if wparam.0 == TIMER_HIDE_WHEEL => {
            with_app(|app| app.hide_unless_dropped());
        }
        WM_WORK => {
            with_app(|app| app.drain_updates());
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

unsafe extern "system" fn wheel_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn status_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            with_app(|app| {
                if !app.status.hovered {
                    app.status.hovered = true;
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                }
                let hover = app.status_button_at(lparam);
                if hover != app.status.hover {
                    app.status.hover = hover;
                    app.paint_status();
                }
            });
        }
        WM_MOUSELEAVE => {
            with_app(|app| {
                app.status.hovered = false;
                if app.status.hover.take().is_some() {
                    app.paint_status();
                }
                if !app.status.lingering && app.status.progress.is_none() && !app.status.failed {
                    app.linger(1500);
                }
            });
        }
        WM_LBUTTONUP => {
            with_app(|app| {
                let buttons = app.status_buttons();
                if let Some(i) = app.status_button_at(lparam) {
                    app.status_click(buttons[i]);
                }
            });
        }
        WM_TIMER if wparam.0 == TIMER_LINGER => {
            with_app(|app| {
                app.stop_linger();
                if !app.status.hovered {
                    hide(app.status_hwnd);
                }
            });
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}
