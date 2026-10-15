// A janela do assistente: a mini-app onde o modo comando mostra a resposta. O backend
// (src-tauri/src/assistant/) é dono da conversa; esta página só a desenha e manda os
// pedidos. Sem framework, como o resto: cada mudança do backend chega como o estado
// completo (`assistant:state`) e as respostas a chegar como o texto até agora
// (`assistant:stream`), que atualiza só a última resposta.

import "@fontsource/instrument-serif/latin-400.css";
import mascotUrl from "../design/mascot/parado.svg?inline";
import {
  api,
  assistant,
  errorText,
  onAssistantState,
  onAssistantStream,
  type AssistantView,
  type ChatConversation,
  type ChatMessage,
  type ChatSearch,
  type ChatStreamEvent,
} from "./api";
import { icon } from "./assistant-icons";
import { closeHistory, isHistoryOpen, openHistory, setupHistory, syncHistory } from "./assistant-history";
import { parseBody, prettyApp, type Block } from "./assistant-text";
import { isMac, renderKeys } from "./format";
import { $, toast } from "./store";

/** A janela acompanha o fim da conversa enquanto estás perto dele. */
const STICK_PX = 80;
const MAX_INPUT_PX = 148;
const FLASH_MS = 1600;

let view: AssistantView | null = null;
/** Mensagens já mostradas: só as novas entram com animação. */
let seen = new Set<string>();
let shownConversation: string | null = null;
let contextOpen = false;
/** A miniatura da captura está aberta (toda, em vez de uma tira). */
let shotOpen = false;
/** Miniaturas já pedidas ao backend, por conversa; `null` = a imagem já não existe. */
const shots = new Map<string, string | null>();
const shotsAsked = new Set<string>();
let stick = true;
/** Respostas a chegar cujo texto ainda não foi desenhado (uma vez por frame). */
const dirty = new Set<string>();
let frame = 0;

const scroller = () => $("scroll");
const input = () => $<HTMLTextAreaElement>("input");

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// ---------------------------------------------------------------- mensagens

function codeNode(block: Extract<Block, { kind: "code" }>): HTMLElement {
  const wrap = el("div", "code");
  const head = el("div", "code-head");
  head.append(el("span", undefined, block.lang || "código"));
  const copy = el("button", "code-copy", "Copiar");
  copy.type = "button";
  copy.addEventListener("click", async () => {
    try {
      await api.copyText(block.code);
      copy.textContent = "Copiado";
      window.setTimeout(() => (copy.textContent = "Copiar"), FLASH_MS);
    } catch (e) {
      toast(errorText(e), "error");
    }
  });
  head.append(copy);
  const pre = el("pre");
  pre.append(el("code", undefined, block.code));
  wrap.append(head, pre);
  return wrap;
}

/** As marcas de citação que a xAI põe no meio do texto (`[[1]](https://…)`): as fontes
 *  aparecem à parte, por baixo da resposta (o mesmo que `citation_marks` no backend). */
const CITATION = /[ \t]?\[\[\d+\]\]\([^)\s]*\)/g;

/** Desenha o texto de uma resposta. Só o código tem formato (ver `parseBody`). */
function fillBody(body: HTMLElement, text: string) {
  const nodes: HTMLElement[] = [];
  for (const block of parseBody(text.replace(CITATION, ""))) {
    if (block.kind === "code") nodes.push(codeNode(block));
    else for (const paragraph of block.paragraphs) nodes.push(el("p", undefined, paragraph));
  }
  body.replaceChildren(...nodes);
}

function contextNode(conv: ChatConversation): HTMLElement {
  const wrap = el("aside", contextOpen ? "context open" : "context");
  const toggle = el("button", "context-toggle");
  toggle.type = "button";
  toggle.setAttribute("aria-expanded", String(contextOpen));
  toggle.append(icon("chevron"), el("span", undefined, conv.app ? `Texto selecionado · ${prettyApp(conv.app)}` : "Texto selecionado"));
  toggle.addEventListener("click", () => {
    contextOpen = !contextOpen;
    wrap.classList.toggle("open", contextOpen);
    toggle.setAttribute("aria-expanded", String(contextOpen));
  });
  wrap.append(toggle, el("blockquote", undefined, conv.selection));
  return wrap;
}

