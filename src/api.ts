// Ponte tipada para o backend. Cada função corresponde a um #[tauri::command] em
// src-tauri/src/commands.rs e cada interface a um struct Rust com #[derive(Serialize)].
// Se mudares um struct no Rust, muda também aqui.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Config {
  shortcut: string;
  /** Modo comando: um segundo atalho que aplica uma instrução falada ao texto selecionado. */
  command_enabled: boolean;
  command_shortcut: string;
  /** Onde aparece a resposta do modo comando: na janela do assistente ou colada no texto. */
  command_output: CommandOutput;
  /** Perguntar sobre o ecrã: um atalho fotografa o ecrã, escolhes a área e perguntas (src/screen.ts). */
  screen_enabled: boolean;
  screen_shortcut: string;
  /** Modelo que vê as capturas do ecrã; vazio = o da limpeza. */
  vision_model: string;
  language: string | null;
  cleanup_level: CleanupLevel;
  /** O modelo Grok da limpeza e dos comandos. */
  cleanup_model: string;
  keep_in_clipboard: boolean;
  history_enabled: boolean;
  history_limit: number;
  /** Guarda o áudio dos últimos 20 ditados para os poder reprocessar (history.rs, AUDIO_KEEP). */
  keep_audio: boolean;
  save_last_recording: boolean;
  show_overlay: boolean;
  voice_commands: boolean;
  spoken_punctuation: boolean;
  /** Atalho do histórico rápido (menu com os últimos ditados); vazio = desligado. */
  quick_history_shortcut: string;
  /** Palavras por minuto que escreves à mão (só para estimar o tempo poupado). */
  typing_wpm: number;
  /** Modelo de texto dos resumos; vazio = o da limpeza. */
  summary_model: string;
  notes_consent: boolean;
  /** Separar quem fala em "Outros" no fim da reunião, com a xAI. */
  notes_speakers: boolean;
  dictionary: Replacement[];
  snippets: Snippet[];
  sounds: boolean;
  monthly_budget_usd: number | null;
  /** Nome do microfone; `null` = o predefinido do sistema. */
  input_device: string | null;
  /** Saída de áudio que se capta nas notas; `null` = a predefinida do sistema. */
  notes_output_device: string | null;
  /** O guia de primeira execução já foi concluído (ou saltado). */
  onboarding_done: boolean;
  /** Estilo por app (config.rs, StyleRule). */
  styles: StyleRule[];
}

/** Quanto a limpeza com IA mexe no texto (config.rs, CleanupLevel). */
export type CleanupLevel = "off" | "light" | "medium" | "high";

/** Para onde vai a resposta do modo comando (config.rs, CommandOutput). */
export type CommandOutput = "window" | "paste";

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

export type DictFormat = "json" | "csv" | "txt";

/** Uma palavra ou expansão que o ficheiro e a tua lista dizem de maneiras diferentes. */
export interface DictConflict {
  list: "dictionary" | "snippet";
  key: string;
  mine: string;
  theirs: string;
}

/** As tuas listas depois de juntar um ficheiro, e o que mudou. */
export interface DictMerged {
  dictionary: Replacement[];
  snippets: Snippet[];
  new: number;
  duplicate: number;
  replaced: number;
  invalid: number;
  conflicts: DictConflict[];
}

/** O que um ficheiro faria, em cada modo de resolver conflitos. Nada está gravado ainda. */
export interface ImportPreview {
  file_name: string;
  format: DictFormat;
  invalid: number;
  keep: DictMerged;
  replace: DictMerged;
}

/** Uma célula do mapa de calor (api.ts espelha `usage::HeatCell`). */
export interface HeatCell {
  date: string;
  words: number;
  /** 0 = nada; 1 a 4 = de menos a mais palavras. */
  level: number;
  /** Um dia que ainda não chegou. */
  future: boolean;
}

export interface UsageSummary {
  total_words: number;
  total_dictations: number;
  month_words: number;
  streak_current: number;
  streak_best: number;
  wpm: number | null;
  saved_minutes: number;
  typing_wpm: number;
  top_apps: { app: string; words: number }[];
  /** 53 semanas × 7 dias, semana a semana (segunda a domingo). */
  heat: HeatCell[];
  estimated: boolean;
  first_day: string | null;
}

