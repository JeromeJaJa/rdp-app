//! Running a command in this session for the client (the API's `POST /run`): hidden, with
//! its output captured, or shown on the desktop (an installer's window, say).
//!
//! A PowerShell command is written to a script file and run with `-File`, so there is no
//! quoting to get wrong. A hidden command runs in a job object, so a timeout ends what it
//! started as well.

use std::io::Read;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use rdp_companion_proto::{RUN_OUTPUT_LIMIT, RunResult, RunSpec, Shell};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Globalization::{CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
use windows::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject};
use windows::Win32::System::Threading::{
    CREATE_NEW_CONSOLE, CREATE_NO_WINDOW, CreateProcessW, GetExitCodeProcess, INFINITE, PROCESS_INFORMATION,
    STARTUPINFOW, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// Once a command has ended, how long its output gets to drain: a program it started can
/// hold the pipes open for as long as that program runs.
const DRAIN: Duration = Duration::from_secs(1);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `spec` in this session, as the signed-in user. An error means it didn't start.
pub(crate) fn run(spec: &RunSpec) -> Result<RunResult, String> {
    let launch = Launch::new(&spec.command, spec.shell, spec.visible)?;
    let timeout = Duration::from_secs(u64::from(spec.timeout_secs));
    if spec.visible {
        shown(launch, spec.wait, timeout)
    } else {
        hidden(launch, spec.wait, timeout)
    }
}

/// What to start: a program, its arguments as one raw string, and the script it runs.
struct Launch {
    program: PathBuf,
    args: String,
    script: Option<Script>,
}

impl Launch {
    fn new(command: &str, shell: Shell, visible: bool) -> Result<Self, String> {
        if command.trim().is_empty() {
            return Err("no command to run".into());
        }
        let system =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())).join("System32");
        Ok(match shell {
            Shell::PowerShell => {
                let script = Script::write(command)?;
                // Someone at the desktop can answer a prompt; nobody can answer a hidden one.
                let interactive = if visible { "" } else { " -NonInteractive" };
                Self {
                    program: system.join(r"WindowsPowerShell\v1.0\powershell.exe"),
                    args: format!(
                        "-NoLogo -NoProfile{interactive} -ExecutionPolicy Bypass -File \"{}\"",
                        script.0.display()
                    ),
                    script: Some(script),
                }
            }
            // `/s`: cmd drops the outer quotes and keeps the rest as typed.
            Shell::Cmd => Self {
                program: system.join("cmd.exe"),
                args: format!("/d /s /c \"{command}\""),
                script: None,
            },
            Shell::Program => {
                let (program, args) = split_program(&crate::apps::expand(command));
                Self {
                    program: program.into(),
                    args,
                    script: None,
                }
            }
        })
    }
}

/// A command line's program (quoted or not) and the rest.
fn split_program(line: &str) -> (String, String) {
    let line = line.trim();
    let (program, rest) = match line.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').unwrap_or((quoted, "")),
        None => line.split_once(char::is_whitespace).unwrap_or((line, "")),
    };
    (program.to_owned(), rest.trim_start().to_owned())
}

/// A PowerShell script file, deleted once its shell is done with it.
struct Script(PathBuf);

impl Script {
    fn write(command: &str) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "rdpapp-run-{}-{}.ps1",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        // Windows PowerShell reads a script without a byte order mark in the ANSI code page.
        // Progress bars are kept out of the output, which is UTF-8; a script that ends on a
        // failing program fails with its exit code.
        let script = format!(
            "\u{feff}$ProgressPreference = 'SilentlyContinue'\r\n\
             try {{ [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false) }} catch {{}}\r\n\
             {command}\r\n\
             if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}\r\n"
        );
        std::fs::write(&path, script).map_err(|e| format!("couldn't write the script: {e}"))?;
        Ok(Self(path))
    }
}