/** Pede (uma vez) a miniatura da captura de uma conversa; quando chega, redesenha. */
function loadShot(id: string) {
  if (shots.has(id) || shotsAsked.has(id)) return;
  shotsAsked.add(id);
  assistant
    .capture(id)
    .then((url) => shots.set(id, url))
    .catch(() => shots.set(id, null))
    .finally(() => {
      if (view?.conversation?.id === id) renderThread(view);
    });
}

/** A captura do ecrã sobre que a conversa começou: uma tira que se abre com um clique. A imagem só
 *  existe na memória do Ditok enquanto a conversa está aberta; uma conversa do histórico já não a tem. */
function shotNode(conv: ChatConversation): HTMLElement {
  const capture = conv.capture!;
  const url = shots.get(conv.id);
  const size = `${capture.width} × ${capture.height}`;
  const where = conv.app ? ` · ${prettyApp(conv.app)}` : "";
  const wrap = el("figure", shotOpen ? "shot open" : "shot");
  if (!url) {
    wrap.classList.add("gone");
    wrap.append(el("figcaption", undefined, url === null ? `Captura do ecrã${where} · já não está guardada` : `Captura do ecrã${where}`));
    loadShot(conv.id);
    return wrap;
  }
  const toggle = el("button", "shot-toggle");
  toggle.type = "button";
  toggle.setAttribute("aria-expanded", String(shotOpen));
  toggle.setAttribute("aria-label", "Captura do ecrã: ver inteira");
  const img = el("img");
  img.src = url;
  img.alt = "Captura do ecrã";
  img.decoding = "async";
  toggle.append(img);
  toggle.addEventListener("click", () => {
    shotOpen = !shotOpen;
    wrap.classList.toggle("open", shotOpen);
    toggle.setAttribute("aria-expanded", String(shotOpen));
  });
  wrap.append(toggle, el("figcaption", undefined, `Captura do ecrã · ${size}${where}`));
  return wrap;
}

function userNode(m: ChatMessage): HTMLElement {
  const node = el("article", "msg user");
  node.dataset.id = m.id;
  node.append(el("div", "bubble", m.text));
  return node;
}

/** As mensagens de erro do backend começam em minúscula ("sem créditos…"). */
const sentence = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** Mostra "feito" num botão durante um instante e volta ao que era. */
function flash(button: HTMLButtonElement, done: string) {
  const before = Array.from(button.childNodes);
  button.replaceChildren(icon("check"), document.createTextNode(done));
  button.classList.add("done");
  window.setTimeout(() => {
    button.replaceChildren(...before);
    button.classList.remove("done");
  }, FLASH_MS);
}

function action(label: string, name: "copy" | "insert" | "retry", main = false): HTMLButtonElement {
  const button = el("button", main ? "act main" : "act");
  button.type = "button";
  button.append(icon(name), document.createTextNode(label));
  return button;
}

function actionsNode(m: ChatMessage, last: boolean, conv: ChatConversation): HTMLElement | null {
  if (!view) return null;
  const row = el("div", "actions");
  const hasText = m.text.trim() !== "";
  if (hasText) {
    const copy = action("Copiar", "copy");
    copy.addEventListener("click", async () => {
      try {
        await assistant.copy(m.id);
        flash(copy, "Copiado");
      } catch (e) {
        toast(errorText(e), "error");
      }
    });
    row.append(copy);
  }
  if (hasText && view.can_apply) {
    const apply = action(conv.selection ? "Substituir seleção" : "Inserir no cursor", "insert", true);
    apply.addEventListener("click", async () => {
      apply.disabled = true;
      try {
        const { pasted } = await assistant.apply(m.id);
        flash(apply, pasted ? "Feito" : "Copiado");
        if (pasted) {
          // Um popup cumpriu a sua função: não fica por cima da app onde acabaste de colar.
          // A conversa continua no histórico.
          window.setTimeout(() => void assistant.close(), 450);
        } else {
          toast(`Não consegui voltar à app: o texto ficou copiado, cola com ${isMac ? "⌘V" : "Ctrl+V"}.`);
        }
      } catch (e) {
        toast(errorText(e), "error");
      } finally {
        apply.disabled = false;
      }
    });
    row.append(apply);
  }
  if (last) row.append(retryButton());
  return row.childElementCount > 0 ? row : null;
}

