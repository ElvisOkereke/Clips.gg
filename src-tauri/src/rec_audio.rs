/// WASAPI audio capture for the standalone recorder binary.
///
/// Captures BOTH system audio loopback and microphone entirely in Rust/WASAPI.
/// NO dshow. NO DirectX Audio Session from FFmpeg.
///
/// Both streams are mixed in Rust and written as f32le PCM (48000Hz, 2ch)
/// to the named pipe that FFmpeg reads as its single audio input.
use anyhow::Result;

/// Fixed output format for the named pipe.
/// We always request/convert to this regardless of device native format.
const PIPE_SAMPLE_RATE: u32 = 48000;
const PIPE_CHANNELS:    u16 = 2;

#[derive(Debug, Clone)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels:    u16,
}

#[cfg(windows)]
pub struct SysAudioCapture {
    stop:    std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread:  Option<std::thread::JoinHandle<()>>,
    pub format: AudioFormat,
}

#[cfg(windows)]
impl SysAudioCapture {
    pub fn start(sys_device_id: Option<String>, mic_device_id: Option<String>) -> Result<Self> {
        use std::sync::{Arc, Mutex, atomic::AtomicBool};

        let stop   = Arc::new(AtomicBool::new(false));
        let stop2  = Arc::clone(&stop);
        let ready: Arc<Mutex<Option<AudioFormat>>> = Arc::new(Mutex::new(None));
        let ready2 = Arc::clone(&ready);

        let thread = std::thread::Builder::new()
            .name("sys-audio".into())
            .spawn(move || {
                if let Err(e) = capture_loop(sys_device_id.as_deref(), mic_device_id.as_deref(), &stop2, &ready2) {
                    eprintln!("[recorder audio] {e}");
                    *ready2.lock().unwrap() = Some(AudioFormat {
                        sample_rate: PIPE_SAMPLE_RATE,
                        channels:    PIPE_CHANNELS,
                    });
                }
            })?;

        let t0 = std::time::Instant::now();
        let format = loop {
            { let g = ready.lock().unwrap(); if let Some(ref f) = *g { break f.clone(); } }
            if t0.elapsed().as_secs() >= 3 {
                break AudioFormat { sample_rate: PIPE_SAMPLE_RATE, channels: PIPE_CHANNELS };
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

/// Build an explicit IEEE float32 WAVEFORMATEX at our target rate/channels.
/// This avoids all ambiguity about what GetMixFormat returns.
#[cfg(windows)]
unsafe fn make_float_format(rate: u32, channels: u16) -> windows::Win32::Media::Audio::WAVEFORMATEX {
    windows::Win32::Media::Audio::WAVEFORMATEX {
        wFormatTag:      3,   // WAVE_FORMAT_IEEE_FLOAT
        nChannels:       channels,
        nSamplesPerSec:  rate,
        wBitsPerSample:  32,
        nBlockAlign:     channels * 4,
        nAvgBytesPerSec: rate * channels as u32 * 4,
        cbSize:          0,
    }
}

/// Open a WASAPI capture or loopback client.
/// Always initialises with our fixed float32 format (48000 Hz, 2ch).
/// WASAPI will resample/convert from the device's native format.
/// Returns (IAudioClient, IAudioCaptureClient, event_handle).
#[cfg(windows)]
unsafe fn open_wasapi(
    enumerator:  &windows::Win32::Media::Audio::IMMDeviceEnumerator,
    device_id:   &str,
    is_loopback: bool,
) -> Result<(
    windows::Win32::Media::Audio::IAudioClient,
    windows::Win32::Media::Audio::IAudioCaptureClient,
    windows::Win32::Foundation::HANDLE,
)> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::{
        Media::Audio::{
            IAudioClient, IAudioCaptureClient,
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        },
        System::{Com::CLSCTX_ALL, Threading::CreateEventW},
    };

    let wide: Vec<u16> = OsStr::new(device_id).encode_wide().chain([0]).collect();
    let device = enumerator.GetDevice(windows::core::PCWSTR(wide.as_ptr()))?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

    // Request our fixed float32 format.
    // AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM + SRC_DEFAULT_QUALITY tells WASAPI
    // to resample/convert from the device's native format to ours automatically.
    let fmt = make_float_format(PIPE_SAMPLE_RATE, PIPE_CHANNELS);

    let mut flags =
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK |
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
        AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;

    if is_loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }

    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        flags,
        10_000_000, // 1-second buffer
        0,
        &fmt,
        None,
    )?;

    let event = CreateEventW(None, false, false, None)?;
    client.SetEventHandle(event)?;
    let capture: IAudioCaptureClient = client.GetService()?;

    Ok((client, capture, event))
}

/// Drain all available frames from a WASAPI capture client into a f32 buffer.
/// Since we always use float32 format, no conversion needed.
#[cfg(windows)]
unsafe fn drain_wasapi(
    capture: &windows::Win32::Media::Audio::IAudioCaptureClient,
    out:     &mut Vec<f32>,
) -> bool {
    out.clear();
    loop {
        let pkt = match capture.GetNextPacketSize() {
            Ok(0)  => break,
            Ok(n)  => n,
            Err(_) => return false,
        };
        let mut ptr    = std::ptr::null_mut();
        let mut frames = 0u32;
        let mut flags  = 0u32;
        if capture.GetBuffer(&mut ptr, &mut frames, &mut flags, None, None).is_err() {
            return false;
        }
        if frames > 0 && !ptr.is_null() {
            // Data is always float32 (we requested that format explicitly)
            let n = frames as usize * PIPE_CHANNELS as usize;
            out.extend_from_slice(std::slice::from_raw_parts(ptr as *const f32, n));
        }
        capture.ReleaseBuffer(frames).ok();
    }
    true
}

#[cfg(windows)]
fn capture_loop(
    sys_device_id: Option<&str>,
    mic_device_id: Option<&str>,
    stop:  &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: &std::sync::Arc<std::sync::Mutex<Option<AudioFormat>>>,
) -> Result<()> {
    use std::sync::atomic::Ordering;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::{
        Foundation::CloseHandle,
        Media::Audio::{IMMDeviceEnumerator, MMDeviceEnumerator},
        System::{
            Com::{CoInitializeEx, CoCreateInstance, CLSCTX_ALL, COINIT_MULTITHREADED},
            Pipes::{CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_WAIT},
            Threading::WaitForSingleObject,
        },
        Storage::FileSystem::{WriteFile, FILE_FLAGS_AND_ATTRIBUTES},
    };

    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok(); }

