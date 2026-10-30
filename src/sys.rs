//! What differs between Linux and Windows, in one place: starting programs,
//! finding them, folders, and whether a process still runs.
use std::path::{Path, PathBuf};
use std::process::Command;

/// Ghostscript's command: `gs` on Linux, the console build `gswin64c` on Windows.
#[cfg(unix)]
pub const GHOSTSCRIPT: &str = "gs";
#[cfg(windows)]
pub const GHOSTSCRIPT: &str = "gswin64c";

/// A command for a converter. On Windows it gets no console window: the app
/// has none, so each ffmpeg would otherwise flash one up.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// True when `program` is on PATH, like `shutil.which`.
pub fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| runnable(&dir, program))
}

#[cfg(unix)]
fn runnable(dir: &Path, program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(dir.join(program)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(windows)]
fn runnable(dir: &Path, program: &str) -> bool {
    // `ffmpeg` means ffmpeg.exe (or .cmd, ...): try each extension Windows would
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
    exts.split(';')
        .filter(|e| !e.is_empty())
        .any(|ext| dir.join(format!("{program}{ext}")).is_file())
}

pub fn home() -> PathBuf {
    #[cfg(unix)]
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    home.map(PathBuf::from).unwrap_or_default()
}

/// Per-user cache: `$XDG_CACHE_HOME` or `~/.cache` on Linux, `%LOCALAPPDATA%` on Windows.
pub fn cache_dir() -> PathBuf {
    #[cfg(unix)]
    let dir = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"));
    #[cfg(windows)]
    let dir = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home().join("AppData").join("Local"));
    dir
}

/// Whether this user may create files in `folder`.
#[cfg(unix)]
pub fn can_write(folder: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let folder = if folder.as_os_str().is_empty() { Path::new(".") } else { folder };
    let Ok(c) = std::ffi::CString::new(folder.as_os_str().as_bytes()) else { return false };
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

/// Whether this user may create files in `folder`. Windows has no cheap
/// equivalent of access(W_OK) that honours ACLs, so try it.
#[cfg(windows)]
pub fn can_write(folder: &Path) -> bool {
    let folder = if folder.as_os_str().is_empty() { Path::new(".") } else { folder };
    let probe = folder.join(format!(".shiftdrop-write-test-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Whether process `pid` still runs (another user's counts).
#[cfg(unix)]
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else { return false };
    // signal 0 only checks; EPERM means it exists but belongs to someone else
    unsafe { libc::kill(pid, 0) == 0 || *libc::__errno_location() != libc::ESRCH }
}

/// Whether process `pid` still runs (another user's counts).
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let mut code = 0;
                let running = GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
                let _ = CloseHandle(handle);
                running
            }
            // no such process; anything else (access denied) means it exists
            Err(e) => e.code() != ERROR_INVALID_PARAMETER.to_hresult(),
        }
    }
}
