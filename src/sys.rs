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

/// A path as a `file:` URL, for LibreOffice's -env:UserInstallation.
pub fn file_uri(path: &Path) -> String {
    #[cfg(unix)]
    let uri = format!("file://{}", path.display());
    #[cfg(windows)]
    let uri = format!("file:///{}", path.display().to_string().replace('\\', "/"));
    uri
}

/// Nothing to do on Linux: PATH is already the user's.
#[cfg(unix)]
pub fn refresh_path() {}

/// Rebuild PATH as a fresh login would see it, plus where the converters hide.
///
/// A program started by an installer (or before winget finished) inherits the
/// PATH from before the converters were installed, so read it from the registry
/// again. Add winget's links folders, LibreOffice's folder (its installer leaves
/// PATH alone) and the Ghostscript shipped next to shiftdrop.exe.
/// Call at startup, before any thread starts.
#[cfg(windows)]
pub fn refresh_path() {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let mut dirs: Vec<PathBuf> = vec![];
    let machine = registry_text(HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment", "Path");
    let user = registry_text(HKEY_CURRENT_USER, "Environment", "Path");
    for list in [machine, user].into_iter().flatten() {
        dirs.extend(std::env::split_paths(&expand(&list)));
    }
    let env_dir = |var: &str| std::env::var_os(var).map(PathBuf::from);
    if let Some(local) = env_dir("LOCALAPPDATA") {
        dirs.push(local.join(r"Microsoft\WinGet\Links"));
    }
    if let Some(programs) = env_dir("ProgramFiles") {
        dirs.push(programs.join(r"WinGet\Links"));
    }
    if let Some(office) = registry_text(HKEY_LOCAL_MACHINE, r"SOFTWARE\LibreOffice\UNO\InstallPath", "") {
        dirs.push(PathBuf::from(office));
    }
    if let Some(here) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        dirs.push(here.join(r"tools\ghostscript\bin"));
    }
    if let Some(current) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&current));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.to_string_lossy().to_lowercase()));
    if let Ok(path) = std::env::join_paths(dirs) {
        // SAFETY: called at startup, before any other thread exists
        unsafe { std::env::set_var("PATH", path) };
    }
}

#[cfg(windows)]
fn registry_text(root: windows::Win32::System::Registry::HKEY, key: &str, value: &str) -> Option<String> {
    use windows::Win32::System::Registry::{RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::HSTRING;
    let (key, value) = (HSTRING::from(key), HSTRING::from(value));
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut bytes = 0u32;
    unsafe {
        if RegGetValueW(root, &key, &value, flags, None, None, Some(&mut bytes)).is_err() {
            return None;
        }
        let mut buf = vec![0u16; bytes as usize / 2 + 1];
        if RegGetValueW(root, &key, &value, flags, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)).is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// `%SystemRoot%\system32` -> `C:\Windows\system32`.
#[cfg(windows)]
fn expand(text: &str) -> String {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows::core::HSTRING;
    let source = HSTRING::from(text);
    unsafe {
        let needed = ExpandEnvironmentStringsW(&source, None);
        let mut buf = vec![0u16; needed as usize];
        let written = ExpandEnvironmentStringsW(&source, Some(&mut buf)) as usize;
        String::from_utf16_lossy(&buf[..written.saturating_sub(1)])
    }
}
