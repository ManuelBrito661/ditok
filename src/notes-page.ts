// Página Notas: gravar uma reunião (microfone + áudio do computador), ver a
// transcrição ao vivo e guardar, resumir e exportar as sessões. Quem trabalha é o
// backend (src-tauri/src/notes/): a gravação continua com a janela fechada, e ao
// reabri-la `notes.state()` devolve o ponto em que está.

import {
  errorText,
  notes,
  onNotesLevel,
  onNotesLine,
  onNotesState,
  onNotesTest,
  speakers,
  summaries,
  type InputDevice,
  type NotesTestEvent,
  type NotesLine,
  type NotesMeta,
  type NotesMode,
  type NotesOut,
  type NotesState,
  type SummaryKind,
} from "./api";
import { duration, usd } from "./format";
import { $, app, confirmDanger, onViewChange, save, toast } from "./store";
import { describeResult, describeText, SUMMARY_KINDS } from "./summary";

let state: NotesState | null = null;
/** Linhas da gravação em curso, para a vista ao vivo. */
let live: NotesLine[] = [];
let liveId: string | null = null;
let detail: NotesOut | null = null;
let timer: number | undefined;

const MODE_TITLE: Record<NotesMode, string> = {
  meeting: "Reunião",
  "system-only": "Áudio do computador",
  "in-person": "Conversa",
};

function stamp(ms: number): string {
  const s = Math.floor(ms / 1000);
  const h = Math.floor(s / 3600);
  const mm = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return h ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}

/** Uma fala por parágrafo, com a hora e o nome. Texto sempre por `textContent`. */
type Names = { mic: string; system: string; speakers?: Record<string, string> };

const namesOf = (m: NotesMeta): Names => ({ mic: m.mic_name, system: m.system_name, speakers: m.speaker_names });

function whoIs(l: NotesLine, names: Names): string {
  if (l.speaker != null) return names.speakers?.[String(l.speaker)] ?? `Orador ${l.speaker}`;
  return l.track === "mic" ? names.mic : names.system;
}

function renderLines(box: HTMLElement, lines: NotesLine[], names: Names) {
  box.replaceChildren(
    ...lines.map((l) => {
      const p = el("p", `note-line ${l.track}${l.failed ? " failed" : ""}`);
      p.append(
        el("span", "note-who", whoIs(l, names)),
        el("time", "note-time", stamp(l.start_ms)),
        document.createTextNode(l.failed ? "[trecho não transcrito]" : l.text),
      );
      return p;
    }),
  );
}

// ---------------------------------------------------------------- estado e gravação ao vivo

function tick() {
  if (!state?.started_ms) return;
  $("notes-live-time").textContent = duration((Date.now() - state.started_ms) / 1000);
}

function renderState(s: NotesState) {
  const before = state?.phase;
  state = s;
  const active = s.phase !== "idle";
  $("notes-idle").hidden = active;
  $("notes-live").hidden = !active;
  $("nav-notes-rec").hidden = s.phase !== "recording";
  window.clearInterval(timer);

  if (!active) {
    // Acabou de fechar: volta à lista. Senão, deixa a vista de detalhe como está.
    if (before && before !== "idle") {
      detail = null;
      void refreshList();
    }
    $("notes-detail").hidden = detail == null;
    $("notes-list-box").hidden = detail != null;
    // Com uma gravação aberta, o "Começar a gravar" não faz companhia à leitura.
    $("notes-idle").hidden = detail != null;
    return;
  }
  detail = null;
  $("notes-detail").hidden = true;
  $("notes-list-box").hidden = true;
  if (liveId !== s.id) {
    liveId = s.id;
    live = [];
  }
  const finishing = s.phase === "finishing";
  $("notes-live-title").textContent = finishing ? (s.step ?? "A acabar de transcrever…") : (s.title ?? "A gravar");
  $<HTMLButtonElement>("notes-stop").hidden = finishing;
  if (!finishing) {
    tick();
    timer = window.setInterval(tick, 1000);
  }
  const parts = [
    s.mic_device ? `Microfone: ${s.mic_device}` : null,
    s.system_device ? `Sistema: ${s.system_device}` : null,
    s.cost_usd > 0 ? `${usd(s.cost_usd, 3)} até agora` : null,
  ].filter(Boolean);
  $("notes-live-detail").textContent = parts.join(" · ");

  const warnings: string[] = [];
  if (s.system_mute) {
    warnings.push(`O computador não está a enviar som (só silêncio). ${s.system_help}`);
  }
  for (const t of s.lost) warnings.push(`Perdi a faixa ${t === "mic" ? "do microfone" : "do sistema"}; a tentar reabrir.`);
  if (s.error) warnings.push(`Não consigo transcrever: ${s.error}`);
  const warn = $("notes-live-warning");
  warn.textContent = warnings.join(" ");
  warn.hidden = warnings.length === 0;
  renderLive();
}

