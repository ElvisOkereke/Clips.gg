/// Recording process management for the standalone recorder binary.
/// Identical logic to src/recorder.rs but without Tauri State wrappers.
use std::io::Write;
use std::process::Child;
use std::time::Instant;
use anyhow::{anyhow, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RecordingStatus {
    pub is_recording: bool,
    pub is_paused:    bool,
    pub elapsed_seconds: f64,
}

pub struct RecorderInner {
    pub process:         Option<Child>,
    pub output_path:     Option<String>,
    pub start_time:      Option<Instant>,
    pub paused_duration: f64,
    pub pause_start:     Option<Instant>,
    pub is_paused:       bool,
    #[cfg(windows)]
    pub sys_audio:       Option<crate::audio::SysAudioCapture>,
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
        }
    }
}

impl RecorderInner {
    pub fn status(&mut self) -> RecordingStatus {
        let alive = if let Some(proc) = &mut self.process {
            matches!(proc.try_wait(), Ok(None))
        } else { false };

        if !alive && self.process.is_some() {
            self.process    = None;
            self.start_time = None;
            self.is_paused  = false;
        }
        RecordingStatus {
            is_recording:    alive,
            is_paused:       self.is_paused,
            elapsed_seconds: if alive { self.elapsed() } else { 0.0 },
        }
    }

    fn elapsed(&self) -> f64 {
        let Some(s) = self.start_time else { return 0.0 };
        let e = s.elapsed().as_secs_f64() - self.paused_duration;
        if self.is_paused {
            if let Some(ps) = self.pause_start { return (e - ps.elapsed().as_secs_f64()).max(0.0); }
        }
        e.max(0.0)
    }

    pub fn start(&mut self, cmd: &[String], output_path: String) -> Result<()> {
        if let Some(p) = &mut self.process {
            if matches!(p.try_wait(), Ok(None)) { return Err(anyhow!("Already recording")); }
        }
        let child = spawn_ffmpeg(cmd)?;
        self.process         = Some(child);
        self.output_path     = Some(output_path);
        self.start_time      = Some(Instant::now());
        self.paused_duration = 0.0;
        self.pause_start     = None;
        self.is_paused       = false;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<String> {
        let path = self.output_path.take().unwrap_or_default();
        #[cfg(windows)]
        if let Some(mut cap) = self.sys_audio.take() { cap.stop(); }
        let Some(mut proc) = self.process.take() else { return Ok(path); };
        self.start_time = None;
        self.is_paused  = false;
        if let Some(mut stdin) = proc.stdin.take() {
            let _ = stdin.write_all(b"q"); let _ = stdin.flush();
        }
        let t0 = Instant::now();
        loop {
            if let Ok(Some(_)) = proc.try_wait() { break; }
            if t0.elapsed().as_secs() > 20 { let _ = proc.kill(); let _ = proc.wait(); break; }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Ok(path)
    }

    pub fn pause(&mut self) -> Result<()> {
        if self.is_paused { return Ok(()); }
        let Some(proc) = &self.process else { return Err(anyhow!("Not recording")); };
        suspend_process(proc.id())?;
        self.pause_start = Some(Instant::now());
        self.is_paused   = true;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        if !self.is_paused { return Ok(()); }
        let Some(proc) = &self.process else { return Err(anyhow!("Not recording")); };
        resume_process(proc.id())?;
        if let Some(ps) = self.pause_start.take() { self.paused_duration += ps.elapsed().as_secs_f64(); }
        self.is_paused = false;
        Ok(())
    }
}

fn spawn_ffmpeg(cmd: &[String]) -> Result<Child> {
    use std::process::{Command, Stdio};
    let exe = cmd.first().ok_or_else(|| anyhow!("Empty command"))?;
    let args = &cmd[1..];

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let child = Command::new(exe).args(args)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW).spawn()
            .map_err(|e| anyhow!("Spawn failed: {e}"))?;
        elevate_priority(child.id());
        return Ok(child);
    }
    #[cfg(not(windows))]
    Command::new(exe).args(args)
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
        .spawn().map_err(|e| anyhow!("Spawn failed: {e}"))
}

#[cfg(windows)]
fn elevate_priority(pid: u32) {
    use windows::Win32::System::Threading::{OpenProcess, SetPriorityClass, HIGH_PRIORITY_CLASS, PROCESS_ALL_ACCESS};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_ALL_ACCESS, false, pid) {
            let _ = SetPriorityClass(h, HIGH_PRIORITY_CLASS);
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    }
}

#[cfg(windows)]
fn suspend_process(pid: u32) -> Result<()> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    use windows::Win32::System::Threading::{OpenThread, SuspendThread, THREAD_SUSPEND_RESUME};
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(|e| anyhow!("{e}"))?;
        let mut te = THREADENTRY32::default();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut te).is_ok() {
            loop {
                if te.th32OwnerProcessID == pid {
                    if let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME, false, te.th32ThreadID) {
                        SuspendThread(th); let _ = windows::Win32::Foundation::CloseHandle(th);
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
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(|e| anyhow!("{e}"))?;
        let mut te = THREADENTRY32::default();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut te).is_ok() {
            loop {
                if te.th32OwnerProcessID == pid {
                    if let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME, false, te.th32ThreadID) {
                        ResumeThread(th); let _ = windows::Win32::Foundation::CloseHandle(th);
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
fn suspend_process(pid: u32) -> Result<()> { std::process::Command::new("kill").args(["-STOP", &pid.to_string()]).status().ok(); Ok(()) }
#[cfg(not(windows))]
fn resume_process(pid: u32) -> Result<()> { std::process::Command::new("kill").args(["-CONT", &pid.to_string()]).status().ok(); Ok(()) }