export interface SettingsView {
  config: Config;
  defaults: Config;
  /** Há chave da xAI no cofre? */
  api_key_set: boolean;
  /** `false`: outra app ocupou o atalho de ditado e ele não funciona. */
  shortcut_ok: boolean;
  /** `false`: o modo comando está ligado mas outra app ocupou o atalho. */
  command_ok: boolean;
  /** `false`: o atalho do histórico rápido está ocupado por outra app. */
  quick_ok: boolean;
  /** `false`: perguntar sobre o ecrã está ligado mas outra app ocupou o atalho. */
  screen_ok: boolean;
  autostart: boolean;
  paths: { config: string; data: string; logs: string };
}

/** O que uma chave da xAI deixa fazer (xai/mod.rs, Access). `null` = funciona; texto = o motivo. */
export interface Access {
  transcribe: string | null;
  text: string | null;
}

export interface ModelInfo {
  id: string;
  /** USD por token de entrada. */
  prompt_price: number | null;
  /** USD por token de saída. */
  completion_price: number | null;
  /** Raciocina antes de responder (mais lento e mais caro). */
  reasoning: boolean;
  /** Também lê imagens: serve para as capturas do ecrã. */
  vision: boolean;
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
  /** Modo comando: `cleanup_*` são o pedido ao LLM com a instrução. */
  command?: boolean;
  /** Um resumo (summarize.rs): só interessa o custo. */
  summary?: boolean;
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
  /** Identifica a entrada (apagar, reprocessar); só letras, algarismos, "-" e "_". */
  id: string;
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
  /** Modo comando: a instrução falada. Aí `raw_text` é o texto selecionado que foi substituído. */
  instruction?: string | null;
  /** O texto foi corrigido à mão (history.rs, `corrected`); ausente quando não. */
  corrected?: boolean;
}

/** Uma entrada como `getHistory` a devolve (history.rs, HistoryItem). */
export interface HistoryItem extends HistoryEntry {
  /** Há áudio guardado: pode ser reprocessada. */
  has_audio: boolean;
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
  /** O atalho do modo comando, legível; `null` com o modo desligado. */
  command_label: string | null;
  /** `false` só com o modo comando ligado e o atalho ocupado por outra app. */
  command_ok: boolean;
  /** O atalho de perguntar sobre o ecrã, legível; `null` com a opção desligada. */
  screen_label: string | null;
  screen_ok: boolean;
}

/** Sítios para onde a janela pode ser levada de fora (settings_window.rs, open_at). */
export type OpenTarget = "key" | "shortcut";

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (config: Config) => invoke<SettingsView>("save_settings", { config }),
  /** Valida a chave da xAI, guarda-a no cofre e diz o que ela deixa fazer. */
  setApiKey: (key: string) => invoke<Access>("set_api_key", { key }),
  deleteApiKey: () => invoke<void>("delete_api_key"),
  /** Confirma que a chave guardada continua válida e o que deixa fazer (custa uma fração de cêntimo). */
  testApiKey: () => invoke<Access>("test_api_key"),
  /** Os modelos de texto da xAI (com cache de 24 h). */
  listModels: (refresh = false) => invoke<ModelList>("list_models", { refresh }),
  getStats: () => invoke<Stats>("get_stats"),
  getHistory: () => invoke<HistoryItem[]>("get_history"),
  clearHistory: () => invoke<void>("clear_history"),
  /** Apaga uma entrada e devolve-a (para o "Desfazer"); `null` se já não existia. */
  deleteHistoryEntry: (id: string) => invoke<HistoryEntry | null>("delete_history_entry", { id }),
  restoreHistoryEntry: (entry: HistoryEntry) => invoke<void>("restore_history_entry", { entry }),
  /** Volta a transcrever e a limpar o áudio guardado; devolve a entrada com o texto novo. */
  reprocessHistoryEntry: (id: string) => invoke<HistoryEntry>("reprocess_history_entry", { id }),
  correctHistoryEntry: (id: string, text: string) => invoke<HistoryEntry>("correct_history_entry", { id, text }),
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
  /** Abre a consola da xAI (onde se criam as chaves) no browser. */
  openXaiConsole: () => invoke<void>("open_xai_console"),
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
  dictionaryExport: (format: DictFormat, withSnippets: boolean) =>
    invoke<string | null>("dictionary_export", { format, withSnippets }),
  dictionaryImportPreview: () => invoke<ImportPreview | null>("dictionary_import_preview"),
  usageSummary: () => invoke<UsageSummary>("usage_summary"),
  usageClear: () => invoke<void>("usage_clear"),
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