function retryButton(): HTMLButtonElement {
  const retry = action("Tentar de novo", "retry");
  retry.addEventListener("click", async () => {
    stick = true;
    try {
      await assistant.retry();
    } catch (e) {
      toast(errorText(e), "error");
    }
  });
  return retry;
}

// ---------------------------------------------------------------- pensar e pesquisar

/** Respostas cujo raciocínio está aberto, e cuja lista de fontes está toda à vista. */
const openActivity = new Set<string>();
const allSources = new Set<string>();
/** Fontes à vista antes do "mais N". */
const SOURCES_SHOWN = 6;

/** "6 s", "1 min 5 s"; abaixo de um segundo, "menos de 1 s". */
function duration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 1) return "menos de 1 s";
  if (s < 60) return `${s} s`;
  const rest = s % 60;
  return rest ? `${Math.floor(s / 60)} min ${rest} s` : `${Math.floor(s / 60)} min`;
}

/** Há quanto tempo a resposta começou (o backend cria-a no momento do pedido). */
const elapsedMs = (m: ChatMessage) => Math.max(0, Date.now() - Date.parse(m.at));

const searchIcon = (kind: ChatSearch["kind"]) => icon(kind === "x" ? "xlogo" : "globe");

function searchText(s: ChatSearch, live: boolean): string {
  const where = s.kind === "x" ? "no X" : "na web";
  const query = s.query?.trim();
  if (live) return query ? `A pesquisar ${where}: «${query}»` : `A pesquisar ${where}…`;
  return query ? `«${query}» ${where}` : `Pesquisa ${where}`;
}

/** A linha por cima da resposta: o que o modelo está a fazer enquanto não escreve ("A pensar…
 *  4 s", "A pesquisar na web: «…»") e, depois, o resumo ("Pensou 6 s · 2 pesquisas"), que abre
 *  o raciocínio e as pesquisas. `null` se não houver nada a dizer. */
function activityNode(m: ChatMessage, streaming: boolean): HTMLElement | null {
  const searches = m.searches ?? [];
  const reasoning = m.reasoning?.trim() ?? "";
  if (!m.thinks && searches.length === 0 && !reasoning) return null;

  const waiting = streaming && m.text === "";
  const wrap = el("details", "activity");
  wrap.dataset.id = m.id;
  wrap.open = openActivity.has(m.id);
  wrap.addEventListener("toggle", () => {
    if (wrap.open) openActivity.add(m.id);
    else openActivity.delete(m.id);
  });
  const summary = el("summary");
  const lastSearch = searches[searches.length - 1];
  if (waiting && lastSearch) {
    // A pesquisa mais recente é o que está a acontecer agora.
    wrap.classList.add("live");
    summary.append(searchIcon(lastSearch.kind), el("span", "label", searchText(lastSearch, true)));
  } else if (waiting) {
    wrap.classList.add("live");
    const label = el("span", "label", `A pensar… ${duration(elapsedMs(m))}`);
    label.dataset.since = m.at;
    summary.append(icon("spark"), label);
  } else {
    const parts: string[] = [];
    if (m.thinks) {
      const ms = m.thinking_ms;
      parts.push(ms != null ? `Pensou ${duration(ms)}` : streaming ? "Pensou" : "Parou a pensar");
    }
    if (searches.length) parts.push(searches.length === 1 ? "1 pesquisa" : `${searches.length} pesquisas`);
    const glyph = m.thinks || searches.length === 0 ? icon("spark") : searchIcon(searches[0].kind);
    summary.append(glyph, el("span", "label", parts.join(" · ")));
  }

  const details = el("div", "activity-body");
  if (searches.length) {
    const list = el("ul", "searches");
    for (const s of searches) {
      const item = el("li");
      item.append(searchIcon(s.kind), el("span", undefined, searchText(s, false)));
      list.append(item);
    }
    details.append(list);
  }
  if (reasoning) details.append(el("p", "reasoning", reasoning));
  else if (m.thinks && m.reasoning_tokens) {
    details.append(el("p", "reasoning-none", `A xAI não mostra o raciocínio deste modelo: pensou com ${m.reasoning_tokens.toLocaleString("pt-PT")} tokens.`));
  }
  if (details.childElementCount === 0) {
    // Nada para abrir: só a linha, sem seta.
    const line = el("div", wrap.className.replace("activity", "activity flat"));
    line.dataset.id = m.id;
    line.append(...Array.from(summary.childNodes));
    return line;
  }
  summary.prepend(icon("chevron"));
  wrap.append(summary, details);
  return wrap;
}

