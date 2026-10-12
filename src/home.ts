// Página Início, inspirada no ecrã principal do Flow: saudação, números do mês,
// cartão escuro com o atalho e a lista de ditados agrupada por dia.

import mascotUrl from "../design/mascot/parado.svg?inline";
import listeningMascotUrl from "../design/mascot/a-ouvir.svg?inline";
import thinkingMascotUrl from "../design/mascot/a-processar.svg?inline";
import readyMascotUrl from "../design/mascot/pronto.svg?inline";
import { api, errorText, type HistoryEntry, type Stats } from "./api";
import { budgetLevel, countWords, dayLabel, greeting, ms, renderKeys, STYLE_LABEL, timeOfDay, usd } from "./format";
import { openSettings } from "./settings-dialog";
import { $, app, onViewChange, save, toast } from "./store";

let history: HistoryEntry[] = [];
let stats: Stats | null = null;
/** Até o primeiro `getHistory` responder não se sabe se há ditados: nada de "Ainda não há". */
let historyState: "loading" | "ready" | "failed" = "loading";

function renderStats() {
  const month = stats?.months[stats.current_month];
  $("s-count").textContent = stats ? String(month?.dictations ?? 0) : "—";
  const spent = month?.cost_usd ?? 0;
  $("s-cost").textContent = stats ? usd(spent, 2) : "—";
  // Com orçamento: "4,20 US$ de 10,00 US$ este mês", âmbar aos 80 % e vermelho aos 100 %.
  const budget = app.view.config.monthly_budget_usd;
  $("s-cost-of").textContent = budget ? `de ${usd(budget, 2)} ` : "";
  $("s-cost-item").className = budgetLevel(spent, budget);
  // O que resta na chave só aparece se a chave tiver um limite de gasto.
  const remaining = app.key?.limit_remaining;
  $("s-key-item").hidden = remaining == null;
  $("s-key").textContent = usd(remaining, 2);
  $("s-speed").textContent = stats?.total ? ms(stats.total.p50) : "—";
  // As palavras só se sabem pelo histórico (as métricas não guardam o texto).
  const now = new Date();
  const words = history
    .filter((h) => {
      const d = new Date(h.at);
      return d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth();
    })
    .reduce((sum, h) => sum + countWords(h.text), 0);
  $("s-words").textContent = app.view.config.history_enabled && historyState === "ready" ? words.toLocaleString("pt-PT") : "—";
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
  // Discreto, mas à vista: quanto tempo demorou e com que modelo.
  const meta = document.createElement("p");
  meta.className = "entry-meta";
  const shortModel = h.model.slice(h.model.lastIndexOf("/") + 1);
  const parts = h.total_ms != null ? [ms(h.total_ms), shortModel] : [shortModel];
  // O estilo só aparece quando mudou alguma coisa (o backend não o guarda nos outros casos).
  if (h.style && h.style !== "default") parts.push(STYLE_LABEL[h.style]);
  // Um comando diz qual foi a instrução, à frente de tudo.
  if (h.instruction) parts.unshift(`Comando: ${h.instruction}`);
  meta.textContent = parts.join(" · ");
  const where = h.instruction ? "comando em" : "ditado em";
  meta.title = h.app ? `${h.model} · ${where} ${h.app}` : h.model;
  body.append(meta);
  if (h.raw_text) {
    const raw = document.createElement("details");
    const summary = document.createElement("summary");
    // Num comando, `raw_text` é o texto que estava selecionado.
    summary.textContent = h.instruction ? "Texto selecionado" : "Transcrição original";
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
  button.addEventListener("click", () => copy(h.text, button));
  actions.append(button);

  li.append(time, body, actions);
  return li;
}

function renderHistory() {
  const enabled = app.view.config.history_enabled;
  $("history-off").hidden = enabled;
  $("history-error").hidden = !enabled || historyState !== "failed";
  $("history-empty").hidden = !enabled || historyState !== "ready" || history.length > 0;

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
  if (h.status === "fulfilled") {
    history = h.value;
    historyState = "ready";
  } else if (historyState !== "ready") {
    // Se já havia uma lista, fica a que estava; senão, diz o que falhou.
    historyState = "failed";
    $("history-error-text").textContent = errorText(h.reason);
  }
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
  $("home-title").textContent = greeting();
  $("history-retry").addEventListener("click", () => void refreshHome());
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
  historyState = "ready";
  renderHistory();
  renderStats();
}