/** O ficheiro inteiro a subir, a xAI a transcrevê-lo, ou o caminho por pedaços. */
export type FileStage = "upload" | "server" | "chunks";

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
  stage: FileStage;
  /** Bytes enviados e tamanho do ficheiro (só contam em "upload"). */
  sent_bytes: number;
  total_bytes: number;
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

// ---------------------------------------------------------------- resumos

/** O que se quer do resumo (summarize.rs, Kind). */
export type SummaryKind = "short" | "keypoints" | "meeting" | "lesson";

/** Um resumo pronto (commands.rs, SummaryOut). */
export interface SummaryOut {
  text: string;
  model: string;
  cost_usd: number | null;
  seconds: number;
}

export const summaries = {
  /** O texto vai à xAI; o custo entra nas métricas. */
  run: (text: string, kind: SummaryKind) => invoke<SummaryOut>("summarize_text", { text, kind }),
  /** Guarda o resumo (Markdown) ao lado do ficheiro transcrito. */
  saveBesideFile: (text: string) => invoke<string>("file_save_summary", { text }),
};

// ---------------------------------------------------------------- notas de reuniões (notes/)

export type NotesMode = "meeting" | "system-only" | "in-person";
export type NotesTrack = "mic" | "system";
export type NotesPhase = "idle" | "recording" | "finishing";

/** Estado da gravação em curso (notes/session.rs, StateView). */
export interface NotesState {
  phase: NotesPhase;
  id: string | null;
  title: string | null;
  mode: NotesMode | null;
  /** Milissegundos desde 1970 (para contar o tempo). */
  started_ms: number | null;
  cost_usd: number;
  lines: number;
  system_mute: boolean;
  mic_device: string | null;
  system_device: string | null;
  lost: string[];
  error: string | null;
  /** Onde se dá a permissão do áudio do sistema neste sistema operativo. */
  /** O que se faz ao fechar ("A identificar quem fala…"). */
  step: string | null;
  system_help: string;
}

export interface NotesMeta {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  mode: NotesMode;
  model: string;
  cost_usd: number;
  audio_ms: number;
  interrupted: boolean;
  finished: boolean;
  mic_name: string;
  system_name: string;
  /** Nomes dados aos oradores separados, por número. */
  speaker_names: Record<string, string>;
}

export interface NotesLine {
  track: NotesTrack;
  start_ms: number;
  end_ms: number;
  text: string;
  failed: boolean;
  /** Orador (1, 2…) dentro da faixa, se foram separados. */
  speaker?: number;
}

export interface NotesOut {
  meta: NotesMeta;
  lines: NotesLine[];
  summary: string | null;
}

export const notes = {
  state: () => invoke<NotesState>("notes_state"),
  outputDevices: () => invoke<InputDevice[]>("notes_output_devices"),
  testStart: (seconds: number) => invoke<void>("notes_test_start", { seconds }),
  acceptConsent: () => invoke<void>("notes_accept_consent"),
  start: (mode: NotesMode) => invoke<NotesState>("notes_start", { mode }),
  stop: () => invoke<NotesState>("notes_stop"),
  list: () => invoke<NotesMeta[]>("notes_list"),
  get: (id: string) => invoke<NotesOut>("notes_get", { id }),
  remove: (id: string) => invoke<void>("notes_delete", { id }),
  rename: (id: string, patch: { title?: string; micName?: string; systemName?: string }) =>
    invoke<NotesMeta>("notes_rename", {
      id,
      title: patch.title ?? null,
      micName: patch.micName ?? null,
      systemName: patch.systemName ?? null,
    }),
  text: (id: string) => invoke<string>("notes_text", { id }),
  saveSummary: (id: string, summary: string) => invoke<void>("notes_save_summary", { id, summary }),
  /** Pergunta onde guardar; `null` se cancelaste. */
  exportTo: (id: string, markdown: boolean) => invoke<string | null>("notes_export", { id, markdown }),
};

export function onNotesState(handler: (s: NotesState) => void): Promise<UnlistenFn> {
  return listen<NotesState>("notes:state", (e) => handler(e.payload));
}

export function onNotesLine(handler: (e: { id: string; line: NotesLine; cost_usd: number }) => void): Promise<UnlistenFn> {
  return listen<{ id: string; line: NotesLine; cost_usd: number }>("notes:line", (e) => handler(e.payload));
}

