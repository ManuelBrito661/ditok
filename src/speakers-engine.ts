// Os modelos de separação de oradores (Notas) nas Definições › Transcrição: escolher,
// descarregar e apagar. Igual aos modelos locais de transcrição (local-engine.ts). Quem
// trabalha é o backend (src-tauri/src/notes/speakers.rs); o progresso chega pelo evento
// `speakers:progress`.

import {
  errorText,
  onSpeakersProgress,
  speakers,
  type SpeakerModel,
  type SpeakersProgress,
  type SpeakersView,
} from "./api";
import { megabytes } from "./format";
import { $, confirmDanger, notifyViewChange, onViewChange, save, toast } from "./store";

let view: SpeakersView | null = null;
/** O progresso mais recente da descarga em curso. */
let progress: SpeakersProgress | null = null;
/** O último erro de uma descarga, até haver outra ação. */
let lastError: string | null = null;

async function refresh() {
  try {
    view = await speakers.list();
  } catch (e) {
    lastError = errorText(e);
  }
  render();
}

function button(label: string, className: string, action: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = className;
  b.textContent = label;
  b.dataset.action = action;
  b.addEventListener("click", onClick);
  return b;
}

async function download(m: SpeakerModel) {
  lastError = null;
  try {
    await speakers.download(m.id);
    progress = { model: m.id, file: "", done: 0, total: m.size_bytes, state: "downloading", message: null };
  } catch (e) {
    lastError = errorText(e);
  }
  await refresh();
}

async function remove(m: SpeakerModel) {
  const ok = await confirmDanger(
    `Apagar o modelo ${m.label}?`,
    `Liberta ${megabytes(m.size_bytes)} no disco (a segmentação só sai se nenhum outro modelo a usar). Podes voltar a descarregá-lo.`,
    "Apagar",
  );
  if (!ok) return;
  try {
    await speakers.remove(m.id);
    toast(`${m.label} apagado`);
  } catch (e) {
    toast(errorText(e), "error");
  }
  await refresh();
}

function modelRow(m: SpeakerModel): HTMLLIElement {
  const chosen = view?.selected === m.id;
  // "Em uso" só com o modelo mesmo descarregado; escolhido e em falta é outra coisa.
  const inUse = chosen && m.installed;
  const downloading = view?.downloading === m.id;
  const busyElsewhere = !!view?.downloading && !downloading;

  const li = document.createElement("li");
  li.className = "local-model";
  li.dataset.id = m.id;

  const text = document.createElement("div");
  text.className = "local-model-text";
  const title = document.createElement("strong");
  title.textContent = m.label;
  if (inUse) {
    const pill = document.createElement("span");
    pill.className = "pill ok";
    pill.textContent = "em uso";
    title.append(" ", pill);
  } else if (chosen) {
    const pill = document.createElement("span");
    pill.className = "pill warn";
    pill.textContent = "por descarregar";
    title.append(" ", pill);
  }
  const desc = document.createElement("span");
  desc.className = "row-desc";
  desc.textContent = m.description;
  const meta = document.createElement("span");
  meta.className = "row-desc";
  meta.textContent = `${megabytes(m.size_bytes)} no disco`;
  text.append(title, desc, meta);

  const actions = document.createElement("div");
  actions.className = "local-model-actions";
  if (downloading) {
    const p = progress?.model === m.id ? progress : null;
    const fraction = p && p.total ? Math.min(1, p.done / p.total) : 0;
    const meter = document.createElement("div");
    meter.className = "meter";
    meter.setAttribute("role", "progressbar");
    meter.setAttribute("aria-label", `A descarregar ${m.label}`);
    meter.setAttribute("aria-valuemin", "0");
    meter.setAttribute("aria-valuemax", "100");
    meter.setAttribute("aria-valuenow", String(Math.round(fraction * 100)));
    const bar = document.createElement("span");
    bar.style.width = `${Math.round(fraction * 100)}%`;
    meter.append(bar);
    const label = document.createElement("span");
    label.className = "row-desc";
    label.textContent = p?.state === "verifying" ? "A verificar o ficheiro…" : `${Math.round(fraction * 100)}%`;
    actions.append(meter, label, button("Cancelar", "ghost small", "cancel", () => void speakers.cancel()));
  } else if (m.installed) {
    const use = button(inUse ? "Em uso" : "Usar", "secondary small", "use", () => {
      void save({ notes_speaker_model: m.id });
    });
    use.disabled = inUse;
    actions.append(use, button("Apagar", "ghost small danger", "delete", () => void remove(m)));
  } else {
    const get = button("Descarregar", "primary small", "download", () => void download(m));
    get.disabled = busyElsewhere;
    actions.append(get);
  }

  li.append(text, actions);
  return li;
}

function render() {
  const available = view?.available ?? true;
  $("speakers-engine").hidden = !available;
  if (view) {
    $("speaker-models").replaceChildren(...view.models.map(modelRow));
  }
  const error = $("speaker-error");
  error.textContent = lastError ?? "";
  error.hidden = !lastError;
}

export function setupSpeakersEngine() {
  void onSpeakersProgress((p) => {
    progress = p;
    if (p.state === "error") lastError = p.message;
    if (p.state === "done") toast("Modelo de oradores descarregado");
    // O fim de uma descarga muda o que está instalado e o que se pode fazer.
    if (p.state === "done" || p.state === "cancelled" || p.state === "error") {
      progress = null;
      void refresh();
      // A página das Notas mostra se o modelo escolhido já está cá.
      if (p.state === "done") notifyViewChange();
    } else {
      render();
    }
  });
  onViewChange(() => void refresh());
  void refresh();
}
