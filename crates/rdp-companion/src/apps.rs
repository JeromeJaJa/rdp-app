//! Launching, restarting and closing a program in this session, for the client's app
//! controls (its context menus' "Launch", "Restart" and "Close").

use std::os::windows::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rdp_companion_proto::AppAction;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcessId, OpenProcess, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, QueryFullProcessImageNameW,
    STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR};

/// How long a program gets to close its windows before it's ended.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// Does `action` to the program at `path` (environment variables such as `%LOCALAPPDATA%`
/// expanded here) in this session. `Ok(true)` if it was running before.
pub(crate) fn control(action: AppAction, path: &str) -> Result<bool, String> {
    let exe = PathBuf::from(expand(path).replace('/', "\\"));
    let running = instances(&exe);
    let was_running = !running.is_empty();
    match action {
        // A second start of a single-instance app brings the running one forward.
        AppAction::Launch => launch(&exe)?,
        AppAction::Close => close(running),
        AppAction::Restart => {
            close(running);
            launch(&exe)?;
        }
    }
    Ok(was_running)
}

fn expand(path: &str) -> String {
    let source = HSTRING::from(path);
    // SAFETY: sizing call, then expanding into a buffer of that size.
    unsafe {
        let len = ExpandEnvironmentStringsW(&source, None) as usize;
        let mut buf = vec![0u16; len.max(1)];
        let written = ExpandEnvironmentStringsW(&source, Some(&mut buf)) as usize;
        String::from_utf16_lossy(&buf[..written.saturating_sub(1).min(buf.len())])
    }
}

/// An open handle to a running instance (closed on drop).
struct Process {
    handle: HANDLE,
    pid: u32,
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: our handle, closed once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// The instances of `exe` running in this session.
fn instances(exe: &Path) -> Vec<Process> {
    let Some(name) = exe.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return Vec::new();
    };
    let want = exe.to_string_lossy().to_lowercase();
    let mut out = Vec::new();
    // SAFETY: a process snapshot walked with a correctly sized entry, and handles opened
    // only to query, wait on and end the matching processes (owned by `Process`).
    unsafe {
        let mut session = 0u32;
        let _ = ProcessIdToSessionId(GetCurrentProcessId(), &mut session);
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let file = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            let pid = entry.th32ProcessID;
            let mut theirs = u32::MAX;
            if file == name
                && ProcessIdToSessionId(pid, &mut theirs).is_ok()
                && theirs == session
                && let Ok(handle) = OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                    false,
                    pid,
                )
            {
                let process = Process { handle, pid };
                if image_path(handle).is_some_and(|p| p.to_lowercase() == want) {
                    out.push(process);
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// A process's executable path.
fn image_path(process: HANDLE) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: a buffer of the length passed.
    unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

/// Asks each instance's windows to close, and ends whatever is still running after the
/// grace period (an app that hides to the tray on close, say).
fn close(processes: Vec<Process>) {
    if processes.is_empty() {
        return;
    }
    let pids: Vec<u32> = processes.iter().map(|p| p.pid).collect();
    // SAFETY: the callback only reads `pids`, which outlives the enumeration.
    let _ = unsafe { EnumWindows(Some(post_close), LPARAM(&raw const pids as isize)) };
    let deadline = Instant::now() + CLOSE_GRACE;
    for process in &processes {
        let left = deadline.saturating_duration_since(Instant::now());
        // SAFETY: waiting on, and if need be ending, a process we hold a handle to.
        unsafe {
            if WaitForSingleObject(process.handle, left.as_millis() as u32) != WAIT_OBJECT_0 {
                tracing::info!(pid = process.pid, "didn't close in time; ending it");
                let _ = TerminateProcess(process.handle, 1);
                let _ = WaitForSingleObject(process.handle, 2000);
            }
        }
    }
}

/// `EnumWindows` callback: posts WM_CLOSE to the visible, unowned windows of the
/// processes in `lparam` (a `&Vec<u32>` of process ids).
unsafe extern "system" fn post_close(window: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `close` passes a live `Vec<u32>`; the rest are queries and a post.
    unsafe {
        let pids = &*(lparam.0 as *const Vec<u32>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        if pids.contains(&pid) && IsWindowVisible(window).as_bool() && GetWindow(window, GW_OWNER).is_err() {
            let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    BOOL(1)
}

/// Starts `exe` in its own folder, in this session (as the signed-in user).
fn launch(exe: &Path) -> Result<(), String> {
    if !exe.is_file() {
        return Err(format!("{} isn't installed here", exe.display()));
    }
    let app: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    let mut command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
    let dir: Option<Vec<u16>> = exe.parent().map(|d| d.as_os_str().encode_wide().chain([0]).collect());
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: NUL-terminated buffers that outlive the call; the new process's handles are
    // closed straight away (it runs on its own).
    unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            PROCESS_CREATION_FLAGS(0),
            None,
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &startup,
            &mut info,
        )
        .map_err(|e| format!("couldn't start {}: {}", exe.display(), e.message()))?;
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    tracing::info!(exe = %exe.display(), "launched");
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    /// A copy of a stock Windows program, under a name nothing else uses.
    fn copy_of(program: &str, name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rdp-companion-apps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = dir.join(name);
        std::fs::copy(Path::new(&system).join("System32").join(program), &exe).unwrap();
        exe
    }

    #[test]
    fn finds_and_closes_a_running_program() {
        let exe = copy_of("PING.EXE", "rdpapp-test-waiter.exe");
        let path = exe.to_string_lossy().into_owned();
        assert_eq!(control(AppAction::Close, &path), Ok(false), "nothing running yet");

        // It has no window to close, so it's ended after the grace period.
        let mut child = Command::new(&exe)
            .args(["-n", "60", "127.0.0.1"])
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        assert_eq!(instances(&exe).len(), 1);
        assert_eq!(control(AppAction::Close, &path), Ok(true));
        assert!(child.try_wait().unwrap().is_some(), "it was ended");
        let _ = std::fs::remove_file(&exe);
    }

    #[test]
    fn launches_what_is_installed() {
        // rundll32 without arguments exits at once, without a window.
        let exe = copy_of("rundll32.exe", "rdpapp-test-launch.exe");
        assert_eq!(control(AppAction::Launch, &exe.to_string_lossy()), Ok(false));
        let missing = exe.with_file_name("not-installed.exe");
        let error = control(AppAction::Launch, &missing.to_string_lossy()).unwrap_err();
        assert!(error.contains("isn't installed"), "{error}");
        std::thread::sleep(Duration::from_millis(500));
        let _ = std::fs::remove_file(&exe);
    }
}
