// Ponte tipada para o backend. Cada função corresponde a um #[tauri::command] em
// src-tauri/src/commands.rs e cada interface a um struct Rust com #[derive(Serialize)].
// Se mudares um struct no Rust, muda também aqui.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Transport = "multipart" | "json";
export type ModelKind = "transcription" | "text";

export interface Config {
  shortcut: string;
  stt_model: string;
  transport: Transport;
  language: string | null;
  cleanup_enabled: boolean;
  cleanup_model: string;
  keep_in_clipboard: boolean;
  history_enabled: boolean;
  history_limit: number;
  save_last_recording: boolean;
  show_overlay: boolean;
}

export interface SettingsView {
  config: Config;
  defaults: Config;
  api_key_set: boolean;
  autostart: boolean;
  paths: { config: string; data: string; logs: string };
}

export interface KeyInfo {
  label: string | null;
  usage: number | null;
  limit: number | null;
  limit_remaining: number | null;
  is_free_tier: boolean | null;
}

export interface ModelInfo {
  id: string;
  name: string;
  prompt_price: number | null;
  completion_price: number | null;
}

export interface ModelList {
  fetched_at: string;
  models: ModelInfo[];
  stale: boolean;
}

export interface Percentiles {
  count: number;
  p50: number;
  p90: number;
  max: number;
}

export interface MetricsRow {
  at: string;
  model: string;
  audio_ms: number;
  stt_ms: number | null;
  cleanup_ms: number | null;
  paste_ms: number | null;
  total_ms: number | null;
  cost_usd: number | null;
  cleanup_cost_usd: number | null;
  error: string | null;
}

export interface Stats {
  current_month: string;
  months: Record<string, { cost_usd: number; dictations: number }>;
  total: Percentiles | null;
  stt: Percentiles | null;
  cleanup: Percentiles | null;
  recent: MetricsRow[];
}

export interface HistoryEntry {
  at: string;
  text: string;
  raw_text: string | null;
  model: string;
  audio_ms: number;
  total_ms: number | null;
}

export interface MemoryReport {
  app_mb: number;
  webview_mb: number;
  webview_processes: number;
}

export interface UpdateInfo {
  current: string;
  version: string;
  notes: string | null;
}

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (config: Config) => invoke<SettingsView>("save_settings", { config }),
  setApiKey: (key: string) => invoke<KeyInfo | null>("set_api_key", { key }),
  deleteApiKey: () => invoke<void>("delete_api_key"),
  testApiKey: () => invoke<KeyInfo>("test_api_key"),
  listModels: (kind: ModelKind, refresh = false) => invoke<ModelList>("list_models", { kind, refresh }),
  getStats: () => invoke<Stats>("get_stats"),
  getHistory: () => invoke<HistoryEntry[]>("get_history"),
  clearHistory: () => invoke<void>("clear_history"),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  getMemory: () => invoke<MemoryReport | null>("get_memory"),
  checkUpdate: () => invoke<UpdateInfo | null>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
};

/// Emitido pelo backend depois de cada ditado (pipeline.rs, EVENT_DICTATION_RECORDED).
export function onDictationRecorded(handler: () => void): Promise<UnlistenFn> {
  return listen("dictation-recorded", handler);
}

/// Os comandos rejeitam com o texto do erro (CommandError em commands.rs).
export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