function renderLive() {
  const sorted = [...live].sort((a, b) => a.start_ms - b.start_ms);
  const box = $("notes-live-text");
  const atEnd = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  renderLines(box, sorted, { mic: "Eu", system: "Outros" });
  if (atEnd) box.scrollTop = box.scrollHeight;
}

function setLevel(track: "mic" | "system", level: number) {
  $(`lvl-${track}`).style.width = `${Math.round(level * 100)}%`;
}

async function start() {
  if (!app.view.config.notes_consent) {
    if (!(await askConsent())) return;
    try {
      await notes.acceptConsent();
      app.view.config.notes_consent = true;
    } catch (e) {
      toast(errorText(e), "error");
      return;
    }
  }
  const button = $<HTMLButtonElement>("notes-start");
  button.disabled = true;
  try {
    live = [];
    renderState(await notes.start($<HTMLSelectElement>("notes-mode").value as NotesMode));
  } catch (e) {
    toast(errorText(e), "error");
  } finally {
    button.disabled = false;
  }
}

function askConsent(): Promise<boolean> {
  const dlg = $<HTMLDialogElement>("notes-consent");
  dlg.returnValue = "";
  dlg.showModal();
  return new Promise((resolve) => {
    dlg.addEventListener("close", () => resolve(dlg.returnValue === "ok"), { once: true });
  });
}

async function stop() {
  const button = $<HTMLButtonElement>("notes-stop");
  button.disabled = true;
  try {
    renderState(await notes.stop());
  } catch (e) {
    toast(errorText(e), "error");
  } finally {
    button.disabled = false;
  }
}

// ---------------------------------------------------------------- saída do áudio do computador

/** Preenche o seletor com as saídas do sistema; a escolha guardada fica sempre na lista. */
export async function renderOutputDevices() {
  const select = $<HTMLSelectElement>("notes-output");
  let devices: InputDevice[] = [];
  let failed = "";
  try {
    devices = await notes.outputDevices();
  } catch (e) {
    failed = errorText(e);
  }
  const current = app.view.config.notes_output_device;
  const def = devices.find((d) => d.is_default);
  const options = [new Option(def ? `Predefinido do sistema (${def.name})` : "Predefinido do sistema", "")];
  for (const d of devices) options.push(new Option(d.name, d.name));
  // Escolhida mas desligada agora: continua na lista para não se perder a escolha.
  if (current && !devices.some((d) => d.name === current)) {
    options.push(new Option(`${current} (desligada: uso a predefinida)`, current));
  }
  select.replaceChildren(...options);
  select.value = current ?? "";
  $("notes-output-hint").textContent = failed
    ? `Não consegui listar as saídas: ${failed}`
    : "Escolhe a saída onde a reunião ou o vídeo toca. Se usas o Sonar ou outra app de áudio, pode ser um canal diferente do predefinido.";
}

function setupOutputDevice() {
  $<HTMLSelectElement>("notes-output").addEventListener("change", async (e) => {
    const value = (e.target as HTMLSelectElement).value;
    await save({ notes_output_device: value || null });
    // Mesmo que a gravação falhe a guardar, volto a mostrar o que está de facto guardado.
    void renderOutputDevices();
  });
}

// ---------------------------------------------------------------- teste de áudio

/** Limiar do teste: abaixo disto não se considera que se ouviu alguma coisa. */
const HEARD = 0.01;

