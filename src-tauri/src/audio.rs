/// Audio device enumeration and WASAPI loopback capture — pure Rust, zero Python.
use serde::{Deserialize, Serialize};
use anyhow::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id:   String,   // WASAPI endpoint ID
    pub name: String,
    pub kind: String,   // "input" | "output"
}

// ── Public API ────────────────────────────────────────────────────────────────

pub fn list_input_devices() -> Vec<AudioDevice> {
    #[cfg(windows)] { wasapi_enum(false).unwrap_or_default() }
    #[cfg(not(windows))] { vec![] }
}

pub fn list_output_devices() -> Vec<AudioDevice> {
    #[cfg(windows)] { wasapi_enum(true).unwrap_or_default() }
    #[cfg(not(windows))] { vec![] }
}

// ── Windows WASAPI enumeration ────────────────────────────────────────────────

#[cfg(windows)]
fn wasapi_enum(render: bool) -> Result<Vec<AudioDevice>> {
    use windows::{
        core::{BSTR, PWSTR},
        Win32::{
            Media::Audio::{
                IMMDeviceEnumerator, MMDeviceEnumerator,
                eCapture, eRender, DEVICE_STATE_ACTIVE,
            },
            Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
            System::Com::{
                CoInitializeEx, CoUninitialize, CoCreateInstance,
                CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
            },
            UI::Shell::PropertiesSystem::IPropertyStore,
        },
    };

    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_err() && hr.0 != -2147417850i32 { hr.ok()?; }
    }
    let _com = ComUninitGuard;

    let flow = if render { eRender } else { eCapture };
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?
    };
    let collection = unsafe {
        enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?
    };
    let count = unsafe { collection.GetCount()? };
    let mut out = Vec::with_capacity(count as usize);

    for i in 0..count {
        let Ok(device) = (unsafe { collection.Item(i) }) else { continue };

        // Endpoint ID
        let id_pwstr: PWSTR = match unsafe { device.GetId() } {
            Ok(p) => p, Err(_) => continue,
        };
        let id = unsafe { id_pwstr.to_string().unwrap_or_default() };
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(id_pwstr.as_ptr() as _)) };

        // Friendly name via IPropertyStore
        let name = unsafe {
            device.OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|store: IPropertyStore| {
                    store.GetValue(&PKEY_Device_FriendlyName).ok()
                        .and_then(|pv| BSTR::try_from(&pv).ok())
                        .map(|b| b.to_string())
                })
                .unwrap_or_else(|| "Unknown".into())
        };

        out.push(AudioDevice {
            id,
            name,
            kind: if render { "output".into() } else { "input".into() },
        });
    }

    Ok(out)
}

#[cfg(windows)]
struct ComUninitGuard;
#[cfg(windows)]
impl Drop for ComUninitGuard {
    fn drop(&mut self) { unsafe { windows::Win32::System::Com::CoUninitialize(); } }
}

// ── WASAPI loopback capture → named pipe ──────────────────────────────────────
//
// Flow: Rust WASAPI loopback → Windows named pipe → FFmpeg (reads as raw f32le PCM)
// FFmpeg opens the pipe as its first input: -f f32le -ar 44100 -ac 2 -i \\.\pipe\...

/// Format discovered from the WASAPI device — shared between capture thread and caller.
#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels:    u16,
}

#[cfg(windows)]
pub struct SysAudioCapture {
    stop:    std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread:  Option<std::thread::JoinHandle<()>>,
    /// Actual format used by the WASAPI device (available after start() returns).
    pub format: AudioFormat,
}

