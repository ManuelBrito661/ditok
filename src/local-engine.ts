// O motor de transcrição local nas Definições › Transcrição: escolher entre a nuvem e
// este computador, e descarregar, escolher e apagar os modelos. Quem trabalha é o
// backend (src-tauri/src/local_models.rs e stt/local.rs): a descarga continua mesmo
// que se feche a janela e o progresso chega pelo evento `local-models:progress`.

import {
  errorText,
  localModels,
  onLocalModelProgress,
  type LocalModel,
  type LocalModelProgress,
  type LocalModelsView,
  type SttEngine,
} from "./api";
import { megabytes } from "./format";
import { $, app, confirmDanger, onViewChange, save, toast } from "./store";

let view: LocalModelsView | null = null;
/** O progresso mais recente de cada descarga (só do modelo que está a descarregar). */
let progress: LocalModelProgress | null = null;
/** O último erro de uma descarga, até haver outra ação. */
let lastError: string | null = null;

const gb = (mb: number) => `${(mb / 1024).toLocaleString("pt-PT", { maximumFractionDigits: 1 })} GB`;

async function refresh() {
  try {
    view = await localModels.list();
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

async function download(m: LocalModel) {
  lastError = null;
  try {
    await localModels.download(m.id);
    progress = { id: m.id, file: "", done: m.downloaded_bytes, total: m.size_bytes, state: "downloading", message: null };
  } catch (e) {
    lastError = errorText(e);
  }
  await refresh();
}

async function remove(m: LocalModel) {
  const ok = await confirmDanger(
    `Apagar o modelo ${m.label}?`,
    `Liberta ${megabytes(m.size_bytes)} no disco. Podes voltar a descarregá-lo quando quiseres.`,
    "Apagar",
  );
  if (!ok) return;
  try {
    await localModels.remove(m.id);
    toast(`${m.label} apagado`);
  } catch (e) {
    toast(errorText(e), "error");
  }
  await refresh();
}

function modelRow(m: LocalModel, enginePicked: boolean): HTMLLIElement {
  const c = app.view.config;
  const chosen = c.local_model === m.id;
  // "Em uso" só com o modelo mesmo descarregado; escolhido e em falta é outra coisa.
  const inUse = enginePicked && chosen && m.installed;
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
    if (enginePicked && !m.installed) {
      // O motor local está ligado mas este modelo ainda não está cá: não ditaria.
      pill.className = "pill warn";
      pill.textContent = "por descarregar";
    } else {
      // Escolhido, mas a nuvem é que está ativa.
      pill.className = "pill";
      pill.textContent = "escolhido";
    }
    title.append(" ", pill);
  }
  const desc = document.createElement("span");
  desc.className = "row-desc";
  desc.textContent = m.description;
  const meta = document.createElement("span");
  meta.className = "row-desc";
  meta.textContent = `${megabytes(m.size_bytes)} no disco · ~${gb(m.ram_mb)} de memória enquanto dita`;
  text.append(title, desc, meta);
  if (m.warning) {
    const warning = document.createElement("span");
    warning.className = "row-desc local-warning";
    warning.textContent = m.warning;
    text.append(warning);
  }

  const actions = document.createElement("div");
  actions.className = "local-model-actions";
  if (downloading) {
    const p = progress?.id === m.id ? progress : null;
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
    label.textContent =
      p?.state === "verifying"
        ? "A verificar o ficheiro…"
        : `${megabytes(p?.done ?? m.downloaded_bytes)} de ${megabytes(m.size_bytes)}`;
    actions.append(meter, label, button("Cancelar", "ghost small", "cancel", () => void localModels.cancel()));
  } else if (m.installed) {
    const use = button(inUse ? "Em uso" : "Usar", "secondary small", "use", () => {
      void save({ stt_engine: "local", local_model: m.id });
    });
    use.disabled = inUse;
    actions.append(use, button("Apagar", "ghost small danger", "delete", () => void remove(m)));
  } else {
    const partial = m.downloaded_bytes > 0 ? ` (retoma de ${megabytes(m.downloaded_bytes)})` : "";
    const get = button(`Descarregar${partial}`, "primary small", "download", () => void download(m));
    get.disabled = busyElsewhere;
    actions.append(get);
  }

  li.append(text, actions);
  return li;
}

function render() {
  const c = app.view.config;
  const cloud = $<HTMLInputElement>("engine-cloud");
  const local = $<HTMLInputElement>("engine-local");
  cloud.checked = c.stt_engine === "cloud";
  local.checked = c.stt_engine === "local";

  const available = view?.available ?? true;
  local.disabled = !available;
  $("engine-unavailable").hidden = available;

  // O seletor de modelos da nuvem só interessa com a nuvem.
  $("stt-model-row").hidden = c.stt_engine === "local";

  // A lista de modelos locais vê-se com o motor local escolhido (ou já com algum modelo
  // descarregado ou a descarregar: é assim que se prepara antes de mudar).
  const hasAny = !!view?.models.some((m) => m.installed || m.downloaded_bytes > 0);
  $("local-engine").hidden = !available || (c.stt_engine !== "local" && !hasAny && !view?.downloading);
  if (view) {
    $("local-models").replaceChildren(...view.models.map((m) => modelRow(m, c.stt_engine === "local")));
  }
  const error = $("local-error");
  error.textContent = lastError ?? "";
  error.hidden = !lastError;

  const idle = $<HTMLInputElement>("local-idle");
  if (document.activeElement !== idle) idle.value = String(c.local_idle_minutes);
}

/** Um modelo local tem de estar descarregado para ditar: avisa se se escolhe o motor sem ele. */
async function chooseEngine(engine: SttEngine) {
  if (!(await save({ stt_engine: engine }))) return;
  if (engine === "local") {
    await refresh();
    const picked = view?.models.find((m) => m.id === app.view.config.local_model);
    if (picked && !picked.installed) {
      toast(`Descarrega o modelo ${picked.label} (ou escolhe outro) para ditar neste computador.`);
    }
  }
}

export function setupLocalEngine() {
  for (const input of document.querySelectorAll<HTMLInputElement>('input[name="stt-engine"]')) {
    input.addEventListener("change", () => {
      if (input.checked) void chooseEngine(input.value as SttEngine);
    });
  }
  $("local-idle").addEventListener("change", (e) => {
    const minutes = Math.round(Number((e.target as HTMLInputElement).value));
    void save({ local_idle_minutes: minutes });
  });
  void onLocalModelProgress((p) => {
    progress = p;
    if (p.state === "error") lastError = p.message;
    if (p.state === "done") toast("Modelo descarregado");
    // O fim de uma descarga muda o que está instalado e o que se pode fazer.
    if (p.state === "done" || p.state === "cancelled" || p.state === "error") {
      progress = null;
      void refresh();
    } else {
      render();
    }
  });
  onViewChange(() => void refresh());
  void refresh();
}