export function onNotesLevel(handler: (e: { track: NotesTrack; level: number }) => void): Promise<UnlistenFn> {
  return listen<{ track: NotesTrack; level: number }>("notes:level", (e) => handler(e.payload));
}

/** Eventos do teste de áudio (notes/check.rs, TestEvent). */
export type NotesTestEvent =
  | { kind: "level"; track: NotesTrack; level: number }
  | { kind: "device"; track: NotesTrack; name: string }
  | { kind: "lost"; track: NotesTrack; why: string }
  | { kind: "failed"; why: string }
  | { kind: "done"; mic_peak: number; system_peak: number };

export function onNotesTest(handler: (e: NotesTestEvent) => void): Promise<UnlistenFn> {
  return listen<NotesTestEvent>("notes:test", (e) => handler(e.payload));
}

// ---------------------------------------------------------------- oradores (notes/speakers.rs)

export const speakers = {
  /** Dá um nome a um orador separado; vazio volta a "Orador N". */
  rename: (id: string, speaker: number, name: string) =>
    invoke<NotesMeta>("notes_rename_speaker", { id, speaker, name }),
};

// ---------------------------------------------------------------- assistente (assistant/)

export type ChatRole = "user" | "assistant";
/** Como acabou uma resposta; ausente = acabou bem (assistant/store.rs, Status). */
export type ChatStatus = "done" | "stopped" | "failed";

export interface ChatMessage {
  id: string;
  role: ChatRole;
  text: string;
  at: string;
  model?: string;
  cost_usd?: number;
  status?: ChatStatus;
  /** Com `status: "failed"`: a frase que explica a falha. */
  error?: string;
  /** O pedido foi dito em voz alta. */
  spoken?: boolean;
}

export interface ChatConversation {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  /** O texto que estava selecionado quando a conversa começou. */
  selection?: string;
  /** A app onde estavas ("outlook.exe"). */
  app?: string;
  /** A conversa é sobre uma captura do ecrã (a imagem não fica guardada em disco). */
  capture?: { width: number; height: number };
  messages: ChatMessage[];
}

/** O que a janela do assistente mostra (assistant/mod.rs, View). */
export interface AssistantView {
  conversation: ChatConversation | null;
  /** Há uma resposta a chegar. */
  busy: boolean;
  /** Dá para devolver a resposta à app de onde veio o comando (só nesta sessão). */
  can_apply: boolean;
  /** As conversas ficam guardadas (o histórico está ligado). */
  saved: boolean;
  /** O atalho do modo comando, no formato do config; `null` com o modo desligado. */
  command_shortcut: string | null;
  /** O atalho de perguntar sobre o ecrã, no formato do config; `null` com a opção desligada. */
  screen_shortcut: string | null;
  key_missing: boolean;
}

/** Uma linha da lista de conversas (assistant/store.rs, Summary). */
export interface ChatSummary {
  id: string;
  title: string;
  updated_at: string;
  messages: number;
  preview: string;
}

/** Um bocado de uma resposta a chegar: o texto todo até agora (assistant/mod.rs, StreamEvent). */
export interface ChatStreamEvent {
  conversation_id: string;
  message_id: string;
  text: string;
}

export const assistant = {
  state: () => invoke<AssistantView>("assistant_state"),
  /** Escreve na conversa aberta (ou numa nova, se não houver nenhuma). */
  send: (text: string) => invoke<void>("assistant_send", { text }),
  /** Pára a resposta a chegar; o texto que já veio fica. */
  stop: () => invoke<void>("assistant_stop"),
  /** Gera outra vez a última resposta (ou a que falhou). */
  retry: () => invoke<void>("assistant_retry"),
  newChat: () => invoke<void>("assistant_new"),
  open: (id: string) => invoke<void>("assistant_open", { id }),
  list: (query: string) => invoke<ChatSummary[]>("assistant_list", { query }),
  remove: (id: string) => invoke<void>("assistant_delete", { id }),
  clear: () => invoke<void>("assistant_clear"),
  /** Copia a resposta (sem as etiquetas e a cerca de código que o modelo ponha à volta). */
  copy: (messageId: string) => invoke<void>("assistant_copy", { messageId }),
  /** Volta à app de onde veio o comando e cola lá a resposta. `pasted: false`: não deu para
   *  voltar a ela e o texto ficou no clipboard. */
  apply: (messageId: string) => invoke<{ pasted: boolean }>("assistant_apply", { messageId }),
  close: () => invoke<void>("assistant_close"),
  /** A miniatura da captura de uma conversa (`data:image/jpeg;base64,…`); `null` se já não existe
   *  (a imagem só vive na memória, nunca no histórico). */
  capture: (id: string) => invoke<string | null>("assistant_capture", { id }),
};