#[cfg(windows)]
impl SysAudioCapture {
    pub fn start(device_id: String) -> Result<Self> {
        use std::sync::{Arc, Mutex, atomic::AtomicBool};

        let stop    = Arc::new(AtomicBool::new(false));
        let stop2   = Arc::clone(&stop);
        // ready carries (pipe_ready: bool, format: Option<AudioFormat>)
        let ready:  Arc<Mutex<Option<AudioFormat>>> = Arc::new(Mutex::new(None));
        let ready2  = Arc::clone(&ready);

        let thread = std::thread::Builder::new()
            .name("sys-audio".into())
            .spawn(move || {
                if let Err(e) = capture_loop(&device_id, &stop2, &ready2) {
                    log::warn!("sys-audio: {e}");
                    // Ensure ready is set even on error so caller doesn't hang
                    *ready2.lock().unwrap() = Some(AudioFormat { sample_rate: 48000, channels: 2 });
                }
            })?;

        // Wait up to 2s for the pipe + format to be ready
        let t0 = std::time::Instant::now();
        let format = loop {
            {
                let g = ready.lock().unwrap();
                if let Some(ref f) = *g {
                    break f.clone();
                }
            }
            if t0.elapsed().as_secs() >= 2 {
                break AudioFormat { sample_rate: 48000, channels: 2 };
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };

        Ok(SysAudioCapture { stop, thread: Some(thread), format })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() { let _ = t.join(); }
    }
}

#[cfg(windows)]
fn capture_loop(
    device_id: &str,
    stop:      &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready:     &std::sync::Arc<std::sync::Mutex<Option<AudioFormat>>>,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        core::PWSTR,
        Win32::{
            Foundation::CloseHandle,
            Media::Audio::{
                IMMDeviceEnumerator, MMDeviceEnumerator,
                IAudioClient, IAudioCaptureClient,
                AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
            },
            System::{
                Com::{
                    CoInitializeEx, CoCreateInstance, CLSCTX_ALL, COINIT_MULTITHREADED,
                },
                Pipes::{
                    CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT,
                },
            },
            Storage::FileSystem::{
                WriteFile, PIPE_ACCESS_OUTBOUND,
            },
        },
    };

    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok(); }
    let _com = ComUninitGuard;

    // Resolve WASAPI device
    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?
    };
    let wide_id: Vec<u16> = OsStr::new(device_id).encode_wide().chain([0]).collect();
    let device = unsafe {
        enumerator.GetDevice(windows::core::PCWSTR(wide_id.as_ptr()))?
    };

    // Activate IAudioClient for loopback capture with event-driven mode.
    // AUDCLNT_STREAMFLAGS_EVENTCALLBACK avoids the Sleep(5ms) polling loop
    // that interferes with DWM's vsync signal and causes ddagrab stalls.
    use windows::Win32::Media::Audio::AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};

    let audio_client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let mix_fmt = unsafe { audio_client.GetMixFormat()? };
    unsafe {
        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            10_000_000, 0, mix_fmt, None,
        )?;
    }
    let channels    = unsafe { (*mix_fmt).nChannels as usize };
    let bits        = unsafe { (*mix_fmt).wBitsPerSample };
    let sample_rate = unsafe { (*mix_fmt).nSamplesPerSec };
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(mix_fmt as *mut _ as *const _)); }

    // Create event that WASAPI signals when audio data is ready
    let audio_event = unsafe { CreateEventW(None, false, false, None)? };
    unsafe { audio_client.SetEventHandle(audio_event)?; }

    let capture_client: IAudioCaptureClient = unsafe { audio_client.GetService()? };

    // Create named pipe (write/server end)
    let pipe_name: Vec<u16> = OsStr::new(r"\\.\pipe\cliplite_sysaudio")
        .encode_wide().chain([0]).collect();
    let pipe = unsafe {
        use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
        let h = CreateNamedPipeW(
            windows::core::PCWSTR(pipe_name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(2), // PIPE_ACCESS_OUTBOUND
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1, 1 << 20, 65536, 0, None,
        );
        if h == windows::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err(anyhow::anyhow!("CreateNamedPipeW failed"));
        }
        h
    };

    // Signal caller with the actual audio format so FFmpeg args use the correct rate
    *ready.lock().unwrap() = Some(AudioFormat {
        sample_rate,
        channels: channels as u16,
    });

    // Block until FFmpeg connects as reader (ignore error — ERROR_PIPE_CONNECTED is ok)
    unsafe {
        windows::Win32::System::Pipes::ConnectNamedPipe(pipe, None).ok();
    }

    unsafe { audio_client.Start()?; }

    let mut f32_buf: Vec<f32> = Vec::new();

    'outer: while !stop.load(Ordering::Relaxed) {
        // Wait for WASAPI to signal that audio data is ready.
        // Uses WaitForSingleObject with a 100ms timeout so we can check the
        // stop flag periodically without sleeping or polling.
        // This replaces the Sleep(5ms) polling loop which interfered with
        // DWM's vsync timer and caused ddagrab to stall every ~500ms.
        use windows::Win32::Foundation::WAIT_EVENT;
        const WAIT_TIMEOUT: WAIT_EVENT = WAIT_EVENT(0x00000102);
        let wait_result = unsafe { WaitForSingleObject(audio_event, 100) };
        if wait_result == WAIT_TIMEOUT {
            continue; // no data yet — check stop flag and wait again
        }

        // Drain all available packets from the capture buffer
        loop {
            let pkt = match unsafe { capture_client.GetNextPacketSize() } {
                Ok(0) => break, // no more packets this wake
                Ok(n) => n,
                Err(_) => break 'outer,
            };

            let mut data_ptr: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags  = 0u32;

            if unsafe { capture_client.GetBuffer(&mut data_ptr, &mut frames, &mut flags, None, None) }.is_err() {
                break 'outer;
            }

            if frames > 0 && !data_ptr.is_null() {
                let n = frames as usize * channels;
                f32_buf.clear();
                if bits == 32 {
                    let slice = unsafe { std::slice::from_raw_parts(data_ptr as *const f32, n) };
                    f32_buf.extend_from_slice(slice);
                } else {
                    let slice = unsafe { std::slice::from_raw_parts(data_ptr as *const i16, n) };
                    for &s in slice { f32_buf.push(s as f32 / 32768.0); }
                }
            }
            unsafe { capture_client.ReleaseBuffer(frames).ok(); }

            if !f32_buf.is_empty() {
                let bytes = unsafe {
                    std::slice::from_raw_parts(f32_buf.as_ptr() as *const u8, f32_buf.len() * 4)
                };
                let mut written = 0u32;
                if unsafe { WriteFile(pipe, Some(bytes), Some(&mut written), None) }.is_err() {
                    break 'outer; // FFmpeg closed the read end
                }
            }
        }
    }

    unsafe { audio_client.Stop().ok(); }
    unsafe { CloseHandle(audio_event).ok(); }
    unsafe { CloseHandle(pipe).ok(); }
    Ok(())
}

// ── Non-Windows stubs ─────────────────────────────────────────────────────────
#[cfg(not(windows))]
pub struct SysAudioCapture;
#[cfg(not(windows))]
impl SysAudioCapture {
    pub fn start(_: String) -> Result<Self> { Ok(SysAudioCapture) }
    pub fn stop(&mut self) {}
}
