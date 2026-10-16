// Página Início, inspirada no ecrã principal do Flow: saudação, números do mês,
// cartão escuro com o atalho e a lista de ditados agrupada por dia, com pesquisa.

import mascotUrl from "../design/mascot/parado.svg?inline";
import listeningMascotUrl from "../design/mascot/a-ouvir.svg?inline";
import thinkingMascotUrl from "../design/mascot/a-processar.svg?inline";
import readyMascotUrl from "../design/mascot/pronto.svg?inline";
import { api, errorText, summaries, type HistoryItem, type Stats, type UsageSummary } from "./api";
import {
  appColor,
  appLabel,
  budgetLevel,
  dayLabel,
  foldText,
  greeting,
  isMac,
  ms,
  renderKeys,
  STYLE_LABEL,
  timeOfDay,
  usd,
} from "./format";
import { addDictionaryEntry, countWordIn, suggestFrom } from "./dictionary-add";
import { openSettings } from "./settings-page";
import { describeResult, summaryModel, wordCount } from "./summary";
import { $, app, confirmDanger, onViewChange, save, toast } from "./store";

/** Quantos ditados se desenham de cada vez (com "Mostrar mais" para o resto). */
const PAGE = 200;
/** Espera depois da última tecla antes de filtrar: com muitos ditados, filtrar
 *  a cada tecla tornava a escrita pesada. */
const SEARCH_DELAY = 120;

let history: HistoryItem[] = [];
let stats: Stats | null = null;
/** As palavras do mês vêm das estatísticas de uso (o histórico só guarda os últimos ditados). */
let usage: UsageSummary | null = null;
/** Até o primeiro `getHistory` responder não se sabe se há ditados: nada de "Ainda não há". */
let historyState: "loading" | "ready" | "failed" = "loading";
/** O que escreveste na pesquisa, tal e qual (para a mensagem) e já normalizado. */
let rawQuery = "";
let query = "";
let limit = PAGE;
let searchTimer: number | undefined;
/** Ditados com o original à vista e a reprocessar: sobrevivem a cada redesenho. */
const openRaw = new Set<string>();
const reprocessing = new Set<string>();
/** O texto normalizado de cada ditado, calculado uma vez (a lista é substituída
 *  a cada `refreshHome`, e com ela os objetos: o WeakMap esquece os antigos). */
const folded = new WeakMap<HistoryItem, string>();

function haystack(h: HistoryItem): string {
  let s = folded.get(h);
  if (s === undefined) {
    s = foldText([h.text, h.raw_text ?? "", h.instruction ?? ""].join("\n"));
    folded.set(h, s);
  }
  return s;
}

function renderStats() {
  const month = stats?.months[stats.current_month];
  $("s-count").textContent = stats ? String(month?.dictations ?? 0) : "—";
  const spent = month?.cost_usd ?? 0;
  $("s-cost").textContent = stats ? usd(spent, 2) : "—";
  // Com orçamento: "4,20 US$ de 10,00 US$ este mês", âmbar aos 80 % e vermelho aos 100 %.
  const budget = app.view.config.monthly_budget_usd;
  $("s-cost-of").textContent = budget ? `de ${usd(budget, 2)} ` : "";
  $("s-cost-item").className = budgetLevel(spent, budget);
  $("s-speed").textContent = stats?.total ? ms(stats.total.p50) : "—";
  $("s-words").textContent = usage ? usage.month_words.toLocaleString("pt-PT") : "—";
}

/** `label` é o rótulo original do botão: com dois cliques seguidos, o texto do
 *  botão já seria "Copiado" e ficava preso nele. */
async function copy(text: string, button: HTMLButtonElement, label: string) {
  try {
    await api.copyText(text); // pelo backend: o clipboard do WebView nem sempre está disponível
    button.textContent = "Copiado";
    window.setTimeout(() => (button.textContent = label), 1400);
  } catch (e) {
    toast(`Não foi possível copiar: ${errorText(e)}`, "error");
  }
}