function setupTest() {
  const go = $<HTMLButtonElement>("notes-test-go");
  const result = $("notes-test-result");
  const mic = $<HTMLMeterElement>("notes-test-mic");
  const sys = $<HTMLMeterElement>("notes-test-sys");
  const names: Partial<Record<"mic" | "system", string>> = {};
  let running = false;

  const describe = () => {
    const parts = [
      names.mic ? `Microfone: ${names.mic}` : null,
      names.system ? `Saída: ${names.system}` : null,
    ].filter(Boolean);
    return parts.join(" · ");
  };

  go.addEventListener("click", async () => {
    if (running) return;
    running = true;
    go.disabled = true;
    mic.value = 0;
    sys.value = 0;
    names.mic = undefined;
    names.system = undefined;
    result.textContent = "A testar… fala e põe som a tocar.";
    try {
      await notes.testStart(10);
    } catch (e) {
      running = false;
      go.disabled = false;
      result.textContent = `Não consegui testar: ${errorText(e)}`;
    }
  });

  void onNotesTest((e: NotesTestEvent) => {
    switch (e.kind) {
      case "level":
        (e.track === "mic" ? mic : sys).value = e.level;
        break;
      case "device":
        names[e.track === "mic" ? "mic" : "system"] = e.name;
        result.textContent = `A testar… ${describe()}`;
        break;
      case "lost":
        result.textContent = `Perdi a faixa ${e.track === "mic" ? "do microfone" : "do sistema"}: ${e.why}`;
        break;
      case "failed":
        running = false;
        go.disabled = false;
        result.textContent = `Não consegui testar: ${e.why}`;
        break;
      case "done": {
        running = false;
        go.disabled = false;
        const heardMic = e.mic_peak >= HEARD;
        const heardSys = e.system_peak >= HEARD;
        const where = describe();
        if (heardMic && heardSys) {
          result.textContent = `Tudo certo: ouvi o microfone e o áudio do computador. ${where}`;
        } else {
          const lines: string[] = [];
          if (!heardMic) lines.push("Não ouvi o microfone: confirma o microfone escolhido nas definições e fala mais perto.");
          if (!heardSys) {
            lines.push(
              "Não ouvi nada na saída do computador. Confirma que o vídeo toca nessa saída (escolhe-a no seletor \"Áudio do computador\").",
            );
          }
          result.textContent = `${lines.join(" ")} ${where}`;
        }
        break;
      }
    }
  });
}

export function renderNotesHint() {
  renderSpeakers();
  void renderOutputDevices();
  $("notes-cost").textContent =
    "O áudio vai à xAI e custa como transcrever um ficheiro: 0,10 US$ por hora de fala (mais 0,10 US$ por hora dos outros, se separares os oradores).";
}

// ---------------------------------------------------------------- vista da transcrição

/** Quem fala: uma faixa, e dentro dela um orador separado (se houver). */
interface Participant {
  key: string;
  name: string;
  color: number;
  ms: number;
}

/** Pausa a partir da qual a mesma pessoa abre um bloco novo. */
const TURN_GAP_MS = 20_000;
const PALETTE = 5;

const view = { query: "", hidden: new Set<string>(), times: true, hit: -1, hits: [] as HTMLElement[] };

const keyOf = (l: NotesLine) => `${l.track}:${l.speaker ?? ""}`;

/** Minúsculas e sem acentos, mantendo o comprimento para os índices coincidirem com o original. */
function fold(text: string): string {
  let out = "";
  for (const ch of text) {
    const f = ch.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();
    out += f.length === ch.length ? f : ch;
  }
  return out;
}

function participantsOf(meta: NotesMeta, lines: NotesLine[]): Participant[] {
  const names = namesOf(meta);
  const found = new Map<string, Participant>();
  for (const l of lines) {
    const key = keyOf(l);
    let p = found.get(key);
    if (!p) {
      p = { key, name: whoIs(l, names), color: found.size % PALETTE, ms: 0 };
      found.set(key, p);
    }
    p.ms += Math.max(0, l.end_ms - l.start_ms);
  }
  return [...found.values()];
}

