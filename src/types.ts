export interface Region {
  x: number;
  y: number;
  width: number;
  height: number;
  monitor: number;
}

export interface AudioConfig {
  mic_device:    string | null;  // legacy dshow name
  sys_device_id: string | null;  // WASAPI endpoint ID for speaker loopback
  mic_device_id: string | null;  // WASAPI endpoint ID for mic (no dshow)
}

export interface EncConfig {
  fps: number;
  quality_crf: number;
  format: string;
  hw_encoder: string;
}

export interface AudioDevice {
  id: string;
  name: string;
  kind: "input" | "output" | "loopback";
}

export interface MonitorInfo {
  index: number;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  is_primary: boolean;
  refresh_rate: number;
}

export interface Clip {
  id: number;
  filename: string;
  filepath: string;
  duration_s: number;
  width: number;
  height: number;
  fps: number;
  filesize_b: number;
  format: string;
  created_at: string;
  tags: string;
  thumbnail: string;
}

export interface RecordingStatus {
  is_recording: boolean;
  is_paused: boolean;
  elapsed_seconds: number;
  output_path: string | null;
  replay_active: boolean;
}

export interface Settings {
  output_dir: string;
  default_format: string;
  default_fps: number;
  default_quality_crf: number;
  mic_device: string | null;
  mic_device_id: string | null;        // WASAPI endpoint ID for mic
  sys_audio_device: string | null;
  sys_audio_device_id: string | null;  // WASAPI endpoint ID for speaker loopback
  capture_cursor: boolean;
  show_keystroke_hud: boolean;
  hotkeys: Record<string, string>;
  hw_encoder: string;
  minimize_to_tray: boolean;
  filename_template: string;
  replay_buffer_duration_secs: number;  // 0 = disabled
  replay_output_dir: string;            // empty = same as output_dir
  replay_filename_template: string;     // e.g. "replay_{datetime}"
  selected_monitor: number;             // last-used monitor index (0 = primary)
  max_replay_buffer_size_mb: number;    // max temp buffer size in MB (0 = unlimited)
}

export type View = "recorder" | "library" | "settings";

export const QUALITY_PRESETS = [
  { label: "Low",       crf: 35 },
  { label: "Medium",    crf: 30 },
  { label: "High",      crf: 28 },
  { label: "Very High", crf: 23 },
  { label: "Lossless",  crf: 18 },
] as const;

export const FPS_OPTIONS = [15, 24, 30, 60] as const;
export const FORMAT_OPTIONS = ["mp4", "webm", "gif"] as const;
