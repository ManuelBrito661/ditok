// O painel do histórico do assistente: desliza por cima da conversa, com pesquisa e as
// conversas agrupadas por dia. Apagar pede um segundo clique (sem diálogos por cima de
// uma janela tão pequena).

import { assistant, errorText, type ChatSummary } from "./api";
import { icon } from "./assistant-icons";
import { dayGroup, shortWhen, type DayGroup } from "./assistant-text";
import { $, toast } from "./store";

const GROUPS: DayGroup[] = ["Hoje", "Ontem", "Últimos 7 dias", "Últimos 30 dias", "Mais antigas"];
/** Quanto tempo um "Apagar?" espera pelo segundo clique. */
const ARM_FOR = 3000;
const SEARCH_DELAY = 150;

let items: ChatSummary[] = [];
let activeId: string | null = null;
let saved = true;
let query = "";
let searchTimer: number | undefined;
/** Cada pesquisa recebe um número: uma resposta atrasada de uma pesquisa antiga não conta. */
let seq = 0;
let afterClose: () => void = () => {};

export const isHistoryOpen = () => $("app").hasAttribute("data-history");

/** A conversa aberta mudou (ou o histórico foi ligado/desligado). */
export function syncHistory(ctx: { activeId: string | null; saved: boolean }) {
  activeId = ctx.activeId;
  saved = ctx.saved;
  if (isHistoryOpen()) void refreshHistory();
}

export function openHistory() {
  $("app").setAttribute("data-history", "");
  $("history").inert = false;
  $("open-history").setAttribute("aria-expanded", "true");
  void refreshHistory();
  requestAnimationFrame(() => $("search").focus());
}

export function closeHistory() {
  if (!isHistoryOpen()) return;
  $("app").removeAttribute("data-history");
  $("history").inert = true;
  $("open-history").setAttribute("aria-expanded", "false");
  query = "";
  $<HTMLInputElement>("search").value = "";
  afterClose();
}

export async function refreshHistory() {
  const mine = ++seq;
  let found: ChatSummary[];
  try {
    found = await assistant.list(query);
  } catch (e) {
    toast(errorText(e), "error");
    return;
  }
  if (mine !== seq) return;
  items = found;
  draw();
}

/** Um botão que só age ao segundo clique: o primeiro mostra `armed()`, o segundo
 *  chama `confirm`. Se não houver segundo clique, volta ao que era. */
function twoStep(btn: HTMLButtonElement, idle: () => void, armed: () => void, confirm: () => void) {
  let timer = 0;
  const reset = () => {
    window.clearTimeout(timer);
    btn.classList.remove("armed");
    idle();
  };
  idle();
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    if (btn.classList.contains("armed")) {
      reset();
      confirm();
      return;
    }
    btn.classList.add("armed");
    armed();
    timer = window.setTimeout(reset, ARM_FOR);
  });
  btn.addEventListener("blur", reset);
}

function rowFor(s: ChatSummary): HTMLElement {
  const row = document.createElement("div");
  row.className = s.id === activeId ? "row active" : "row";

  const main = document.createElement("button");
  main.type = "button";
  main.className = "row-main";
  const title = document.createElement("span");
  title.className = "row-title";
  title.textContent = s.title;
  const preview = document.createElement("span");
  preview.className = "row-prev";
  preview.textContent = s.preview;
  main.append(title, preview);
  main.addEventListener("click", async () => {
    try {
      await assistant.open(s.id);
      closeHistory();
    } catch (e) {
      toast(errorText(e), "error");
    }
  });

  const time = document.createElement("span");
  time.className = "row-time";
  time.textContent = shortWhen(s.updated_at);

  const del = document.createElement("button");
  del.type = "button";
  del.className = "row-del";
  del.title = "Apagar conversa";
  twoStep(
    del,
    () => {
      del.replaceChildren(icon("trash"));
      del.setAttribute("aria-label", `Apagar a conversa "${s.title}"`);
    },
    () => {
      del.textContent = "Apagar?";
      del.setAttribute("aria-label", "Confirmar: apagar a conversa");
    },
    async () => {
      try {
        await assistant.remove(s.id);
      } catch (e) {
        toast(errorText(e), "error");
        return;
      }
      items = items.filter((x) => x.id !== s.id);
      draw();
    },
  );

  row.append(main, time, del);
  return row;
}

function draw() {
  const list = $("history-list");
  const note = $("history-note");
  const clear = $<HTMLButtonElement>("clear-all");
  list.replaceChildren();
  clear.hidden = items.length === 0;
  note.hidden = items.length > 0;
  if (items.length === 0) {
    note.textContent = query
      ? "Nenhuma conversa com essas palavras."
      : saved
        ? "Ainda não há conversas. As que tiveres com o modo comando aparecem aqui."
        : "O histórico está desligado nas Definições, por isso as conversas só existem enquanto o Ditok estiver aberto.";
    return;
  }
  const now = new Date();
  for (const group of GROUPS) {
    const inGroup = items.filter((s) => dayGroup(s.updated_at, now) === group);
    if (inGroup.length === 0) continue;
    const label = document.createElement("div");
    label.className = "group";
    label.textContent = group;
    list.append(label, ...inGroup.map(rowFor));
  }
}

export function setupHistory(onClose: () => void) {
  afterClose = onClose;
  $("open-history").addEventListener("click", () => (isHistoryOpen() ? closeHistory() : openHistory()));
  $("close-history").addEventListener("click", closeHistory);
  $("scrim").addEventListener("click", closeHistory);
  $("search").addEventListener("input", (e) => {
    query = (e.target as HTMLInputElement).value;
    window.clearTimeout(searchTimer);
    searchTimer = window.setTimeout(() => void refreshHistory(), SEARCH_DELAY);
  });
  const clear = $<HTMLButtonElement>("clear-all");
  twoStep(
    clear,
    () => (clear.textContent = "Apagar todas"),
    () => (clear.textContent = "Apagar todas? Clica outra vez"),
    async () => {
      try {
        await assistant.clear();
      } catch (e) {
        toast(errorText(e), "error");
        return;
      }
      items = [];
      draw();
    },
  );
}