/** A conversa mudou (nova mensagem, resposta acabada, outra conversa aberta…). */
export function onAssistantState(handler: (view: AssistantView) => void): Promise<UnlistenFn> {
  return listen<AssistantView>("assistant:state", (e) => handler(e.payload));
}

/** A resposta em curso ganhou texto (no máximo ~10 vezes por segundo). */
export function onAssistantStream(handler: (e: ChatStreamEvent) => void): Promise<UnlistenFn> {
  return listen<ChatStreamEvent>("assistant:stream", (e) => handler(e.payload));
}

// ---------------------------------------------------------------- perguntar sobre o ecrã (screen/)

/** O que a janela de seleção precisa para se desenhar (screen/mod.rs, View). */
export interface ScreenView {
  /** Tamanho, em píxeis, da fotografia. */
  width: number;
  height: number;
  /** O atalho de ditar, legível ("Ctrl+Win"). */
  dictate: string;
}

/** A área escolhida, em píxeis CSS da janela (screen/shot.rs, Area). */
export interface ScreenArea {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Uma resposta binária do backend. Pelo canal rápido do Tauri chega como `ArrayBuffer`; quando o
 *  WebView2 cai para o canal de reserva (postMessage) chega como uma lista de números, e um
 *  `Blob` feito dessa lista seria o texto "255,216,…", não a imagem. */
function toBytes(value: unknown): Uint8Array {
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  if (Array.isArray(value)) return Uint8Array.from(value as number[]);
  throw new Error(`resposta binária inesperada (${Object.prototype.toString.call(value)})`);
}

export const screen = {
  /** `null`: a captura já não existe. */
  view: () => invoke<ScreenView | null>("screen_view"),
  /** A fotografia do ecrã, em JPEG. */
  image: async () => toBytes(await invoke<unknown>("screen_image")),
  /** Já desenhei a fotografia: mostra a janela. */
  ready: () => invoke<void>("screen_ready"),
  /** Envia a área com a pergunta (vazia = "explica o que se vê"). `viewport`: o tamanho visível da
   *  página, com que o backend converte a área em píxeis da fotografia. */
  submit: (prompt: string, area: ScreenArea, viewport: { width: number; height: number }, spoken: boolean) =>
    invoke<void>("screen_submit", { prompt, area, viewport, spoken }),
  cancel: () => invoke<void>("screen_cancel"),
  /** Conta ao ficheiro de registo o que correu mal na página. */
  report: (message: string) => invoke<void>("screen_report", { message }),
};

/// O que o microfone já ouviu, enquanto se fala (pipeline: forward_partials).
export function onScreenPartial(handler: (text: string) => void): Promise<UnlistenFn> {
  return listen<string>("screen:partial", (e) => handler(e.payload));
}

/// O texto final ditado, para acrescentar à caixa de pergunta (pipeline::screen_prompt).
export function onScreenTranscript(handler: (text: string) => void): Promise<UnlistenFn> {
  return listen<string>("screen:transcript", (e) => handler(e.payload));
}

/// O estado do microfone, o mesmo da pílula: idle | recording | processing | done | copied | failed | nothing-heard.
export type MicState = "idle" | "recording" | "processing" | "done" | "copied" | "failed" | "nothing-heard";

export function onMicState(handler: (state: MicState) => void): Promise<UnlistenFn> {
  return listen<MicState>("overlay:state", (e) => handler(e.payload));
}

/// O nível do microfone entre 0 e 1, ~20 vezes por segundo.
export function onMicLevel(handler: (level: number) => void): Promise<UnlistenFn> {
  return listen<number>("overlay:level", (e) => handler(e.payload));
}

/// O motivo curto de uma falha do microfone ou da transcrição ("Sem rede"…).
export function onMicFailure(handler: (label: string) => void): Promise<UnlistenFn> {
  return listen<{ label: string; retry: boolean }>("overlay:failure", (e) => handler(e.payload.label));
}