/** Texto com as ocorrências da pesquisa dentro de `<mark>`; devolve-as para a navegação. */
function highlight(text: string, query: string): { nodes: Node[]; marks: HTMLElement[] } {
  const q = fold(query.trim());
  if (!q) return { nodes: [document.createTextNode(text)], marks: [] };
  const hay = fold(text);
  const nodes: Node[] = [];
  const marks: HTMLElement[] = [];
  let at = 0;
  for (let i = hay.indexOf(q); i !== -1; i = hay.indexOf(q, at)) {
    if (i > at) nodes.push(document.createTextNode(text.slice(at, i)));
    const m = el("mark", undefined, text.slice(i, i + q.length));
    nodes.push(m);
    marks.push(m);
    at = i + q.length;
  }
  if (at < text.length) nodes.push(document.createTextNode(text.slice(at)));
  return { nodes, marks };
}

function renderChips(people: Participant[]) {
  const total = people.reduce((n, p) => n + p.ms, 0) || 1;
  const box = $("nd-chips");
  box.hidden = people.length < 2;
  box.replaceChildren(
    ...people.map((p) => {
      const b = el("button", `nd-chip c${p.color}`);
      b.type = "button";
      b.setAttribute("aria-pressed", String(!view.hidden.has(p.key)));
      b.append(el("span", "nd-dot"), el("span", "nd-chip-name", p.name), el("span", "nd-chip-pct", `${Math.round((p.ms / total) * 100)}%`));
      b.title = "Mostrar ou esconder as falas desta pessoa";
      b.addEventListener("click", () => {
        if (!view.hidden.delete(p.key)) view.hidden.add(p.key);
        renderTranscript();
      });
      return b;
    }),
  );
}

function renderTranscript(keepScroll = true) {
  if (!detail) return;
  const { meta, lines } = detail;
  const box = $("nd-text");
  const top = box.scrollTop;
  const people = participantsOf(meta, lines);
  const color = new Map(people.map((p) => [p.key, p]));
  renderChips(people);

  view.hits = [];
  const blocks: HTMLElement[] = [];
  let current: { key: string; end: number; body: HTMLElement } | null = null;
  for (const l of lines) {
    const key = keyOf(l);
    if (view.hidden.has(key)) {
      current = null;
      continue;
    }
    const who = color.get(key)!;
    if (!current || current.key !== key || l.start_ms - current.end > TURN_GAP_MS) {
      const turn = el("section", `nd-turn c${who.color} ${l.track}`);
      const head = el("header", "nd-turn-head");
      head.append(el("span", "nd-avatar", [...who.name.trim()][0]?.toUpperCase() ?? "?"), el("strong", "note-who", who.name));
      if (view.times) head.append(el("time", "note-time", stamp(l.start_ms)));
      const body = el("div", "nd-turn-body");
      turn.append(head, body);
      blocks.push(turn);
      current = { key, end: l.end_ms, body };
    }
    current.end = Math.max(current.end, l.end_ms);
    const span = el("span", l.failed ? "nd-sentence failed" : "nd-sentence");
    if (l.failed) {
      span.textContent = "[trecho não transcrito]";
    } else {
      const { nodes, marks } = highlight(l.text, view.query);
      span.append(...nodes);
      view.hits.push(...marks);
    }
    current.body.append(span, " ");
  }
  box.classList.toggle("nd-no-times", !view.times);
  if (blocks.length) {
    box.replaceChildren(...blocks);
  } else {
    box.replaceChildren(el("p", "muted", lines.length ? "Todos os intervenientes estão escondidos." : "Esta gravação não tem fala transcrita."));
  }
  box.scrollTop = keepScroll ? top : 0;
  view.hit = view.hits.length && view.query.trim() ? Math.min(Math.max(view.hit, 0), view.hits.length - 1) : -1;
  updateHits(false);
}

function updateHits(scroll: boolean) {
  const q = view.query.trim();
  view.hits.forEach((m, i) => m.classList.toggle("current", i === view.hit));
  $("nd-count").textContent = !q ? "" : view.hits.length ? `${view.hit + 1} de ${view.hits.length}` : "Sem resultados";
  $<HTMLButtonElement>("nd-prev").disabled = $<HTMLButtonElement>("nd-next").disabled = view.hits.length < 2;
  if (scroll && view.hit >= 0) view.hits[view.hit].scrollIntoView({ block: "center", behavior: "smooth" });
}

function stepHit(delta: number) {
  if (!view.hits.length) return;
  view.hit = (view.hit + delta + view.hits.length) % view.hits.length;
  updateHits(true);
}

