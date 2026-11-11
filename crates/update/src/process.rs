// SPDX-License-Identifier: GPL-3.0-or-later
//! Starting programs: the Open3D apps themselves, the file manager, and helper tools.

use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// A `Command` that won't flash a console window on Windows, and clears AppImage-injected
/// environment variables that corrupt system helper binaries and external applications.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Starts `cmd` without waiting for it.
///
/// On Unix the process is reaped when it exits: a thread that waits for the process starts
/// first and starts the process itself. When no thread can be started, the call returns an error
/// and does not start the process. On Windows the process starts on the calling thread, and no
/// thread waits for it.
pub fn spawn_detached(mut cmd: Command) -> Result<(), String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if cfg!(windows) {
        return cmd.spawn().map(drop).map_err(|e| format!("couldn't start it ({e})"));
    }
    spawn_detached_with(
        move || cmd.spawn(),
        |wait| {
            std::thread::Builder::new()
                .name("reap-child".into())
                .spawn(move || {
                    let _ = wait();
                })
                .map(drop)
        },
    )
}

/// The thread that [`spawn_detached`] starts on Unix: `start_thread` starts a thread that runs
/// the closure it is given, and that closure calls `spawn` and then waits for the process.
/// `spawn` is called on that thread only, so when `start_thread` fails nothing is started.
fn spawn_detached_with(
    spawn: impl FnOnce() -> std::io::Result<Child> + Send + 'static,
    start_thread: impl FnOnce(Box<dyn FnOnce() -> Option<ExitStatus> + Send>) -> std::io::Result<()>,
) -> Result<(), String> {
    let (started, start_result) = mpsc::channel();
    start_thread(Box::new(move || start_and_wait(spawn, &started))).map_err(|e| format!("couldn't start a thread to wait for it ({e})"))?;
    match start_result.recv() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("couldn't start it ({e})")),
        Err(_) => Err("couldn't start it (its thread ended before starting it)".into()),
    }
}

/// Calls `spawn`, sends whether that worked on `started`, and waits for the process to exit.
/// Returns its exit status, or `None` when it didn't start or couldn't be waited for.
fn start_and_wait(spawn: impl FnOnce() -> std::io::Result<Child>, started: &mpsc::Sender<std::io::Result<()>>) -> Option<ExitStatus> {
    match spawn() {
        Ok(mut child) => {
            let _ = started.send(Ok(()));
            child.wait().ok()
        }
        Err(e) => {
            let _ = started.send(Err(e));
            None
        }
    }
}

/// Waits up to `timeout` for the process `pid` to exit, and says whether it has. A process that
/// can't be opened, because it's gone or never was, counts as exited.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    use winsafe::{HPROCESS, co};
    let Ok(process) = HPROCESS::OpenProcess(co::PROCESS::SYNCHRONIZE, false, pid) else { return true };
    // u32::MAX would mean INFINITE.
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    process.WaitForSingleObject(Some(ms)).is_ok_and(|w| w == co::WAIT::OBJECT_0)
}

#[cfg(test)]
mod run_tests {
    use super::*;

    /// A shell command line, for the platform's shell.
    fn shell(script: &str) -> (&'static str, [&str; 2]) {
        if cfg!(windows) { ("cmd", ["/C", script]) } else { ("/bin/sh", ["-c", script]) }
    }

    #[cfg(windows)]
    #[test]
    fn waiting_for_a_process_ends_when_it_exits() {
        let mut child = Command::new("ping").args(["-n", "3", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
        assert!(!wait_for_exit(child.id(), Duration::from_millis(200)), "it's still running");
        assert!(wait_for_exit(child.id(), Duration::from_secs(30)), "it has exited");
        child.wait().unwrap();
    }

    /// The thread that starts a detached process waits for it, so the process is reaped when it
    /// exits: only a wait gives its exit status.
    #[test]
    fn a_detached_process_is_waited_for() {
        let (program, args) = shell("exit 7");
        let mut cmd = command(program);
        cmd.args(args);
        let (status_sent, status) = mpsc::channel();
        let result = spawn_detached_with(
            move || cmd.spawn(),
            move |wait| {
                std::thread::Builder::new()
                    .spawn(move || {
                        let _ = status_sent.send(wait());
                    })
                    .map(drop)
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(status.recv().ok().flatten().and_then(|status| status.code()), Some(7), "the waiting thread gets the exit status");
    }

    /// When no thread can be started to wait for a process, the process is not started and the
    /// call says why.
    #[test]
    fn no_process_starts_without_a_thread_to_wait_for_it() {
        let (program, args) = shell("exit 0");
        let mut cmd = command(program);
        cmd.args(args);
        let spawned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let spawn_called = std::sync::Arc::clone(&spawned);
        let result = spawn_detached_with(
            move || {
                spawn_called.store(true, std::sync::atomic::Ordering::SeqCst);
                cmd.spawn()
            },
            |_wait| Err(std::io::Error::other("no threads")),
        );
        assert_eq!(result, Err("couldn't start a thread to wait for it (no threads)".into()));
        assert!(!spawned.load(std::sync::atomic::Ordering::SeqCst), "the process was started");
    }

    #[test]
    fn a_detached_program_that_cannot_start_says_so() {
        let result = spawn_detached(command("craft-update-test-no-such-program"));
        assert!(matches!(&result, Err(e) if e.starts_with("couldn't start it (")), "{result:?}");
    }
}