impl Drop for Script {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs it with no window, its output captured.
fn hidden(launch: Launch, wait: bool, timeout: Duration) -> Result<RunResult, String> {
    let mut command = Command::new(&launch.program);
    command
        .raw_arg(&launch.args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null());
    if let Some(home) = std::env::var_os("USERPROFILE") {
        command.current_dir(home);
    }
    if wait {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", launch.program.display()))?;
    let pid = child.id();
    if !wait {
        reap_child(child, launch.script);
        return Ok(RunResult {
            pid,
            ..Default::default()
        });
    }
    // What it starts joins its job, so a timeout can end all of it (bar anything started
    // in the moment before it joined).
    let job = Job::of(&child);
    let (done, drained) = mpsc::channel();
    let stdout = capture(child.stdout.take(), done.clone());
    let stderr = capture(child.stderr.take(), done);
    let ended = wait_for(HANDLE(child.as_raw_handle()), timeout);
    let exit_code = if ended {
        child.wait().ok().and_then(|status| status.code())
    } else {
        match &job {
            Some(job) => job.terminate(),
            None => {
                let _ = child.kill();
            }
        }
        let _ = child.wait();
        None
    };
    let deadline = Instant::now() + DRAIN;
    for _ in 0..2 {
        if drained
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_err()
        {
            break;
        }
    }
    let (stdout, stderr) = (lock(&stdout), lock(&stderr));
    Ok(RunResult {
        pid,
        exit_code,
        stdout: decode(&stdout.bytes),
        stderr: decode(&stderr.bytes),
        truncated: stdout.truncated || stderr.truncated,
        timed_out: !ended,
    })
}

/// Runs it on the desktop, in a console of its own if it's a console program.
fn shown(launch: Launch, wait: bool, timeout: Duration) -> Result<RunResult, String> {
    let mut line: Vec<u16> = format!("\"{}\" {}", launch.program.display(), launch.args)
        .encode_utf16()
        .chain([0])
        .collect();
    let home: Option<Vec<u16>> = std::env::var_os("USERPROFILE").map(|d| d.encode_wide().chain([0]).collect());
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: NUL-terminated buffers that outlive the call; the thread handle is closed
    // straight away and the process handle is owned by `Process`.
    unsafe {
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NEW_CONSOLE,
            None,
            home.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            &startup,
            &mut info,
        )
        .map_err(|e| format!("couldn't start {}: {}", launch.program.display(), e.message()))?;
        let _ = CloseHandle(info.hThread);
    }
    let process = Process(info.hProcess);
    let pid = info.dwProcessId;
    if !wait || !wait_for(process.0, timeout) {
        reap(process, launch.script);
        return Ok(RunResult {
            pid,
            timed_out: wait,
            ..Default::default()
        });
    }
    let mut code = 0u32;
    // SAFETY: our handle to a process that has ended.
    let exit_code = unsafe { GetExitCodeProcess(process.0, &mut code) }
        .ok()
        .map(|()| code as i32);
    Ok(RunResult {
        pid,
        exit_code,
        ..Default::default()
    })
}

/// Waits up to `timeout` for a process to end; true if it did.
fn wait_for(process: HANDLE, timeout: Duration) -> bool {
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX).min(INFINITE - 1);
    // SAFETY: a process handle its owner keeps open for the call.
    unsafe { WaitForSingleObject(process, ms) == WAIT_OBJECT_0 }
}

/// A process handle (closed on drop).
struct Process(HANDLE);

// SAFETY: a process handle can be waited on and closed from any thread.
unsafe impl Send for Process {}

impl Process {
    fn wait(&self) {
        // SAFETY: our open handle.
        unsafe { WaitForSingleObject(self.0, INFINITE) };
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: our handle, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Deletes the script once the process running it ends (it's still running now).
fn reap(process: Process, script: Option<Script>) {
    if let Some(script) = script {
        let _ = std::thread::Builder::new().name("run-reap".into()).spawn(move || {
            process.wait();
            drop(script);
        });
    }
}

fn reap_child(mut child: Child, script: Option<Script>) {
    if let Some(script) = script {
        let _ = std::thread::Builder::new().name("run-reap".into()).spawn(move || {
            let _ = child.wait();
            drop(script);
        });
    }
}

/// A job object holding a hidden command and what it starts. Closing it ends nothing.
struct Job(HANDLE);

impl Job {
    fn of(child: &Child) -> Option<Self> {
        // SAFETY: a new job of our own, and the child's open process handle.
        unsafe {
            let job = Self(CreateJobObjectW(None, PCWSTR::null()).ok()?);
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).ok()?;
            Some(job)
        }
    }

    fn terminate(&self) {
        // SAFETY: our open job handle.
        let _ = unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: our handle, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// One output stream, up to [`RUN_OUTPUT_LIMIT`].
#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Reads `stream` on a thread of its own until it closes (past the limit too, so the
/// program never blocks on a full pipe), then signals `done`.
fn capture(stream: Option<impl Read + Send + 'static>, done: mpsc::Sender<()>) -> Arc<Mutex<Captured>> {
    let captured = Arc::new(Mutex::new(Captured::default()));
    let Some(mut stream) = stream else {
        let _ = done.send(());
        return captured;
    };
    let sink = Arc::clone(&captured);
    let _ = std::thread::Builder::new().name("run-output".into()).spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n @ 1..) = stream.read(&mut buf) {
            let mut captured = lock(&sink);
            let room = RUN_OUTPUT_LIMIT.saturating_sub(captured.bytes.len());
            captured.bytes.extend_from_slice(&buf[..n.min(room)]);
            captured.truncated |= n > room;
        }
        let _ = done.send(());
    });
    captured
}

/// Output as text: UTF-8 if it is (PowerShell's is), else the OEM code page (cmd's, and
/// most console programs').
fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        // Cut mid-character at the limit.
        Err(e) if e.error_len().is_none() => String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
        Err(_) => oem(bytes),
    }
}