function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/** As fontes citadas, como pastilhas com o domínio; o clique abre-as no browser. */
function sourcesNode(m: ChatMessage): HTMLElement | null {
  const sources = m.sources ?? [];
  if (sources.length === 0) return null;
  const wrap = el("div", "sources");
  wrap.dataset.id = m.id;
  wrap.append(el("span", "sources-label", "Fontes"));
  const all = allSources.has(m.id);
  sources.forEach((s, i) => {
    if (!all && i >= SOURCES_SHOWN) return;
    const chip = el("button", "source");
    chip.type = "button";
    chip.title = s.title ? `${s.title}\n${s.url}` : s.url;
    chip.append(icon(hostOf(s.url).match(/^(x|twitter)\.com$/) ? "xlogo" : "link"), el("span", undefined, hostOf(s.url)));
    chip.addEventListener("click", () => void assistant.openSource(m.id, i).catch((e) => toast(errorText(e), "error")));
    wrap.append(chip);
  });
  if (!all && sources.length > SOURCES_SHOWN) {
    const more = el("button", "source more", `mais ${sources.length - SOURCES_SHOWN}`);
    more.type = "button";
    more.addEventListener("click", () => {
      allSources.add(m.id);
      const node = sourcesNode(m);
      if (node) wrap.replaceWith(node);
    });
    wrap.append(more);
  }
  return wrap;
}

/** Põe `next` no lugar de `current` (ou tira-o, ou acrescenta-o a seguir a `after`). */
function swap(current: Element | null, next: HTMLElement | null, place: (n: HTMLElement) => void) {
  if (current && next) current.replaceWith(next);
  else if (current) current.remove();
  else if (next) place(next);
}

/** O segundo a contar no "A pensar… 4 s": um só temporizador, que só existe enquanto há uma
 *  resposta a chegar. */
let ticker = 0;
function syncTicker(busy: boolean) {
  if (busy && !ticker) {
    ticker = window.setInterval(() => {
      for (const label of document.querySelectorAll<HTMLElement>(".activity.live .label[data-since]")) {
        label.textContent = `A pensar… ${duration(Math.max(0, Date.now() - Date.parse(label.dataset.since!)))}`;
      }
    }, 1000);
  } else if (!busy && ticker) {
    window.clearInterval(ticker);
    ticker = 0;
  }
}