    let enumerator: IMMDeviceEnumerator = unsafe {
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?
    };

    // Open requested devices
    let sys = sys_device_id.and_then(|id| unsafe { open_wasapi(&enumerator, id, true)  }.ok());
    let mic = mic_device_id.and_then(|id| unsafe { open_wasapi(&enumerator, id, false) }.ok());

    if sys.is_none() && mic.is_none() {
        return Err(anyhow::anyhow!("No audio devices available"));
    }

    // The primary timing event — prefer sys audio; fall back to mic
    let primary_event = sys.as_ref().map(|(_, _, ev)| *ev)
        .or_else(|| mic.as_ref().map(|(_, _, ev)| *ev))
        .unwrap();

    // Create named pipe
    let pipe_name: Vec<u16> = OsStr::new(r"\\.\pipe\cliplite_sysaudio")
        .encode_wide().chain([0]).collect();
    let pipe = unsafe {
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

    // Signal ready with our fixed pipe format
    *ready.lock().unwrap() = Some(AudioFormat {
        sample_rate: PIPE_SAMPLE_RATE,
        channels:    PIPE_CHANNELS,
    });

    // Start WASAPI clients BEFORE connecting the pipe so the kernel buffer
    // begins filling immediately. This avoids a silent gap at the start.
    if let Some((ref sc, _, _)) = sys { unsafe { sc.Start().ok(); } }
    if let Some((ref mc, _, _)) = mic { unsafe { mc.Start().ok(); } }

    // Block until FFmpeg opens the pipe as a client.
    unsafe { windows::Win32::System::Pipes::ConnectNamedPipe(pipe, None).ok(); }

    // Pre-roll discard: drain and discard the first 50ms of WASAPI data that
    // accumulated in the kernel buffer while FFmpeg was starting up.
    // This burst is what causes the choppy/accelerated audio at the start of
    // recordings — it arrives all at once faster than real-time.
    {
        let discard_until = std::time::Instant::now() + std::time::Duration::from_millis(50);
        let mut discard_buf: Vec<f32> = Vec::new();
        while std::time::Instant::now() < discard_until {
            if let Some((_, ref sc, _)) = sys { unsafe { drain_wasapi(sc, &mut discard_buf) }; }
            if let Some((_, ref mc, _)) = mic { unsafe { drain_wasapi(mc, &mut discard_buf) }; }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    let mut sys_buf:   Vec<f32> = Vec::new();
    let mut mic_buf:   Vec<f32> = Vec::new();
    // mic_window: sliding buffer that accumulates mic samples between sys wake-ups.
    // Sys audio is the timing master — output is always clocked to sys_buf.len().
    // Mic excess is kept here and consumed first next cycle; mic shortfall is zero-padded.
    let mut mic_window: Vec<f32> = Vec::new();
    let mut mixed:     Vec<f32> = Vec::new();

    'outer: while !stop.load(Ordering::Relaxed) {
        use windows::Win32::Foundation::WAIT_EVENT;
        const WAIT_TIMEOUT: WAIT_EVENT = WAIT_EVENT(0x00000102);

        let wait = unsafe { WaitForSingleObject(primary_event, 100) };
        if wait == WAIT_TIMEOUT { continue; }

        // Drain whatever is available this cycle
        if let Some((_, ref sc, _)) = sys {
            if !unsafe { drain_wasapi(sc, &mut sys_buf) } { break 'outer; }
        }
        if let Some((_, ref mc, _)) = mic {
            unsafe { drain_wasapi(mc, &mut mic_buf) };
            mic_window.extend_from_slice(&mic_buf);
        }

        // Mix or pass through
        let out_slice: &[f32] = match (!sys_buf.is_empty(), !mic_window.is_empty()) {
            (true, true) => {
                // Output exactly as many samples as sys produced this cycle.
                // mic_window supplies the mic signal; any excess carries to next cycle.
                let out_len = sys_buf.len();
                mixed.resize(out_len, 0.0f32);
                for i in 0..out_len {
                    let s = sys_buf[i];
                    let m = mic_window.get(i).copied().unwrap_or(0.0);
                    mixed[i] = (s + m).clamp(-1.0, 1.0);
                }
                // Keep only the excess mic samples for next cycle
                if mic_window.len() > out_len {
                    mic_window.drain(..out_len);
                } else {
                    mic_window.clear();
                }
                &mixed
            }
            (true, false)  => &sys_buf,
            (false, true)  => {
                // Mic only — no sys clock, pass mic_window straight through
                let len = mic_window.len();
                mixed.resize(len, 0.0f32);
                mixed.copy_from_slice(&mic_window);
                mic_window.clear();
                &mixed
            }
            (false, false) => continue,
        };

        let bytes = unsafe {
            std::slice::from_raw_parts(out_slice.as_ptr() as *const u8, out_slice.len() * 4)
        };
        let mut written = 0u32;
        if unsafe { WriteFile(pipe, Some(bytes), Some(&mut written), None) }.is_err() {
            break 'outer;
        }
    }

    // Clean up
    if let Some((ref sc, _, ev)) = sys { unsafe { sc.Stop().ok(); CloseHandle(ev).ok(); } }
    if let Some((ref mc, _, ev)) = mic { unsafe { mc.Stop().ok(); CloseHandle(ev).ok(); } }
    unsafe { CloseHandle(pipe).ok(); }
    Ok(())
}

#[cfg(not(windows))]
pub struct SysAudioCapture;
#[cfg(not(windows))]
impl SysAudioCapture {
    pub fn start(_: Option<String>, _: Option<String>) -> Result<Self> { Ok(SysAudioCapture) }
    pub fn stop(&mut self) {}
}
