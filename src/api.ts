/**
 * Tauri command wrappers — typed bridge to the Rust backend.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  AudioConfig, AudioDevice, Clip, EncConfig,
  MonitorInfo, RecordingStatus, Region, Settings,
} from "./types";

// FFmpeg
export const findFFmpeg = () => invoke<string>("find_ffmpeg");
export const detectHwEncoder = () => invoke<string>("detect_hw_encoder");

// Recording
export const startRecording = (
  region: Region,
  audio_cfg: AudioConfig,
  enc_cfg: EncConfig,
) => invoke<string>("start_recording", { region, audioCfg: audio_cfg, encCfg: enc_cfg });

export const stopRecording = () => invoke<string>("stop_recording");
export const pauseRecording = () => invoke<void>("pause_recording");
export const resumeRecording = () => invoke<void>("resume_recording");
export const getRecordingStatus = () => invoke<RecordingStatus>("get_recording_status");

// Replay buffer
export const startReplay = (region: Region, audio_cfg: AudioConfig, enc_cfg: EncConfig) =>
  invoke<string>("start_replay", { region, audioCfg: audio_cfg, encCfg: enc_cfg });
export const stopReplay  = () => invoke<void>("stop_replay");
export const saveReplay  = (secs: number) => invoke<string>("save_replay", { secs });

// Audio
export const listAudioDevices = () => invoke<AudioDevice[]>("list_audio_devices");
export const listSystemAudioDevices = () => invoke<AudioDevice[]>("list_system_audio_devices");

// Library
export const getClips = (search = "") => invoke<Clip[]>("get_clips", { search });
export const deleteClip = (clip_id: number, delete_file = true) =>
  invoke<void>("delete_clip", { clipId: clip_id, deleteFile: delete_file });
export const updateClipTags = (clip_id: number, tags: string) =>
  invoke<void>("update_clip_tags", { clipId: clip_id, tags });
export const addClip = (filepath: string) => invoke<Clip>("add_clip", { filepath });

// Settings
export const getSettings = () => invoke<Settings>("get_settings");
export const saveSettings = (settings: Settings) => invoke<void>("save_settings", { settings });
export const applyHotkeys = () => invoke<void>("apply_hotkeys");

// Utility
export const getMonitors = () => invoke<MonitorInfo[]>("get_monitors");
export const openPath = (path: string) => invoke<void>("open_path", { path });
export const trimClip = (
  input_path: string,
  output_path: string,
  start_time: number,
  end_time: number,
) => invoke<void>("trim_clip", { inputPath: input_path, outputPath: output_path, startTime: start_time, endTime: end_time });