function botNode(m: ChatMessage, last: boolean, streaming: boolean, conv: ChatConversation): HTMLElement {
  const node = el("article", last ? "msg bot last" : "msg bot");
  node.dataset.id = m.id;
  node.dataset.state = streaming ? "streaming" : (m.status ?? "done");
  const activity = activityNode(m, streaming);
  if (activity) node.append(activity);
  else if (streaming && m.text === "") {
    const dots = el("div", "thinking");
    dots.setAttribute("role", "status");
    dots.setAttribute("aria-label", "A pensar");
    dots.append(el("i"), el("i"), el("i"));
    node.append(dots);
  }
  const body = el("div", "body");
  fillBody(body, m.text);
  node.append(body);
  const sources = sourcesNode(m);
  if (sources) node.append(sources);
  if (!streaming && m.status === "stopped") node.append(el("p", "note", "Resposta parada."));
  if (!streaming && m.status === "failed") {
    node.append(el("p", "note error", sentence(m.error || "Não foi possível obter a resposta.")));
  }
  if (!streaming) {
    const actions = actionsNode(m, last, conv);
    if (actions) node.append(actions);
  }
  return node;
}

/** A conversa acabou num pedido sem resposta (a app fechou a meio): oferece repetir. */
function unansweredNode(): HTMLElement {
  const node = el("article", "msg bot last");
  node.append(el("p", "note", "Este pedido ficou sem resposta."));
  const row = el("div", "actions");
  row.append(retryButton());
  node.append(row);
  return node;
}

// ---------------------------------------------------------------- desenhar

function renderThread(v: AssistantView) {
  const conv = v.conversation;
  const messages = conv?.messages ?? [];
  const thread = $("thread");
  thread.hidden = messages.length === 0;
  $("empty").hidden = messages.length > 0;
  thread.setAttribute("aria-busy", String(v.busy));
  if (!conv || messages.length === 0) {
    thread.replaceChildren();
    return;
  }

  const nodes: HTMLElement[] = [];
  if (conv.capture) nodes.push(shotNode(conv));
  if (conv.selection) nodes.push(contextNode(conv));
  messages.forEach((m, i) => {
    const last = i === messages.length - 1;
    // A última resposta, enquanto chega, é a única em streaming. O "Tentar de novo" é só dela.
    const node = m.role === "user" ? userNode(m) : botNode(m, last, last && v.busy, conv);
    if (!seen.has(m.id)) {
      node.classList.add("enter");
      seen.add(m.id);
    }
    nodes.push(node);
  });
  const last = messages[messages.length - 1];
  if (last.role === "user" && !v.busy) nodes.push(unansweredNode());
  thread.replaceChildren(...nodes);
}

function renderChrome(v: AssistantView) {
  const conv = v.conversation;
  $("app").toggleAttribute("data-busy", v.busy);
  const title = $("title");
  title.textContent = conv?.title || "Nova conversa";
  title.classList.toggle("has-title", !!conv?.title);

  const keys = $("empty-keys");
  keys.hidden = !v.command_shortcut;
  if (v.command_shortcut) renderKeys(keys, v.command_shortcut);
  // Outra maneira de chegar aqui: perguntar sobre o ecrã.
  $("empty-screen").hidden = !v.screen_shortcut;
  if (v.screen_shortcut) renderKeys($("empty-screen-keys"), v.screen_shortcut);
  input().placeholder = v.attachment
    ? "Pergunta sobre a captura… (Enter sem texto: explica)"
    : v.conversation?.capture
      ? "Pergunta mais sobre a captura…"
      : "Escreve ou diz o que queres…";
  renderAttachment(v);

  const notice = $("notice");
  notice.hidden = !v.key_missing;
  if (v.key_missing) {
    const open = el("button", undefined, "Abrir as definições");
    open.type = "button";
    open.addEventListener("click", () => void api.overlayOpen("key").catch((e) => toast(errorText(e), "error")));
    notice.replaceChildren(document.createTextNode("Falta a chave da xAI. "), open, document.createTextNode("."));
  }
  updateSend();
}

/** A captura anexada que já se mostrou (para dar o foco à caixa só quando chega uma nova). */
let shownAttachment: string | null = null;

