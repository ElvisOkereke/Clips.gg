/// Recording process management — pure Rust.
use std::io::Write;
use std::process::Child;
use std::sync::Mutex;
use std::time::Instant;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

// ── RecorderChild — Win32 handle wrapper ─────────────────────────────────────
//
// Wraps the process and pipe handles produced by our custom CreateProcessW
// call (with bInheritHandles whitelist). This ensures NO D3D/GPU handles
// from the Tauri/WebView2 process leak into the recorder subprocess.

#[cfg(windows)]
pub struct RecorderChild {
    pub process_handle: windows::Win32::Foundation::HANDLE,
    pub thread_handle:  windows::Win32::Foundation::HANDLE,
    pub pid:            u32,
    /// Write end of stdin pipe — Tauri writes IPC commands here
    pub stdin_write:    windows::Win32::Foundation::HANDLE,
    /// Read end of stdout pipe — Tauri reads IPC events here
    pub stdout_read:    windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl RecorderChild {
    /// Send a newline-terminated JSON string to the recorder's stdin.
    pub fn send_line(&self, line: &str) -> Result<()> {
        use windows::Win32::Storage::FileSystem::WriteFile;
        let bytes = line.as_bytes();
        let mut written = 0u32;
        unsafe {
            WriteFile(self.stdin_write, Some(bytes), Some(&mut written), None)
                .map_err(|e| anyhow!("WriteFile: {e}"))?;
            let nl = b"\n";
            WriteFile(self.stdin_write, Some(nl), Some(&mut written), None).ok();
        }
        Ok(())
    }

    /// Close the stdin write handle, sending EOF to the recorder's stdin reader.
    /// The recorder's main loop exits on EOF, triggering a clean shutdown.
    pub fn close_stdin(&mut self) {
        let h = self.stdin_write;
        // HANDLE(*mut c_void): null = closed/invalid
        if !h.0.is_null() {
            unsafe { windows::Win32::Foundation::CloseHandle(h).ok(); }
            self.stdin_write = windows::Win32::Foundation::HANDLE(std::ptr::null_mut());
        }
    }

    /// Read one line from the recorder's stdout (blocking).
    pub fn read_line(&self) -> Option<String> {
        use windows::Win32::Storage::FileSystem::ReadFile;
        let mut buf = vec![0u8; 4096];
        let mut read = 0u32;
        unsafe {
            ReadFile(self.stdout_read, Some(&mut buf), Some(&mut read), None).ok()?;
        }
        if read == 0 { return None; }
        String::from_utf8(buf[..read as usize].to_vec()).ok()
            .map(|s| s.trim_end_matches(['\r', '\n']).to_string())
    }

    /// Non-blocking check if the process has exited. Returns Some(exit_code) if done.
    pub fn try_wait(&self) -> Option<u32> {
        use windows::Win32::System::Threading::WaitForSingleObject;
        use windows::Win32::Foundation::WAIT_OBJECT_0;
        unsafe {
            let r = WaitForSingleObject(self.process_handle, 0);
            if r == WAIT_OBJECT_0 {
                let mut code = 0u32;
                windows::Win32::System::Threading::GetExitCodeProcess(
                    self.process_handle, &mut code
                ).ok();
                Some(code)
            } else {
                None
            }
        }
    }

    /// Close all handles. Call after the process has exited.
    pub fn close_handles(&self) {
        unsafe {
            windows::Win32::Foundation::CloseHandle(self.stdin_write).ok();
            windows::Win32::Foundation::CloseHandle(self.stdout_read).ok();
            windows::Win32::Foundation::CloseHandle(self.thread_handle).ok();
            windows::Win32::Foundation::CloseHandle(self.process_handle).ok();
        }
    }

    pub fn kill(&self) {
        unsafe {
            windows::Win32::System::Threading::TerminateProcess(
                self.process_handle, 1
            ).ok();
        }
    }
}

#[cfg(windows)]
impl Drop for RecorderChild {
    fn drop(&mut self) {
        // close_stdin() zeroes the handle after closing; check before closing again
        let close = |h: windows::Win32::Foundation::HANDLE| {
            if !h.0.is_null() {
                unsafe { windows::Win32::Foundation::CloseHandle(h).ok(); }
            }
        };
        close(self.stdin_write);
        close(self.stdout_read);
        close(self.thread_handle);
        close(self.process_handle);
    }
}

// SAFETY: RecorderChild contains Windows HANDLE values (pointer-sized integers).
// HANDLEs are safe to send between threads; we never use them concurrently
// because RecorderInner is behind a Mutex.
#[cfg(windows)]
unsafe impl Send for RecorderChild {}
#[cfg(windows)]
unsafe impl Sync for RecorderChild {}

// Stub for non-Windows
#[cfg(not(windows))]
pub struct RecorderChild;
#[cfg(not(windows))]
impl RecorderChild {
    pub fn send_line(&self, _: &str) -> Result<()> { Ok(()) }
    pub fn read_line(&self) -> Option<String> { None }
    pub fn try_wait(&self) -> Option<u32> { None }
    pub fn close_handles(&self) {}
    pub fn kill(&self) {}
}

// ── RecordingStatus ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingStatus {
    pub is_recording: bool,
    pub is_paused: bool,
    pub elapsed_seconds: f64,
    pub output_path: Option<String>,
}

