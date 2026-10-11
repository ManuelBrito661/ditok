// Página Início, inspirada no ecrã principal do Flow: saudação, números do mês,
// cartão escuro com o atalho e a lista de ditados agrupada por dia.

import mascotUrl from "../design/mascot/parado.svg?inline";
import listeningMascotUrl from "../design/mascot/a-ouvir.svg?inline";
import thinkingMascotUrl from "../design/mascot/a-processar.svg?inline";
import readyMascotUrl from "../design/mascot/pronto.svg?inline";
import { api, errorText, type HistoryEntry, type Stats } from "./api";
import { countWords, dayLabel, ms, renderKeys, timeOfDay, usd } from "./format";
import { openSettings } from "./settings-dialog";
import { $, app, onViewChange, save, toast } from "./store";

let history: HistoryEntry[] = [];
let stats: Stats | null = null;

function renderStats() {
  const month = stats?.months[stats.current_month];
  $("s-count").textContent = stats ? String(month?.dictations ?? 0) : "—";
  $("s-cost").textContent = stats ? usd(month?.cost_usd ?? 0, 2) : "—";
  $("s-speed").textContent = stats?.total ? ms(stats.total.p50) : "—";
  // As palavras só se sabem pelo histórico (as métricas não guardam o texto).
  const now = new Date();
  const words = history
    .filter((h) => {
      const d = new Date(h.at);
      return d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth();
    })
    .reduce((sum, h) => sum + countWords(h.text), 0);
  $("s-words").textContent = app.view.config.history_enabled ? words.toLocaleString("pt-PT") : "—";
}

async function copy(text: string, button: HTMLButtonElement) {
  try {
    await api.copyText(text); // pelo backend: o clipboard do WebView nem sempre está disponível
    button.textContent = "Copiado";
    window.setTimeout(() => (button.textContent = "Copiar"), 1400);
  } catch (e) {
    toast(`Não foi possível copiar: ${errorText(e)}`, "error");
  }
}

function entryRow(h: HistoryEntry): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "entry";

  const time = document.createElement("time");
  time.dateTime = h.at;
  time.textContent = timeOfDay(h.at);

  const body = document.createElement("div");
  body.className = "entry-body";
  const text = document.createElement("p");
  text.textContent = h.text; // textContent: o texto ditado nunca é interpretado como HTML
  body.append(text);
  if (h.raw_text) {
    const raw = document.createElement("details");
    const summary = document.createElement("summary");
    summary.textContent = "Transcrição original";
    const p = document.createElement("p");
    p.textContent = h.raw_text;
    raw.append(summary, p);
    body.append(raw);
  }

  const actions = document.createElement("div");
  actions.className = "entry-actions";
  const button = document.createElement("button");
  button.type = "button";
  button.className = "secondary small";
  button.textContent = "Copiar";
  button.title = `${ms(h.total_ms)} · ${h.model}`;
  button.addEventListener("click", () => copy(h.text, button));
  actions.append(button);

  li.append(time, body, actions);
  return li;
}

function renderHistory() {
  const enabled = app.view.config.history_enabled;
  $("history-off").hidden = enabled;
  $("history-empty").hidden = !enabled || history.length > 0;

  // O backend devolve do mais recente para o mais antigo: basta cortar por dia.
  const groups = new Map<string, HistoryEntry[]>();
  for (const h of enabled ? history : []) {
    const label = dayLabel(new Date(h.at));
    if (!groups.has(label)) groups.set(label, []);
    groups.get(label)!.push(h);
  }
  $("history-days").replaceChildren(
    ...[...groups].map(([label, entries]) => {
      const section = document.createElement("section");
      section.className = "day";
      const h2 = document.createElement("h2");
      h2.className = "day-title";
      h2.textContent = label;
      const ol = document.createElement("ol");
      ol.className = "entries";
      ol.append(...entries.map(entryRow));
      section.append(h2, ol);
      return section;
    }),
  );
}

function renderView() {
  renderKeys($("promo-keys"), app.view.config.shortcut);
  $("key-banner").hidden = app.view.api_key_set;
  renderHistory();
  renderStats();
}

/** Volta a pedir histórico e métricas ao backend (no arranque e depois de cada ditado). */
export async function refreshHome() {
  const [h, s] = await Promise.allSettled([api.getHistory(), api.getStats()]);
  if (h.status === "fulfilled") history = h.value;
  if (s.status === "fulfilled") stats = s.value;
  renderHistory();
  renderStats();
}

/** A mascote do cartão escuro passa pelas quatro poses, como num ditado. */
function cycleMascot() {
  const img = $<HTMLImageElement>("promo-mascot");
  const poses = [mascotUrl, listeningMascotUrl, thinkingMascotUrl, readyMascotUrl];
  let i = 0;
  img.src = poses[0];
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  window.setInterval(() => {
    if (document.hidden) return; // janela minimizada: não vale a pena animar
    img.classList.add("swap");
    window.setTimeout(() => {
      i = (i + 1) % poses.length;
      img.src = poses[i];
      img.classList.remove("swap");
    }, 180);
  }, 2600);
}

export function setupHome() {
  for (const img of document.querySelectorAll<HTMLImageElement>(".empty-mascot")) img.src = mascotUrl;
  cycleMascot();
  $("key-banner-open").addEventListener("click", () => openSettings("transcricao"));
  $("history-enable").addEventListener("click", async () => {
    if (await save({ history_enabled: true })) void refreshHome();
  });
  onViewChange(renderView);
  renderView();
}

export function clearHomeHistory() {
  history = [];
  renderHistory();
  renderStats();
}