/** A captura do ecrã por cima da caixa de texto, à espera da pergunta. */
function renderAttachment(v: AssistantView) {
  const box = $("attachment");
  const a = v.attachment;
  box.hidden = !a;
  if (!a) {
    shownAttachment = null;
    return;
  }
  if (a.thumb !== shownAttachment) {
    shownAttachment = a.thumb;
    $<HTMLImageElement>("attachment-img").src = a.thumb;
    input().focus();
  }
  $("attachment-size").textContent = `${a.width} × ${a.height}`;
}

/** O que foi ditado enquanto se escolhia a área do ecrã: entra na caixa de texto. */
function takeDraft(v: AssistantView) {
  const text = v.draft?.trim();
  if (!text) return;
  const box = input();
  const before = box.value.trimEnd();
  box.value = before ? `${before} ${text}` : text;
  autosize();
  updateSend();
  box.focus();
  box.setSelectionRange(box.value.length, box.value.length);
  void assistant.takeDraft().catch(() => {});
}

function toEnd() {
  const s = scroller();
  s.scrollTop = s.scrollHeight;
}

/** Diz a quem usa um leitor de ecrã que a resposta acabou (e o que diz, em resumo). */
function announceAnswer(v: AssistantView) {
  const messages = v.conversation?.messages ?? [];
  const last = messages[messages.length - 1];
  if (!last || last.role !== "assistant" || last.status === "stopped") return;
  const text =
    last.status === "failed"
      ? `A resposta falhou. ${last.error ?? ""}`
      : `Resposta pronta. ${last.text.replace(/\s+/g, " ").slice(0, 240)}`;
  $("announce").textContent = text;
}

function applyView(v: AssistantView) {
  const wasBusy = view?.busy ?? false;
  const id = v.conversation?.id ?? null;
  if (id !== shownConversation) {
    // Outra conversa: nada do que lá está entra com animação, e começa-se pelo fim.
    shownConversation = id;
    seen = new Set(v.conversation?.messages.map((m) => m.id));
    contextOpen = false;
    shotOpen = false;
    stick = true;
  }
  view = v;
  dirty.clear();
  renderChrome(v);
  renderThread(v);
  takeDraft(v);
  syncTicker(v.busy);
  syncHistory({ activeId: id, saved: v.saved });
  if (wasBusy && !v.busy) announceAnswer(v);
  if (stick) requestAnimationFrame(toEnd);
}

// ---------------------------------------------------------------- resposta a chegar

function onStream(e: ChatStreamEvent) {
  const conv = view?.conversation;
  if (!conv || conv.id !== e.conversation_id) return;
  const message = conv.messages.find((m) => m.id === e.message_id);
  // Tudo vem sempre inteiro; menos do que o que já temos chegou fora de ordem.
  if (!message || e.text.length < message.text.length) return;
  if ((e.reasoning?.length ?? 0) < (message.reasoning?.length ?? 0)) return;
  if (e.searches.length < (message.searches?.length ?? 0) || e.sources.length < (message.sources?.length ?? 0)) return;
  message.text = e.text;
  message.reasoning = e.reasoning ?? undefined;
  message.thinking_ms = e.thinking_ms ?? undefined;
  message.searches = e.searches;
  message.sources = e.sources;
  dirty.add(message.id);
  if (!frame) frame = requestAnimationFrame(flushStream);
}

function flushStream() {
  frame = 0;
  for (const id of dirty) {
    const node = document.querySelector<HTMLElement>(`.msg.bot[data-id="${CSS.escape(id)}"]`);
    const m = view?.conversation?.messages.find((m) => m.id === id);
    if (!node || !m) continue;
    const body = node.querySelector<HTMLElement>(".body")!;
    const activity = activityNode(m, true);
    if (activity || m.text !== "") node.querySelector(".thinking")?.remove();
    swap(node.querySelector(":scope > .activity"), activity, (n) => node.prepend(n));
    fillBody(body, m.text);
    swap(node.querySelector(":scope > .sources"), sourcesNode(m), (n) => body.after(n));
  }
  dirty.clear();
  if (stick) toEnd();
}