// ── RecorderInner ────────────────────────────────────────────────────────────

pub struct RecorderInner {
    pub process:          Option<Child>,
    pub output_path:      Option<String>,
    pub start_time:       Option<Instant>,
    pub paused_duration:  f64,
    pub pause_start:      Option<Instant>,
    pub is_paused:        bool,

    // System audio capture handle (Windows only, fallback path)
    #[cfg(windows)]
    pub sys_audio: Option<crate::audio::SysAudioCapture>,

    // Recorder subprocess via Win32 (preferred — no handle inheritance)
    pub recorder_child: Option<RecorderChild>,

    // Old std::process::Child path — kept only for status() check of fallback
    pub recorder_proc: Option<std::process::Child>,
}

impl Default for RecorderInner {
    fn default() -> Self {
        Self {
            process:         None,
            output_path:     None,
            start_time:      None,
            paused_duration: 0.0,
            pause_start:     None,
            is_paused:       false,
            #[cfg(windows)]
            sys_audio:       None,
            recorder_child:  None,
            recorder_proc:   None,
        }
    }
}

#[derive(Default)]
pub struct RecorderState(pub Mutex<RecorderInner>);

impl RecorderInner {
    // ── Status ────────────────────────────────────────────────────────────────

    pub fn status(&mut self) -> RecordingStatus {
        // Check Win32 recorder child first
        let alive_win32 = if let Some(child) = &self.recorder_child {
            child.try_wait().is_none()
        } else {
            false
        };

        // Check old recorder_proc (std::process::Child fallback)
        let alive_proc = if let Some(proc) = &mut self.recorder_proc {
            matches!(proc.try_wait(), Ok(None))
        } else {
            false
        };

        // Check direct FFmpeg process (last resort fallback)
        let alive_ffmpeg = if let Some(proc) = &mut self.process {
            matches!(proc.try_wait(), Ok(None))
        } else {
            false
        };

        let alive = alive_win32 || alive_proc || alive_ffmpeg;

        if !alive && self.process.is_some() {
            self.process = None;
            self.start_time = None;
            self.is_paused = false;
        }
        if !alive && self.recorder_child.is_some() {
            self.recorder_child = None;
            self.start_time = None;
        }

        RecordingStatus {
            is_recording:    alive,
            is_paused:       self.is_paused,
            elapsed_seconds: if alive { self.elapsed_seconds() } else { 0.0 },
            output_path:     self.output_path.clone(),
        }
    }

    fn elapsed_seconds(&self) -> f64 {
        let Some(start) = self.start_time else { return 0.0 };
        let elapsed = start.elapsed().as_secs_f64() - self.paused_duration;
        if self.is_paused {
            if let Some(ps) = self.pause_start {
                return (elapsed - ps.elapsed().as_secs_f64()).max(0.0);
            }
        }
        elapsed.max(0.0)
    }

    // ── Start (direct FFmpeg — used only if recorder binary unavailable) ───────

