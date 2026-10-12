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
  summaries,
  type NotesLine,
  type NotesMeta,
  type NotesMode,
  type NotesOut,
  type NotesState,
  type SummaryKind,
} from "./api";
import { duration, usd } from "./format";
import { $, app, confirmDanger, toast } from "./store";
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
function renderLines(box: HTMLElement, lines: NotesLine[], names: { mic: string; system: string }) {
  box.replaceChildren(
    ...lines.map((l) => {
      const p = el("p", `note-line ${l.track}${l.failed ? " failed" : ""}`);
      p.append(
        el("span", "note-who", l.track === "mic" ? names.mic : names.system),
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
  $("notes-live-title").textContent = finishing ? "A acabar de transcrever…" : (s.title ?? "A gravar");
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

export function renderNotesHint() {
  const c = app.view.config;
  $("notes-cost").textContent =
    c.stt_engine === "local"
      ? "Transcrição neste computador, sem custo: o áudio não sai daqui."
      : `O áudio vai ao OpenRouter (${c.stt_model}) e custa como transcrever um ficheiro: só se paga o que se fala.`;
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
  $("notes-detail").hidden = false;
  $<HTMLInputElement>("nd-title").value = meta.title;
  $<HTMLInputElement>("nd-mic").value = meta.mic_name;
  $<HTMLInputElement>("nd-system").value = meta.system_name;
  $("nd-meta").textContent = `${MODE_TITLE[meta.mode]} · ${metaLine(meta)} · ${meta.model}`;
  renderLines($("nd-text"), lines, { mic: meta.mic_name, system: meta.system_name });
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
    renderLines($("nd-text"), detail.lines, { mic: meta.mic_name, system: meta.system_name });
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

// ---------------------------------------------------------------- arranque

export async function setupNotes(): Promise<void> {
  const kind = $<HTMLSelectElement>("nd-kind");
  for (const k of SUMMARY_KINDS) kind.append(new Option(k.label, k.value));
  kind.value = "meeting";

  $("notes-start").addEventListener("click", () => void start());
  $("notes-stop").addEventListener("click", () => void stop());
  $("nd-back").addEventListener("click", closeDetail);
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

  renderNotesHint();
  renderState(await notes.state());
  await refreshList();
}