fn oem(bytes: &[u8]) -> String {
    let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0);
    // SAFETY: sizing call, then converting into a buffer of that size.
    unsafe {
        let len = MultiByteToWideChar(CP_OEMCP, flags, bytes, None).max(0) as usize;
        let mut wide = vec![0u16; len];
        let written = MultiByteToWideChar(CP_OEMCP, flags, bytes, Some(&mut wide)).max(0) as usize;
        String::from_utf16_lossy(&wide[..written.min(len)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(command: &str, shell: Shell) -> RunSpec {
        RunSpec {
            command: command.into(),
            shell,
            visible: false,
            wait: true,
            timeout_secs: 60,
        }
    }

    #[test]
    fn runs_powershell_and_captures_its_output() {
        let result = run(&spec(
            "Write-Output 'héllo'; [Console]::Error.WriteLine('oops'); exit 3",
            Shell::PowerShell,
        ))
        .unwrap();
        assert_eq!(result.exit_code, Some(3));
        assert_eq!(result.stdout.trim(), "héllo");
        assert_eq!(result.stderr.trim(), "oops");
        assert!(!result.timed_out && !result.truncated && result.pid != 0);
    }

    #[test]
    fn hands_back_exit_codes() {
        // A PowerShell script that ends on a failing program fails with its code.
        let result = run(&spec("cmd /c exit 5", Shell::PowerShell)).unwrap();
        assert_eq!(result.exit_code, Some(5));
        let result = run(&spec("echo one & exit /b 7", Shell::Cmd)).unwrap();
        assert_eq!((result.exit_code, result.stdout.trim()), (Some(7), "one"));
    }

    #[test]
    fn runs_a_program_without_a_shell() {
        let result = run(&spec(
            r#""%SystemRoot%\System32\cmd.exe" /c echo %USERNAME%"#,
            Shell::Program,
        ))
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout.trim(), std::env::var("USERNAME").unwrap());
        assert!(run(&spec("no-such-program-here.exe", Shell::Program)).is_err());
        assert!(run(&spec("  ", Shell::Cmd)).is_err());
    }

    #[test]
    fn ends_a_command_that_runs_too_long_with_what_it_started() {
        // A copy of ping under a name nothing else uses: its file stays locked while it runs.
        let dir = std::env::temp_dir().join(format!("rdp-companion-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let exe = dir.join("rdpapp-test-long.exe");
        std::fs::copy(PathBuf::from(system).join(r"System32\PING.EXE"), &exe).unwrap();

        let mut long = spec(&format!("\"{}\" -n 60 127.0.0.1", exe.display()), Shell::Cmd);
        long.timeout_secs = 1;
        let started = Instant::now();
        let result = run(&long).unwrap();
        assert!(result.timed_out && result.exit_code.is_none());
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
        // The ping cmd started was ended too, so its file can go.
        let gone = (0..30).any(|_| {
            std::thread::sleep(Duration::from_millis(100));
            std::fs::remove_file(&exe).is_ok()
        });
        assert!(gone, "what the command started is still running");
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn answers_at_once_when_not_waiting() {
        let mut detached = spec("Start-Sleep 5", Shell::PowerShell);
        detached.wait = false;
        let started = Instant::now();
        let result = run(&detached).unwrap();
        assert!(result.pid != 0 && result.exit_code.is_none() && !result.timed_out);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn cuts_output_at_the_limit() {
        let result = run(&spec("[Console]::Out.Write('x' * 1100000)", Shell::PowerShell)).unwrap();
        assert!(result.truncated);
        assert_eq!(result.stdout.len(), RUN_OUTPUT_LIMIT);
    }

    #[test]
    fn decodes_output() {
        assert_eq!(decode("\u{feff}hé".as_bytes()), "hé");
        // Cut mid-character.
        assert_eq!(decode(&"hé".as_bytes()[..2]), "h");
        assert_eq!(decode(b"plain"), "plain");
    }

    #[test]
    fn splits_a_command_line() {
        let split = |s: &str| split_program(s);
        assert_eq!(
            split(r#""C:\Program Files\x.exe" /S /D=a"#),
            (r"C:\Program Files\x.exe".into(), "/S /D=a".into())
        );
        assert_eq!(split("notepad  a.txt"), ("notepad".into(), "a.txt".into()));
        assert_eq!(split("notepad"), ("notepad".into(), String::new()));
    }
}
