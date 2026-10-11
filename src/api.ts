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
  cleanup_level: CleanupLevel;
  cleanup_model: string;
  keep_in_clipboard: boolean;
  history_enabled: boolean;
  history_limit: number;
  save_last_recording: boolean;
  show_overlay: boolean;
  voice_commands: boolean;
  dictionary: Replacement[];
  snippets: Snippet[];
  sounds: boolean;
  monthly_budget_usd: number | null;
  /** Nome do microfone; `null` = o predefinido do sistema. */
  input_device: string | null;
  /** O guia de primeira execução já foi concluído (ou saltado). */
  onboarding_done: boolean;
  /** Estilo por app (config.rs, StyleRule). */
  styles: StyleRule[];
}

/** Quanto a limpeza com IA mexe no texto (config.rs, CleanupLevel). */
export type CleanupLevel = "off" | "light" | "medium" | "high";

/** O tom de um ditado, pela app onde estás (config.rs, Style). "default" = Predefinido. */
export type Style = "default" | "casual" | "formal" | "literal";

/** "Nesta app, este estilo." `app`: nome do executável ("whatsapp.exe"). */
export interface StyleRule {
  app: string;
  style: Style;
}

/** Uma palavra do dicionário. `from`: formas separadas por vírgulas; vazio = a própria palavra. */
export interface Replacement {
  from: string;
  to: string;
}

/** Uma expansão: dizes `trigger` e é colado `text`. */
export interface Snippet {
  trigger: string;
  text: string;
}

export interface SettingsView {
  config: Config;
  defaults: Config;
  api_key_set: boolean;
  /** `false`: outra app ocupou o atalho de ditado e ele não funciona. */
  shortcut_ok: boolean;
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
  /** Linha de um ficheiro transcrito, não de um ditado. */
  file?: boolean;
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
  /** A app onde o ditado começou ("whatsapp.exe"); ausente em entradas antigas. */
  app?: string | null;
  /** O estilo aplicado, só quando mudou alguma coisa. */
  style?: Style | null;
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

/** Um microfone (audio/capture.rs, InputDevice). */
export interface InputDevice {
  name: string;
  is_default: boolean;
}

/** O que o medidor do microfone conta (audio/capture.rs, MicEvent). */
export type MicEvent =
  | { kind: "opened"; value: string }
  | { kind: "level"; value: number }
  | { kind: "failed"; value: string };

/** O que impede o ditado de funcionar (state.rs, Status). */
export interface Status {
  key_missing: boolean;
  shortcut_ok: boolean;
  shortcut: string;
  shortcut_label: string;
}

/** Sítios para onde a janela pode ser levada de fora (settings_window.rs, open_at). */
export type OpenTarget = "key" | "shortcut";

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (config: Config) => invoke<SettingsView>("save_settings", { config }),
  setApiKey: (key: string) => invoke<KeyInfo | null>("set_api_key", { key }),
  deleteApiKey: () => invoke<void>("delete_api_key"),
  /** Valida a chave e devolve o gasto e o limite dela (GET /key, gratuito). */
  testApiKey: () => invoke<KeyInfo>("test_api_key"),
  listModels: (kind: ModelKind, refresh = false) => invoke<ModelList>("list_models", { kind, refresh }),
  getStats: () => invoke<Stats>("get_stats"),
  getHistory: () => invoke<HistoryEntry[]>("get_history"),
  clearHistory: () => invoke<void>("clear_history"),
  /** As últimas apps onde ditaste nesta sessão (mais recente primeiro). */
  recentApps: () => invoke<string[]>("recent_apps"),
  copyText: (text: string) => invoke<void>("copy_text", { text }),
  setAutostart: (enabled: boolean) => invoke<boolean>("set_autostart", { enabled }),
  getMemory: () => invoke<MemoryReport | null>("get_memory"),
  checkUpdate: () => invoke<UpdateInfo | null>("check_update"),
  installUpdate: () => invoke<void>("install_update"),
  listInputDevices: () => invoke<InputDevice[]>("list_input_devices"),
  /** Liga (ou renova) o medidor do microfone; desliga-se sozinho sem renovação. */
  micMonitor: (on: boolean) => invoke<void>("mic_monitor", { on }),
  openOpenRouterKeys: () => invoke<void>("open_openrouter_keys"),
  /** Para onde a janela foi chamada (ex.: clique na pílula); uma só vez. */
  takeOpen: () => invoke<OpenTarget | null>("main_take_open"),
  // Barra flutuante (overlay.ts)
  overlayClick: () => invoke<void>("overlay_click"),
  overlayCancel: () => invoke<void>("overlay_cancel"),
  overlayHitRect: (rect: { x: number; y: number; width: number; height: number }) =>
    invoke<void>("overlay_hit_rect", { rect }),
  overlayStatus: () => invoke<Status>("overlay_status"),
  overlayOpen: (target: OpenTarget) => invoke<void>("overlay_open", { target }),
  overlayMenu: () => invoke<void>("overlay_menu"),
};

/// Emitido pelo backend depois de cada ditado (pipeline.rs, EVENT_DICTATION_RECORDED).
export function onDictationRecorded(handler: () => void): Promise<UnlistenFn> {
  return listen("dictation-recorded", handler);
}

/// As definições mudaram fora da janela (ex.: "Silenciar sons" no tray; state.rs).
export function onSettingsChanged(handler: () => void): Promise<UnlistenFn> {
  return listen("settings-changed", handler);
}

/// Alguém (a pílula) pediu para a janela ir a um sítio; vai buscá-lo com `api.takeOpen`.
export function onOpenRequest(handler: () => void): Promise<UnlistenFn> {
  return listen("main:open", handler);
}

/// Medidor do microfone fora de um ditado (lib.rs, "mic:level").
export function onMic(handler: (e: MicEvent) => void): Promise<UnlistenFn> {
  return listen<MicEvent>("mic:level", (e) => handler(e.payload));
}

/// Os comandos rejeitam com o texto do erro (CommandError em commands.rs).
export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}

// ---------------------------------------------------------------- ficheiros (files.rs)

export interface FileProbe {
  name: string;
  path: string;
  duration_s: number | null;
  size_bytes: number;
}

export type FileStatus = "running" | "done" | "failed" | "cancelled";

export interface FileJob {
  name: string;
  status: FileStatus;
  model: string;
  total_s: number | null;
  done_s: number;
  cost_usd: number;
  elapsed_s: number;
  text: string | null;
  message: string | null;
  saved_to: string | null;
  failed_chunks: number;
}

export const files = {
  probe: (path: string) => invoke<FileProbe>("file_probe", { path }),
  pick: () => invoke<string | null>("file_pick"),
  start: (path: string) => invoke<FileJob>("file_start", { path }),
  cancel: () => invoke<void>("file_cancel"),
  status: () => invoke<FileJob | null>("file_status"),
  clear: () => invoke<void>("file_clear"),
  saveBeside: () => invoke<string>("file_save_beside"),
};

/// Emitido pelo backend a cada pedaço transcrito e no fim (files.rs, EVENT).
export function onFileJob(handler: (job: FileJob) => void): Promise<UnlistenFn> {
  return listen<FileJob>("file:job", (e) => handler(e.payload));
}