/** Apaga já, com "Desfazer" no aviso (como no dicionário). O áudio, se houver,
 *  só é apagado pelo backend quando o "Desfazer" já não puder chegar. */
async function remove(h: HistoryItem) {
  let removed;
  try {
    removed = await api.deleteHistoryEntry(h.id);
  } catch (e) {
    toast(`Não foi possível apagar: ${errorText(e)}`, "error");
    return;
  }
  history = history.filter((x) => x.id !== h.id);
  openRaw.delete(h.id);
  if (editing?.id === h.id) editing = null;
  renderHistory();
  renderStats();
  if (!removed) return; // já tinha sido apagado (ex.: noutra ação): nada para desfazer
  const entry = removed;
  const undo = toast("Ditado apagado", "ok", {
    label: "Desfazer",
    run: async () => {
      try {
        await api.restoreHistoryEntry(entry);
        await refreshHome();
        // O "Desfazer" escondeu-se: o foco vai para o ditado reposto.
        document.querySelector<HTMLElement>(`.entry[data-id="${CSS.escape(entry.id)}"] [data-action="copy"]`)?.focus();
      } catch (e) {
        toast(`Não foi possível repor: ${errorText(e)}`, "error");
      }
    },
  });
  // O botão de apagar desapareceu com a linha: o foco vai para o "Desfazer".
  undo?.focus();
}

async function reprocess(h: HistoryItem) {
  // O texto novo substitui o corrigido à mão: pergunta antes.
  if (
    h.corrected &&
    !(await confirmDanger(
      "Reprocessar substitui a tua correção",
      "O texto que corrigiste à mão vai ser trocado pelo da nova transcrição.",
      "Reprocessar",
    ))
  ) {
    return;
  }
  reprocessing.add(h.id);
  renderHistory();
  try {
    const updated = await api.reprocessHistoryEntry(h.id);
    toast(updated.text === h.text ? "Reprocessei: o texto ficou igual" : "Reprocessei: o texto foi atualizado");
    await refreshHome();
  } catch (e) {
    toast(`Não consegui reprocessar: ${errorText(e)}`, "error");
  } finally {
    reprocessing.delete(h.id);
    renderHistory();
  }
}


/** O ditado que está a ser corrigido (só um de cada vez), o que já escreveste nele e,
 *  se abriste "Ensinar uma palavra", os dois campos dessa pequena caixa. */
let editing: { id: string; draft: string; teach: { from: string; to: string } | null } | null = null;

function startEdit(h: HistoryItem) {
  editing = { id: h.id, draft: h.text, teach: null };
  renderHistory();
  document.querySelector<HTMLTextAreaElement>(`.entry[data-id="${CSS.escape(h.id)}"] textarea`)?.focus();
}

function cancelEdit() {
  const id = editing?.id;
  editing = null;
  renderHistory();
  // O editor desapareceu: o foco volta ao botão "Corrigir" do mesmo ditado.
  if (id) document.querySelector<HTMLElement>(`.entry[data-id="${CSS.escape(id)}"] [data-action="edit"]`)?.focus();
}

async function saveEdit(h: HistoryItem) {
  if (!editing || editing.id !== h.id) return;
  const text = editing.draft.trim();
  if (text === h.text.trim()) return cancelEdit(); // nada mudou
  try {
    const updated = await api.correctHistoryEntry(h.id, text);
    editing = null;
    // `updated` não traz o `has_audio`: junta-se ao que já se sabia.
    history = history.map((x) => (x.id === h.id ? { ...x, ...updated } : x));
    renderHistory();
    toast("Texto corrigido");
    document.querySelector<HTMLElement>(`.entry[data-id="${CSS.escape(h.id)}"] [data-action="edit"]`)?.focus();
  } catch (e) {
    toast(`Não consegui guardar: ${errorText(e)}`, "error");
  }
}

/** O editor inline: o texto numa caixa, "Guardar" e "Cancelar", e uma pequena caixa
 *  para ensinar ao dicionário uma palavra que o modelo escreve mal. */
