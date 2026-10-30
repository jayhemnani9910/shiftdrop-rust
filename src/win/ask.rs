//! The two small dialogs for tools that need a value first: trim (start and end
//! in seconds) and resize (the longest side). Plain Windows controls, like the
//! GTK dialogs on Linux.
use std::path::PathBuf;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET, FW_NORMAL, FONT_CLIP_PRECISION,
    FONT_OUTPUT_PRECISION, HBRUSH, HFONT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW,
    DestroyWindow, GetCursorPos, GetWindowTextW, HMENU, IDC_ARROW, LoadCursorW, RegisterClassExW, SendMessageW,
    SetForegroundWindow, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_SETFONT, WNDCLASSEXW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL, ES_AUTOHSCROLL,
};
use windows::core::{HSTRING, PCWSTR, Result, w};

use shiftdrop::session::RESIZE_EDGES;
use shiftdrop::tools::{Asks, Params, Tool};

const ID_START: i32 = 1; // also the Enter key's button
const ID_CANCEL: i32 = 2; // also Esc
const ID_FIRST: i32 = 10;
const ID_SECOND: i32 = 11;

/// A dialog that is open: what it's for and where its inputs are.
pub struct Ask {
    pub hwnd: HWND,
    tool: &'static Tool,
    files: Vec<PathBuf>,
    first: HWND,          // trim: start; resize: the size list
    second: Option<HWND>, // trim: end
    font: HFONT,
}

impl Ask {
    /// The values typed in, or what's wrong with them.
    pub fn params(&self) -> std::result::Result<Params, String> {
        if self.tool.asks == Some(Asks::Trim) {
            let seconds = |hwnd: HWND, what: &str| {
                text(hwnd).trim().replace(',', ".").parse::<f64>().map_err(|_| format!("{what} isn't a number of seconds"))
            };
            let start = seconds(self.first, "Start")?;
            let end = seconds(self.second.expect("trim has an end field"), "End")?;
            return Ok(Params { start, end, ..Default::default() });
        }
        let picked = unsafe { SendMessageW(self.first, CB_GETCURSEL, None, None).0 };
        let edge = RESIZE_EDGES.get(picked.max(0) as usize).copied().unwrap_or(1920);
        Ok(Params { long_edge: edge, ..Default::default() })
    }

    pub fn into_job(self) -> (&'static Tool, Vec<PathBuf>) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(self.font.into());
        }
        (self.tool, self.files)
    }
}

fn text(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}

/// Open the dialog for `tool` near the pointer. `length` is the clip's length, for trim.
pub fn open(tool: &'static Tool, files: Vec<PathBuf>, length: f64, scale: f32, proc_: super::Proc) -> Result<Ask> {
    let trim = tool.asks == Some(Asks::Trim);
    let px = |n: f32| (n * scale) as i32;
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("ShiftDropAsk");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as isize as *mut _),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc); // fails harmlessly the second time
        let (width, height) = (px(320.0), px(if trim { 210.0 } else { 175.0 }));
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        let title = if trim { w!("Trim") } else { w!("Resize images") };
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_DLGMODALFRAME, class, title,
                                   WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
                                   at.x - width / 2, at.y - height / 2, width, height, None, None, Some(instance), None)?;
        let font = CreateFontW(-px(15.0), 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, DEFAULT_CHARSET,
                               FONT_OUTPUT_PRECISION(0), FONT_CLIP_PRECISION(0), CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
        let control = |class: PCWSTR, label: &str, style: WINDOW_STYLE, (x, y, w, h): (f32, f32, f32, f32), id: i32| {
            let child = CreateWindowExW(Default::default(), class, &HSTRING::from(label), WS_CHILD | WS_VISIBLE | style,
                                        px(x), px(y), px(w), px(h), Some(hwnd), Some(HMENU(id as isize as *mut _)),
                                        Some(instance), None);
            if let Ok(child) = child {
                SendMessageW(child, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            }
            child
        };
        let edit = WS_BORDER | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let (first, second);
        if trim {
            let name = files[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            control(w!("STATIC"), &format!("{name} · {length:.1}s"), WINDOW_STYLE(0), (16.0, 14.0, 280.0, 22.0), -1)?;
            control(w!("STATIC"), "Start (s)", WINDOW_STYLE(0), (16.0, 48.0, 110.0, 22.0), -1)?;
            first = control(w!("EDIT"), "0.0", edit, (140.0, 45.0, 150.0, 26.0), ID_FIRST)?;
            control(w!("STATIC"), "End (s)", WINDOW_STYLE(0), (16.0, 84.0, 110.0, 22.0), -1)?;
            second = Some(control(w!("EDIT"), &format!("{length:.1}"), edit, (140.0, 81.0, 150.0, 26.0), ID_SECOND)?);
        } else {
            control(w!("STATIC"), "Longest side", WINDOW_STYLE(0), (16.0, 18.0, 120.0, 22.0), -1)?;
            first = control(w!("COMBOBOX"), "", WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                            (140.0, 14.0, 150.0, 200.0), ID_FIRST)?;
            for edge in RESIZE_EDGES {
                let label = HSTRING::from(format!("{edge} px"));
                SendMessageW(first, CB_ADDSTRING, None, Some(LPARAM(label.as_ptr() as isize)));
            }
            SendMessageW(first, CB_SETCURSEL, Some(WPARAM(2)), None); // 1920 px
            control(w!("STATIC"), "Smaller images stay as they are.", WINDOW_STYLE(0), (16.0, 52.0, 280.0, 22.0), -1)?;
            second = None;
        }
        let row = if trim { 124.0 } else { 90.0 };
        control(w!("BUTTON"), "Cancel", WS_TABSTOP, (110.0, row, 85.0, 30.0), ID_CANCEL)?;
        control(w!("BUTTON"), "Start", WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32), (205.0, row, 85.0, 30.0),
                ID_START)?;
        let _ = SetForegroundWindow(hwnd);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(first));
        Ok(Ask { hwnd, tool, files, first, second, font })
    }
}

/// What a click or key in the dialog means.
pub enum Action {
    Start,
    Cancel,
}

pub fn action(msg: u32, wparam: WPARAM) -> Option<Action> {
    match msg {
        WM_CLOSE => Some(Action::Cancel),
        WM_COMMAND => match (wparam.0 & 0xffff) as i32 {
            ID_START => Some(Action::Start),
            ID_CANCEL => Some(Action::Cancel),
            _ => None,
        },
        _ => None,
    }
}

pub unsafe fn default(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