// ---------------------------------------------------------------- compositor

function autosize() {
  const box = input();
  box.style.height = "auto";
  box.style.height = `${Math.min(box.scrollHeight, MAX_INPUT_PX)}px`;
}

function updateSend() {
  const send = $<HTMLButtonElement>("send");
  const busy = !!view?.busy;
  const empty = input().value.trim() === "";
  // Com uma captura anexada, enviar sem texto pede uma explicação do que se vê.
  send.disabled = !busy && empty && !view?.attachment;
  const label = busy ? "Parar" : empty && view?.attachment ? "Explicar a captura" : "Enviar";
  send.setAttribute("aria-label", label);
  send.title = busy ? "Parar a resposta" : `${label} (Enter)`;
}

async function submit() {
  if (view?.busy) {
    await assistant.stop().catch((e) => toast(errorText(e), "error"));
    return;
  }
  const box = input();
  const text = box.value.trim();
  if (!text && !view?.attachment) return;
  box.value = "";
  autosize();
  updateSend();
  stick = true;
  try {
    await assistant.send(text);
  } catch (e) {
    box.value = text;
    autosize();
    updateSend();
    toast(errorText(e), "error");
  }
}

async function newChat() {
  closeHistory();
  try {
    await assistant.newChat();
  } catch (e) {
    toast(errorText(e), "error");
  }
  input().focus();
}

function setupComposer() {
  const box = input();
  box.addEventListener("input", () => {
    autosize();
    updateSend();
  });
  box.addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || e.shiftKey || e.isComposing) return;
    e.preventDefault();
    // Enter não pára uma resposta por engano: para isso há o botão.
    if (!view?.busy) $<HTMLFormElement>("composer").requestSubmit();
  });
  $("composer").addEventListener("submit", (e) => {
    e.preventDefault();
    void submit();
  });
}

function setupKeys() {
  document.addEventListener("keydown", (e) => {
    const mod = e.ctrlKey || e.metaKey;
    const key = e.key.toLowerCase();
    if (mod && key === "k") {
      e.preventDefault();
      if (isHistoryOpen()) closeHistory();
      else openHistory();
    } else if (mod && key === "n") {
      e.preventDefault();
      void newChat();
    } else if (e.key === "Escape") {
      if (isHistoryOpen()) closeHistory();
      else if (input().value === "") void assistant.close();
    }
  });
}

/** Tira o menu do WebView (Anterior, Atualizar, Imprimir…), que denuncia que é uma
 *  página web. Fica nas caixas de texto e em `tauri dev`. */
function blockContextMenu() {
  if (import.meta.env.DEV) return;
  document.addEventListener("contextmenu", (e) => {
    if ((e.target as HTMLElement).closest("input, textarea")) return;
    e.preventDefault();
  });
}

async function main() {
  $<HTMLImageElement>("mascot").src = mascotUrl;
  blockContextMenu();
  setupComposer();
  setupKeys();
  setupHistory(() => input().focus());
  $("new-chat").addEventListener("click", () => void newChat());
  $("close").addEventListener("click", () => void assistant.close());
  $("attachment-drop").addEventListener("click", () => {
    void assistant.dropAttachment().catch((e) => toast(errorText(e), "error"));
    input().focus();
  });
  scroller().addEventListener(
    "scroll",
    () => {
      const s = scroller();
      stick = s.scrollHeight - s.scrollTop - s.clientHeight < STICK_PX;
      $("head").classList.toggle("scrolled", s.scrollTop > 2);
    },
    { passive: true },
  );

  // Primeiro os ouvintes, depois o estado: o que chegar a meio não se perde (o
  // texto de uma resposta vem sempre inteiro, por isso nada se repete nem falha).
  await Promise.all([onAssistantState(applyView), onAssistantStream(onStream)]);
  try {
    applyView(await assistant.state());
  } catch (e) {
    toast(errorText(e), "error");
  }
  input().focus();
}

void main();