function editorFor(h: HistoryItem): HTMLDivElement {
  const state = editing!;
  const box = document.createElement("div");
  box.className = "entry-edit";

  const area = document.createElement("textarea");
  area.value = state.draft; // .value: o texto nunca é interpretado como HTML
  area.rows = Math.min(10, Math.max(3, Math.ceil(state.draft.length / 70)));
  area.dataset.action = "edit-text";
  area.setAttribute("aria-label", "Texto corrigido");
  area.addEventListener("input", () => (state.draft = area.value));
  area.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      cancelEdit();
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      void saveEdit(h);
    }
  });

  const buttons = document.createElement("div");
  buttons.className = "entry-edit-buttons";
  const saveButton = smallButton("Guardar", "edit-save", "primary small");
  saveButton.addEventListener("click", () => void saveEdit(h));
  const cancel = smallButton("Cancelar", "edit-cancel");
  cancel.addEventListener("click", cancelEdit);
  const teach = smallButton("Ensinar uma palavra…", "edit-teach", "secondary small");
  teach.setAttribute("aria-expanded", String(!!state.teach));
  teach.addEventListener("click", () => {
    if (state.teach) {
      state.teach = null;
    } else {
      // A palavra que tiveres selecionado na caixa de cima é a que "deve ficar".
      const { selectionStart: start, selectionEnd: end } = area;
      state.teach = { from: suggestFrom(h.text, area.value, start, end), to: area.value.slice(start, end).trim() };
    }
    renderHistory();
  });
  buttons.append(saveButton, cancel, teach);
  box.append(area, buttons);

  const teachState = state.teach;
  if (teachState) {
    const panel = document.createElement("div");
    panel.className = "entry-teach";
    const hint = document.createElement("p");
    hint.className = "muted";
    hint.textContent =
      "À esquerda, como o modelo escreveu (opcional: vazio só acerta as maiúsculas); à direita, como deve ficar. " +
      "Seleciona a palavra certa no texto antes de abrir isto e eu preencho-a.";
    const row = document.createElement("div");
    row.className = "pair add-row";
    const from = document.createElement("input");
    from.placeholder = "Como saiu (opcional)";
    from.setAttribute("aria-label", "Como o modelo escreveu");
    from.value = teachState.from;
    from.spellcheck = false;
    from.addEventListener("input", () => (teachState.from = from.value));
    const arrow = document.createElement("span");
    arrow.className = "arrow";
    arrow.setAttribute("aria-hidden", "true");
    arrow.textContent = "→";
    const to = document.createElement("input");
    to.placeholder = "Como deve ficar";
    to.setAttribute("aria-label", "Como deve ficar escrito");
    to.value = teachState.to;
    to.spellcheck = false;
    to.addEventListener("input", () => (teachState.to = to.value));
    const add = smallButton("Adicionar ao dicionário", "teach-add", "primary small");
    add.addEventListener("click", async () => {
      // Os outros ditados: este ainda tem a palavra errada no texto guardado.
      const others = history.filter((x) => x.id !== h.id).map((x) => x.text);
      if (await addDictionaryEntry(teachState.from, teachState.to, (form) => countWordIn(others, form))) {
        state.teach = null;
        renderHistory();
      }
    });
    row.append(from, arrow, to, add);
    panel.append(hint, row);
    box.append(panel);
  }
  return box;
}

/** Os resumos pedidos nesta sessão (só em memória), por ditado, e os que estão a correr. */
const entrySummaries = new Map<string, { text: string; detail: string }>();
const summarizingIds = new Set<string>();
/** Só vale a pena resumir um ditado com algum comprimento. */
const MIN_WORDS_TO_SUMMARIZE = 150;