function resetView() {
  view.query = "";
  view.hidden.clear();
  view.hit = -1;
  $<HTMLInputElement>("nd-search").value = "";
}

function setupTranscriptView() {
  const search = $<HTMLInputElement>("nd-search");
  search.addEventListener("input", () => {
    view.query = search.value;
    view.hit = 0;
    renderTranscript();
    updateHits(true);
  });
  search.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      stepHit(e.shiftKey ? -1 : 1);
    }
  });
  $("nd-next").addEventListener("click", () => stepHit(1));
  $("nd-prev").addEventListener("click", () => stepHit(-1));
  $<HTMLInputElement>("nd-times").addEventListener("change", (e) => {
    view.times = (e.target as HTMLInputElement).checked;
    renderTranscript();
  });
}

// ---------------------------------------------------------------- lista e detalhe

function metaLine(m: NotesMeta): string {
  const when = new Date(m.started_at).toLocaleString("pt-PT", { dateStyle: "medium", timeStyle: "short" });
  const parts = [when, `${duration(m.audio_ms / 1000)} de fala`];
  if (m.cost_usd > 0) parts.push(usd(m.cost_usd, 3));
  if (m.interrupted) parts.push("interrompida");
  return parts.join(" · ");
}

async function refreshList() {
  const list = await notes.list().catch(() => [] as NotesMeta[]);
  $("notes-list").replaceChildren(
    ...list.map((m) => {
      const li = el("li");
      const b = el("button", "notes-item");
      b.type = "button";
      b.append(el("strong", undefined, m.title), el("span", "muted", metaLine(m)));
      b.addEventListener("click", () => void openDetail(m.id));
      li.append(b);
      return li;
    }),
  );
  $("notes-empty").hidden = list.length > 0;
}

async function openDetail(id: string) {
  try {
    detail = await notes.get(id);
  } catch (e) {
    toast(errorText(e), "error");
    return;
  }
  const { meta, lines, summary } = detail;
  $("notes-list-box").hidden = true;
  $("notes-idle").hidden = true;
  $("notes-detail").hidden = false;
  $<HTMLInputElement>("nd-title").value = meta.title;
  $<HTMLInputElement>("nd-mic").value = meta.mic_name;
  $<HTMLInputElement>("nd-system").value = meta.system_name;
  $("nd-meta").textContent = `${MODE_TITLE[meta.mode]} · ${metaLine(meta)} · ${meta.model}`;
  resetView();
  renderTranscript(false);
  renderSpeakerNames(meta, lines);
  const d = describeText(lines.map((l) => l.text).join(" "));
  $("nd-hint").textContent = d.hint;
  $<HTMLButtonElement>("nd-run").disabled = !d.ok;
  showSummary(summary);
  $("nd-detail").hidden = true;
}

function showSummary(text: string | null) {
  const box = $("nd-summary");
  box.hidden = !text;
  box.textContent = text ?? "";
}

function closeDetail() {
  detail = null;
  $("notes-detail").hidden = true;
  $("notes-list-box").hidden = false;
  $("notes-idle").hidden = state?.phase !== "idle";
  void refreshList();
}

async function rename() {
  if (!detail) return;
  try {
    const meta = await notes.rename(detail.meta.id, {
      title: $<HTMLInputElement>("nd-title").value,
      micName: $<HTMLInputElement>("nd-mic").value,
      systemName: $<HTMLInputElement>("nd-system").value,
    });
    detail.meta = meta;
    renderTranscript();
  } catch (e) {
    toast(errorText(e), "error");
  }
}

async function summarize() {
  if (!detail) return;
  const id = detail.meta.id;
  const run = $<HTMLButtonElement>("nd-run");
  run.disabled = true;
  run.textContent = "A resumir…";
  try {
    const text = await notes.text(id);
    const out = await summaries.run(text, $<HTMLSelectElement>("nd-kind").value as SummaryKind);
    await notes.saveSummary(id, out.text);
    if (detail?.meta.id === id) {
      detail.summary = out.text;
      showSummary(out.text);
      $("nd-detail").textContent = describeResult(out);
      $("nd-detail").hidden = false;
    }
  } catch (e) {
    toast(errorText(e), "error");
  } finally {
    run.disabled = false;
    run.textContent = "Resumir";
  }
}