    pub fn start(&mut self, cmd: &[String], output_path: String) -> Result<()> {
        if let Some(proc) = &mut self.process {
            if matches!(proc.try_wait(), Ok(None)) {
                return Err(anyhow!("Already recording"));
            }
        }
        let child = crate::ffmpeg::spawn_ffmpeg(cmd)?;
        self.process         = Some(child);
        self.output_path     = Some(output_path);
        self.start_time      = Some(Instant::now());
        self.paused_duration = 0.0;
        self.pause_start     = None;
        self.is_paused       = false;
        Ok(())
    }

    // ── Stop (direct FFmpeg fallback path) ─────────────────────────────────────

    pub fn stop(&mut self) -> Result<String> {
        let path = self.output_path.take().unwrap_or_default();

        #[cfg(windows)]
        if let Some(mut cap) = self.sys_audio.take() {
            cap.stop();
        }

        let Some(mut proc) = self.process.take() else {
            return Ok(path);
        };
        self.start_time = None;
        self.is_paused  = false;

        if let Some(mut stdin) = proc.stdin.take() {
            let _ = stdin.write_all(b"q");
            let _ = stdin.flush();
        }

        let deadline = Instant::now();
        loop {
            if let Ok(Some(_)) = proc.try_wait() { break; }
            if deadline.elapsed().as_secs() > 20 {
                let _ = proc.kill();
                let _ = proc.wait();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        Ok(path)
    }

    // ── Pause / resume (direct FFmpeg path) ────────────────────────────────────

    pub fn pause(&mut self) -> Result<()> {
        if self.is_paused { return Ok(()); }
        // Pause via recorder child if available
        if let Some(child) = &self.recorder_child {
            child.send_line("{\"cmd\":\"pause\"}").ok();
            self.pause_start = Some(Instant::now());
            self.is_paused   = true;
            return Ok(());
        }
        let Some(proc) = &self.process else { return Err(anyhow!("Not recording")); };
        suspend_process(proc.id())?;
        self.pause_start = Some(Instant::now());
        self.is_paused   = true;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if !self.is_paused { return Ok(()); }
        if let Some(child) = &self.recorder_child {
            child.send_line("{\"cmd\":\"resume\"}").ok();
            if let Some(ps) = self.pause_start.take() {
                self.paused_duration += ps.elapsed().as_secs_f64();
            }
            self.is_paused = false;
            return Ok(());
        }
        let Some(proc) = &self.process else { return Err(anyhow!("Not recording")); };
        resume_process(proc.id())?;
        if let Some(ps) = self.pause_start.take() {
            self.paused_duration += ps.elapsed().as_secs_f64();
        }
        self.is_paused = false;
        Ok(())
    }
}

// ── Platform process suspend/resume ──────────────────────────────────────────

#[cfg(windows)]
fn suspend_process(pid: u32) -> Result<()> {
    use windows::Win32::System::Threading::{OpenProcess, SuspendThread, PROCESS_ALL_ACCESS};
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::Threading::{OpenThread, THREAD_SUSPEND_RESUME};
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)
            .map_err(|e| anyhow!("snapshot: {e}"))?;
        let mut te = THREADENTRY32::default();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut te).is_ok() {
            loop {
                if te.th32OwnerProcessID == pid {
                    if let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME, false, te.th32ThreadID) {
                        SuspendThread(th);
                        let _ = windows::Win32::Foundation::CloseHandle(th);
                    }
                }
                te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut te).is_err() { break; }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    Ok(())
}

#[cfg(windows)]
fn resume_process(pid: u32) -> Result<()> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)
            .map_err(|e| anyhow!("snapshot: {e}"))?;
        let mut te = THREADENTRY32::default();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut te).is_ok() {
            loop {
                if te.th32OwnerProcessID == pid {
                    if let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME, false, te.th32ThreadID) {
                        ResumeThread(th);
                        let _ = windows::Win32::Foundation::CloseHandle(th);
                    }
                }
                te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
                if Thread32Next(snap, &mut te).is_err() { break; }
            }
        }
        let _ = windows::Win32::Foundation::CloseHandle(snap);
    }
    Ok(())
}

#[cfg(not(windows))]
fn suspend_process(pid: u32) -> Result<()> {
    std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status().ok();
    Ok(())
}

#[cfg(not(windows))]
fn resume_process(pid: u32) -> Result<()> {
    std::process::Command::new("kill").args(["-CONT", &pid.to_string()]).status().ok();
    Ok(())
}