async function summarizeEntry(h: HistoryItem) {
  summarizingIds.add(h.id);
  renderHistory();
  try {
    const out = await summaries.run(h.text, "keypoints");
    entrySummaries.set(h.id, { text: out.text, detail: describeResult(out) });
  } catch (e) {
    toast(`Não consegui resumir: ${errorText(e)}`, "error");
  } finally {
    summarizingIds.delete(h.id);
    renderHistory();
  }
}

function smallButton(label: string, action: string, className = "ghost small"): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = className;
  b.textContent = label;
  b.dataset.action = action;
  return b;
}

function entryRow(h: HistoryItem): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "entry";
  li.dataset.id = h.id;

  const time = document.createElement("time");
  time.dateTime = h.at;
  time.textContent = timeOfDay(h.at);

  const body = document.createElement("div");
  body.className = "entry-body";
  const text = document.createElement("p");
  text.textContent = h.text; // textContent: o texto ditado nunca é interpretado como HTML
  body.append(editing?.id === h.id ? editorFor(h) : text);
  // Discreto, mas à vista: quanto tempo demorou e com que modelo.
  const meta = document.createElement("p");
  meta.className = "entry-meta";
  const shortModel = h.model.slice(h.model.lastIndexOf("/") + 1);
  const parts = h.total_ms != null ? [ms(h.total_ms), shortModel] : [shortModel];
  // O estilo só aparece quando mudou alguma coisa (o backend não o guarda nos outros casos).
  if (h.style && h.style !== "default") parts.push(STYLE_LABEL[h.style]);
  // Um comando diz qual foi a instrução, à frente de tudo.
  if (h.instruction) parts.unshift(`Comando: ${h.instruction}`);
  if (h.corrected) parts.push("corrigido");
  const where = h.instruction ? "comando em" : "ditado em";
  meta.title = h.app ? `${h.model} · ${where} ${h.app}` : h.model;
  // A app onde foi dito, como pastilha com a inicial (a cor é sempre a mesma para a mesma app).
  if (h.app) {
    const chip = document.createElement("span");
    chip.className = `app-chip ${appColor(h.app)}`;
    const initial = document.createElement("i");
    initial.setAttribute("aria-hidden", "true");
    initial.textContent = [...appLabel(h.app)][0] ?? "?";
    chip.append(initial, appLabel(h.app));
    meta.append(chip);
  }
  const detail = document.createElement("span");
  detail.textContent = parts.join(" · ");
  meta.append(detail);
  body.append(meta);

  const links = document.createElement("div");
  links.className = "entry-links";
  if (h.raw_text) {
    // Num comando, `raw_text` é o texto que estava selecionado.
    const noun = h.instruction ? "seleção" : "original";
    const open = openRaw.has(h.id);
    const toggle = smallButton(open ? `Esconder ${noun}` : `Ver ${noun}`, "raw");
    const panelId = `raw-${h.id}`;
    toggle.setAttribute("aria-expanded", String(open));
    toggle.setAttribute("aria-controls", panelId);
    toggle.addEventListener("click", () => {
      if (openRaw.has(h.id)) openRaw.delete(h.id);
      else openRaw.add(h.id);
      renderHistory();
    });
    links.append(toggle);

    const panel = document.createElement("div");
    panel.className = "entry-raw";
    panel.id = panelId;
    panel.hidden = !open;
    const label = document.createElement("p");
    label.className = "entry-raw-label";
    label.textContent = h.instruction ? "Texto selecionado" : "Transcrição antes da limpeza";
    const raw = document.createElement("p");
    raw.textContent = h.raw_text;
    const copyLabel = `Copiar ${noun}`;
    const copyRaw = smallButton(copyLabel, "copy-raw", "secondary small");
    copyRaw.addEventListener("click", () => copy(h.raw_text ?? "", copyRaw, copyLabel));
    panel.append(label, raw, copyRaw);
    body.append(panel);
  }
  // Só os ditados se corrigem e reprocessam: um comando vem de uma instrução e
  // precisava da mesma seleção.
  if (!h.instruction && editing?.id !== h.id) {
    const edit = smallButton("Corrigir", "edit");
    edit.title = "Corrigir o texto à mão (e ensinar palavras ao dicionário)";
    edit.addEventListener("click", () => startEdit(h));
    links.append(edit);
  }
  if (h.has_audio && !h.instruction) {
    const busy = reprocessing.has(h.id);
    const again = smallButton(busy ? "A reprocessar…" : "Reprocessar", "reprocess");
    again.title = "Transcrever outra vez o áudio guardado, com o modelo e o nível de limpeza atuais";
    again.disabled = busy;
    again.addEventListener("click", () => void reprocess(h));
    links.append(again);
  }
  // Um ditado comprido pode resumir-se em pontos-chave (o texto vai à xAI).
  if (!h.instruction && editing?.id !== h.id && wordCount(h.text) >= MIN_WORDS_TO_SUMMARIZE) {
    const busy = summarizingIds.has(h.id);
    const done = entrySummaries.has(h.id);
    const sum = smallButton(busy ? "A resumir…" : done ? "Resumir outra vez" : "Resumir", "summarize");
    sum.title = `Pontos-chave deste ditado, com ${summaryModel()}`;
    sum.disabled = busy;
    sum.addEventListener("click", () => void summarizeEntry(h));
    links.append(sum);
  }
  // Os botões ficam antes do painel do original, logo por baixo do texto.
  if (links.childElementCount) body.insertBefore(links, meta.nextSibling);

  const summary = entrySummaries.get(h.id);
  if (summary) {
    const panel = document.createElement("div");
    panel.className = "entry-raw";
    const label = document.createElement("p");
    label.className = "entry-raw-label";
    label.textContent = `Pontos-chave · ${summary.detail}`;
    const text = document.createElement("p");
    text.textContent = summary.text;
    const copySummary = smallButton("Copiar resumo", "copy-summary", "secondary small");
    copySummary.addEventListener("click", () => copy(summary.text, copySummary, "Copiar resumo"));
    panel.append(label, text, copySummary);
    body.append(panel);
  }

  const actions = document.createElement("div");
  actions.className = "entry-actions";
  const button = smallButton("Copiar", "copy", "secondary small");
  button.addEventListener("click", () => copy(h.text, button, "Copiar"));
  const del = document.createElement("button");
  del.type = "button";
  del.className = "icon-button";
  del.dataset.action = "delete";
  del.setAttribute("aria-label", "Apagar este ditado");
  del.title = "Apagar";
  del.innerHTML = `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M10 11v6m4-6v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3"/></svg>`;
  del.addEventListener("click", () => void remove(h));
  actions.append(button, del);

  li.append(time, body, actions);
  return li;
}