async function copyText() {
  if (!detail) return;
  try {
    await navigator.clipboard.writeText(await notes.text(detail.meta.id));
    toast("Texto copiado.");
  } catch (e) {
    toast(errorText(e), "error");
  }
}

async function exportAs(markdown: boolean) {
  if (!detail) return;
  try {
    const path = await notes.exportTo(detail.meta.id, markdown);
    if (path) toast(`Guardado em ${path}`);
  } catch (e) {
    toast(errorText(e), "error");
  }
}

async function remove() {
  if (!detail) return;
  const { id, title } = detail.meta;
  if (!(await confirmDanger("Apagar esta gravação?", `"${title}" e a sua transcrição serão apagadas. Não há volta a dar.`, "Apagar"))) {
    return;
  }
  try {
    await notes.remove(id);
    closeDetail();
  } catch (e) {
    toast(errorText(e), "error");
  }
}

/** Um campo por orador separado, para lhe dar o nome ("Orador 2" → "Ana"). */
function renderSpeakerNames(meta: NotesMeta, lines: NotesLine[]) {
  const box = $("nd-speakers");
  const found = [...new Set(lines.map((l) => l.speaker).filter((n): n is number => n != null))].sort((a, b) => a - b);
  box.hidden = found.length === 0;
  box.replaceChildren(
    ...found.map((n) => {
      const label = el("label", undefined, `Orador ${n}`);
      const input = el("input");
      input.type = "text";
      input.maxLength = 80;
      input.placeholder = `Orador ${n}`;
      input.value = meta.speaker_names[String(n)] ?? "";
      input.addEventListener("change", () => void renameSpeaker(n, input.value));
      label.append(input);
      return label;
    }),
  );
}

async function renameSpeaker(n: number, name: string) {
  if (!detail) return;
  try {
    detail.meta = await speakers.rename(detail.meta.id, n, name);
    renderTranscript();
  } catch (e) {
    toast(errorText(e), "error");
  }
}

// ---------------------------------------------------------------- oradores
// A separação é da xAI: no fim da reunião a faixa dos outros vai inteira à API, que diz
// quem fala em cada palavra. Aqui só se liga e desliga.

function renderSpeakers() {
  $<HTMLInputElement>("notes-speakers-on").checked = app.view.config.notes_speakers;
  $("notes-speakers-hint").textContent =
    "No fim da reunião a faixa dos outros vai outra vez à xAI, que separa as vozes. Funciona melhor com poucas pessoas e sem vozes a sobrepor-se.";
}

function setupSpeakers() {
  $("notes-speakers-on").addEventListener("change", async (e) => {
    const checked = (e.target as HTMLInputElement).checked;
    if (await save({ notes_speakers: checked })) return;
    (e.target as HTMLInputElement).checked = !checked;
  });
  onViewChange(renderSpeakers);
}

// ---------------------------------------------------------------- arranque

export async function setupNotes(): Promise<void> {
  const kind = $<HTMLSelectElement>("nd-kind");
  for (const k of SUMMARY_KINDS) kind.append(new Option(k.label, k.value));
  kind.value = "meeting";

  $("notes-start").addEventListener("click", () => void start());
  $("notes-stop").addEventListener("click", () => void stop());
  $("nd-back").addEventListener("click", closeDetail);
  setupTranscriptView();
  $("nd-run").addEventListener("click", () => void summarize());
  $("nd-copy").addEventListener("click", () => void copyText());
  $("nd-md").addEventListener("click", () => void exportAs(true));
  $("nd-txt").addEventListener("click", () => void exportAs(false));
  $("nd-delete").addEventListener("click", () => void remove());
  for (const id of ["nd-title", "nd-mic", "nd-system"]) $(id).addEventListener("change", () => void rename());

  await onNotesState(renderState);
  await onNotesLine((e) => {
    if (e.id !== liveId) return;
    live.push(e.line);
    renderLive();
  });
  await onNotesLevel((e) => setLevel(e.track, e.level));

  setupSpeakers();
  setupOutputDevice();
  setupTest();
  renderSpeakers();
  renderNotesHint();
  renderState(await notes.state());
  await refreshList();
}