function renderHistory() {
  const enabled = app.view.config.history_enabled;
  const ready = enabled && historyState === "ready";
  $("history-skeleton").hidden = !(enabled && historyState === "loading");
  $("history-off").hidden = enabled;
  $("history-error").hidden = !enabled || historyState !== "failed";
  $("history-empty").hidden = !ready || history.length > 0;
  $("history-tools").hidden = !ready || history.length === 0;

  const matches = enabled ? (query ? history.filter((h) => haystack(h).includes(query)) : history) : [];
  const noResults = $("history-no-results");
  noResults.hidden = !ready || !query || matches.length > 0;
  noResults.textContent = noResults.hidden ? "" : `Sem resultados para «${rawQuery}»`;
  const more = $<HTMLButtonElement>("history-more");
  const rest = matches.length - limit;
  more.hidden = rest <= 0;
  more.textContent = `Mostrar mais (${rest.toLocaleString("pt-PT")})`;

  // Redesenhar a lista tira o foco ao botão onde estava: guarda-se qual era
  // (ditado e ação) para o devolver ao botão equivalente da lista nova.
  const active = document.activeElement as HTMLElement | null;
  const focusedId = active?.closest<HTMLElement>(".entry")?.dataset.id;
  const focusedAction = active?.dataset.action;

  // O backend devolve do mais recente para o mais antigo: basta cortar por dia.
  const groups = new Map<string, HistoryItem[]>();
  for (const h of matches.slice(0, limit)) {
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
  if (focusedId && focusedAction) {
    const row = document.querySelector<HTMLElement>(`.entry[data-id="${CSS.escape(focusedId)}"]`);
    row?.querySelector<HTMLElement>(`[data-action="${focusedAction}"]`)?.focus();
  }
}

function renderView() {
  renderKeys($("promo-keys"), app.view.config.shortcut);
  $("key-banner").hidden = app.view.api_key_set;
  renderHistory();
  renderStats();
}

/** Volta a pedir histórico e métricas ao backend (no arranque e depois de cada ditado). */
export async function refreshHome() {
  const [h, s, u] = await Promise.allSettled([api.getHistory(), api.getStats(), api.usageSummary()]);
  if (h.status === "fulfilled") {
    history = h.value;
    historyState = "ready";
  } else if (historyState !== "ready") {
    // Se já havia uma lista, fica a que estava; senão, diz o que falhou.
    historyState = "failed";
    $("history-error-text").textContent = errorText(h.reason);
  }
  if (s.status === "fulfilled") stats = s.value;
  if (u.status === "fulfilled") usage = u.value;
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

function setupSearch() {
  const input = $<HTMLInputElement>("history-search");
  const apply = () => {
    window.clearTimeout(searchTimer);
    rawQuery = input.value.trim();
    query = foldText(rawQuery);
    limit = PAGE; // uma pesquisa nova começa outra vez do topo
    renderHistory();
  };
  input.addEventListener("input", () => {
    window.clearTimeout(searchTimer);
    searchTimer = window.setTimeout(apply, SEARCH_DELAY);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || !input.value) return;
    // Esc limpa (em vez de deixar o campo de pesquisa fazê-lo à sua maneira).
    e.preventDefault();
    e.stopPropagation();
    input.value = "";
    apply();
  });
  // Ctrl+F (ou Ctrl+K; ⌘ no Mac) leva à pesquisa, se o Início estiver à vista.
  $("history-search-hint").textContent = isMac ? "⌘F" : "Ctrl+F";
  document.addEventListener("keydown", (e) => {
    const key = e.key.toLowerCase();
    if (!(isMac ? e.metaKey : e.ctrlKey) || e.altKey || e.shiftKey || (key !== "f" && key !== "k")) return;
    if ($("page-inicio").hidden || $("history-tools").hidden || document.querySelector("dialog[open]")) return;
    e.preventDefault();
    input.focus();
    input.select();
  });
  $("history-more").addEventListener("click", () => {
    // O foco fica no "Mostrar mais", que desce para o fim da lista nova.
    limit += PAGE;
    renderHistory();
  });
}

/** A onda de som decorativa do palco: barras de alturas e atrasos espalhados (sem aleatoriedade,
 *  para ser sempre a mesma), que respiram devagar. */
function buildWave() {
  const wave = $("hero-wave");
  const bars = 84;
  wave.replaceChildren(
    ...Array.from({ length: bars }, (_, i) => {
      const span = document.createElement("span");
      const shape = Math.abs(Math.sin(i * 0.62) * 0.55 + Math.sin(i * 1.7 + 1) * 0.45);
      span.style.setProperty("--h", String(Math.round(22 + shape * 78)));
      span.style.setProperty("--d", `${-((i * 173) % 2800)}ms`);
      return span;
    }),
  );
}

export function setupHome() {
  $("home-title").textContent = greeting();
  $("home-eyebrow").textContent = new Date().toLocaleDateString("pt-PT", { weekday: "long", day: "numeric", month: "long" });
  buildWave();
  $("history-retry").addEventListener("click", () => void refreshHome());
  for (const img of document.querySelectorAll<HTMLImageElement>(".empty-mascot")) img.src = mascotUrl;
  cycleMascot();
  setupSearch();
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
  openRaw.clear();
  renderHistory();
  renderStats();
}
