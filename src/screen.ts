// A seleção da captura do ecrã: mostra a fotografia "congelada", deixa escolher uma área com o
// rato (ou o ecrã todo com Enter) e escrever ou ditar a pergunta. O backend (src-tauri/src/screen/)
// guarda a fotografia e faz o recorte: esta página só desenha e diz que área e que pergunta.
//
// A janela cobre o monitor inteiro e está por cima de tudo, por isso tem de se poder fechar de
// várias maneiras: Esc, o ✕, ou perder o foco (o backend trata disso).

import {
  api,
  errorText,
  onMicFailure,
  onMicLevel,
  onMicState,
  onScreenPartial,
  onScreenTranscript,
  screen,
  type MicState,
} from "./api";
import { $ } from "./store";

/** Uma caixa em píxeis CSS da janela. */
interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Menos do que isto, arrastado, é um clique (e um clique escolhe o ecrã todo). */
const MIN = 8;
const MAX_PROMPT_PX = 168;
/** Distância entre a área e a caixa de pergunta. */
const GAP = 12;
const EDGE = 16;

type Mode = "idle" | "selecting" | "selected";
type Drag =
  | { kind: "new"; ox: number; oy: number }
  | { kind: "move"; px: number; py: number; start: Box }
  | { kind: "resize"; handle: string; start: Box };

let mode: Mode = "idle";
let box: Box | null = null;
let drag: Drag | null = null;
let imageSize = { width: 1, height: 1 };
let viewport = { width: window.innerWidth, height: window.innerHeight };
let mic: MicState = "idle";
let micFailure = "";
/** A pergunta teve voz (o assistente marca-a como dita). */
let spoken = false;
let sending = false;
let closing = false;

const stage = () => $("stage");
const selEl = () => $("sel");
const askEl = () => $<HTMLFormElement>("ask");
const promptEl = () => $<HTMLTextAreaElement>("prompt");

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  node.className = className;
  return node;
}

// ---------------------------------------------------------------- a área

function fromCorners(ax: number, ay: number, bx: number, by: number): Box {
  const x1 = clamp(Math.min(ax, bx), 0, window.innerWidth);
  const y1 = clamp(Math.min(ay, by), 0, window.innerHeight);
  const x2 = clamp(Math.max(ax, bx), 0, window.innerWidth);
  const y2 = clamp(Math.max(ay, by), 0, window.innerHeight);
  return { x: x1, y: y1, w: x2 - x1, h: y2 - y1 };
}

function setMode(next: Mode) {
  mode = next;
  stage().dataset.mode = next;
}

/** Desenha a área e põe a caixa de pergunta no sítio. */
function paint() {
  const sel = selEl();
  sel.hidden = !box;
  if (!box) return;
  Object.assign(sel.style, { left: `${box.x}px`, top: `${box.y}px`, width: `${box.w}px`, height: `${box.h}px` });
  // O ecrã todo escolhido: sem dimming nem pegas (não há nada a escurecer nem a ajustar).
  sel.toggleAttribute("data-full", box.w >= window.innerWidth - 1 && box.h >= window.innerHeight - 1);
  // O tamanho que segue para o modelo, em píxeis da fotografia (não da janela).
  const rx = imageSize.width / window.innerWidth;
  const ry = imageSize.height / window.innerHeight;
  $("size").textContent = `${Math.round(box.w * rx)} × ${Math.round(box.h * ry)}`;
  layoutAsk();
}

/** A caixa de pergunta fica por baixo da área; sem espaço, por cima; sem espaço nenhum (o ecrã
 *  todo), dentro dela, junto ao fundo. A etiqueta do tamanho passa para dentro da área quando
 *  não cabe por cima. */
function layoutAsk() {
  const ask = askEl();
  if (!box || ask.hidden) return;
  const w = ask.offsetWidth;
  const h = ask.offsetHeight;
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const x = clamp(box.x + box.w / 2 - w / 2, EDGE, Math.max(EDGE, vw - w - EDGE));
  let y: number;
  let labelInside = box.y < 32;
  if (box.y + box.h + GAP + h + EDGE <= vh) {
    y = box.y + box.h + GAP;
  } else if (box.y - GAP - h - 34 >= EDGE) {
    y = box.y - GAP - h - 34;
    labelInside = true;
  } else {
    y = vh - h - 28;
    labelInside = true;
  }
  Object.assign(ask.style, { left: `${x}px`, top: `${Math.max(EDGE, y)}px` });
  selEl().toggleAttribute("data-top", labelInside);
}

function showAsk() {
  const ask = askEl();
  if (ask.hidden) {
    ask.hidden = false;
    updateSend();
    autosize();
  }
}

/** A área está escolhida: mostra a pergunta e dá-lhe o foco. */
function settle() {
  setMode("selected");
  showAsk();
  paint();
  promptEl().focus({ preventScroll: true });
}

function selectAll() {
  box = { x: 0, y: 0, w: window.innerWidth, h: window.innerHeight };
  settle();
}

// ---------------------------------------------------------------- rato

const insideBox = (b: Box, x: number, y: number) => x >= b.x && x <= b.x + b.w && y >= b.y && y <= b.y + b.h;

function onPointerDown(e: PointerEvent) {
  if (e.button !== 0 || sending) return;
  const target = e.target as HTMLElement;
  if (askEl().contains(target) || target.closest("#close")) return;
  stage().setPointerCapture(e.pointerId);
  const handle = target.closest<HTMLElement>(".handle")?.dataset.h;
  if (box && handle) drag = { kind: "resize", handle, start: { ...box } };
  else if (box && insideBox(box, e.clientX, e.clientY)) drag = { kind: "move", px: e.clientX, py: e.clientY, start: { ...box } };
  else drag = { kind: "new", ox: e.clientX, oy: e.clientY };
}

function onPointerMove(e: PointerEvent) {
  if (!drag) return;
  const x = e.clientX;
  const y = e.clientY;
  if (drag.kind === "new") {
    // Só passa a ser uma seleção quando o rato se afasta um pouco: um clique não pisca.
    if (mode !== "selecting" && Math.hypot(x - drag.ox, y - drag.oy) < 3) return;
    setMode("selecting");
    askEl().hidden = true;
    box = fromCorners(drag.ox, drag.oy, x, y);
  } else if (drag.kind === "move") {
    const s = drag.start;
    box = {
      ...s,
      x: clamp(s.x + x - drag.px, 0, window.innerWidth - s.w),
      y: clamp(s.y + y - drag.py, 0, window.innerHeight - s.h),
    };
  } else {
    const s = drag.start;
    let [l, t, r, b] = [s.x, s.y, s.x + s.w, s.y + s.h];
    const cx = clamp(x, 0, window.innerWidth);
    const cy = clamp(y, 0, window.innerHeight);
    if (drag.handle.includes("w")) l = cx;
    if (drag.handle.includes("e")) r = cx;
    if (drag.handle.includes("n")) t = cy;
    if (drag.handle.includes("s")) b = cy;
    box = fromCorners(l, t, r, b);
  }
  paint();
}

function onPointerUp(e: PointerEvent) {
  if (!drag) return;
  const finished = drag;
  drag = null;
  if (stage().hasPointerCapture(e.pointerId)) stage().releasePointerCapture(e.pointerId);
  if (finished.kind === "new") {
    // Um clique, ou um arrasto minúsculo: o ecrã todo.
    if (!box || mode !== "selecting" || box.w < MIN || box.h < MIN) return selectAll();
  } else if (box && (box.w < MIN || box.h < MIN)) {
    box = finished.start;
  }
  settle();
}

// ---------------------------------------------------------------- a pergunta

function autosize() {
  const field = promptEl();
  field.style.height = "auto";
  field.style.height = `${Math.min(field.scrollHeight, MAX_PROMPT_PX)}px`;
  layoutAsk();
}

/** Sem texto, o botão diz o que vai acontecer: pede-se uma explicação. */
function updateSend() {
  const empty = promptEl().value.trim() === "";
  $("send").firstElementChild!.textContent = empty ? "Explicar" : "Enviar";
}

function say(text: string, isError = false) {
  const status = $("status");
  status.textContent = text;
  status.classList.toggle("error", isError);
}

function micBusy() {
  return mic === "recording" || mic === "processing";
}

/** O que o microfone está a fazer, na linha de estado e no botão. */
function renderMic() {
  const ask = askEl();
  if (micBusy()) ask.dataset.mic = mic;
  else delete ask.dataset.mic;
  const button = $<HTMLButtonElement>("mic");
  button.setAttribute("aria-pressed", String(mic === "recording"));
  // Depois de uma falha, um clique na pílula do Ditok "tentava de novo" o ditado antigo: durante
  // esses segundos o botão espera.
  button.disabled = mic === "failed";
  if (mic === "recording") say("A ouvir… clica no microfone para parar.");
  else if (mic === "processing") say("A transcrever…");
  else if (mic === "failed") say(micFailure ? `Falhou: ${micFailure}` : "O microfone falhou.", true);
  else if (mic === "nothing-heard") say("Não ouvi nada.");
  else if (!sending) say("");
  if (!micBusy()) {
    $("live").hidden = true;
    ask.style.removeProperty("--level");
  }
  layoutAsk();
}

function onTranscript(text: string) {
  spoken = true;
  if (!box) selectAll();
  const field = promptEl();
  const before = field.value.trimEnd();
  field.value = before ? `${before} ${text}` : text;
  $("live").hidden = true;
  autosize();
  updateSend();
  field.focus({ preventScroll: true });
  field.setSelectionRange(field.value.length, field.value.length);
}

async function submit() {
  if (!box || sending || closing) return;
  if (micBusy()) return say("Espera que a gravação acabe.");
  sending = true;
  askEl().dataset.sending = "";
  say("A enviar…");
  try {
    await screen.submit(
      promptEl().value.trim(),
      { x: box.x, y: box.y, width: box.w, height: box.h },
      { width: window.innerWidth, height: window.innerHeight },
      spoken,
    );
    // A janela fecha-se sozinha (e o assistente abre-se com a resposta).
  } catch (e) {
    sending = false;
    delete askEl().dataset.sending;
    say(errorText(e), true);
  }
}

async function cancel() {
  if (closing) return;
  closing = true;
  await screen.cancel().catch(() => {});
}

// ---------------------------------------------------------------- teclado

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    void cancel();
    return;
  }
  // Uma tecla do atalho ainda em baixo (a repetir) quando a janela abre não é uma pergunta.
  if (e.isComposing || e.repeat || sending) return;
  const inButton = (e.target as HTMLElement).closest("button");
  if (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey) {
    if (inButton) return; // Enter num botão carrega nele
    e.preventDefault();
    if (box) void submit();
    else selectAll();
    return;
  }
  // Começar a escrever sem ter escolhido nada: o ecrã todo, e a letra vai para a pergunta.
  if (!box && e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) {
    selectAll();
  }
}

/** Tira o menu do WebView (Anterior, Atualizar, Imprimir…). Fica na caixa de texto e em `tauri dev`. */
function blockContextMenu() {
  if (import.meta.env.DEV) return;
  document.addEventListener("contextmenu", (e) => {
    if ((e.target as HTMLElement).closest("textarea")) return;
    e.preventDefault();
  });
}

function onResize() {
  // A janela passou a ecrã inteiro depois de criada: o que estava escolhido acompanha.
  if (box && viewport.width > 0 && viewport.height > 0) {
    const sx = window.innerWidth / viewport.width;
    const sy = window.innerHeight / viewport.height;
    box = { x: box.x * sx, y: box.y * sy, w: box.w * sx, h: box.h * sy };
    paint();
  }
  viewport = { width: window.innerWidth, height: window.innerHeight };
}

// ---------------------------------------------------------------- arranque

/** "Ctrl+Win" → duas teclas. */
function dictateHint(label: string) {
  const tip = el("p", "line");
  tip.append("Segura ");
  label.split("+").forEach((key, i) => {
    if (i > 0) tip.append(" + ");
    const kbd = document.createElement("kbd");
    kbd.textContent = key;
    tip.append(kbd);
  });
  tip.append(" para ditar a pergunta");
  $("hint").append(tip);
}

async function drawShot() {
  const bytes = await screen.image();
  // Um JPEG começa por FF D8: se não, diz-se no registo o que chegou em vez de "could not be decoded".
  if (bytes.length < 2 || bytes[0] !== 0xff || bytes[1] !== 0xd8) {
    const head = Array.from(bytes.subarray(0, 8), (b) => b.toString(16).padStart(2, "0")).join(" ");
    throw new Error(`a fotografia não é um JPEG (${bytes.length} bytes, começa por ${head || "nada"})`);
  }
  const bitmap = await createImageBitmap(new Blob([bytes as BlobPart], { type: "image/jpeg" }));
  const canvas = $<HTMLCanvasElement>("shot");
  canvas.width = bitmap.width;
  canvas.height = bitmap.height;
  canvas.getContext("2d")!.drawImage(bitmap, 0, 0);
  bitmap.close();
}

async function main() {
  blockContextMenu();
  const view = await screen.view();
  if (!view) {
    await cancel();
    return;
  }
  imageSize = { width: view.width, height: view.height };
  dictateHint(view.dictate);

  const stageEl = stage();
  stageEl.addEventListener("pointerdown", onPointerDown);
  stageEl.addEventListener("pointermove", onPointerMove);
  stageEl.addEventListener("pointerup", onPointerUp);
  stageEl.addEventListener("pointercancel", onPointerUp);
  document.addEventListener("keydown", onKey);
  window.addEventListener("resize", onResize);
  $("close").addEventListener("click", () => void cancel());

  const field = promptEl();
  field.addEventListener("input", () => {
    autosize();
    updateSend();
  });
  askEl().addEventListener("submit", (e) => {
    e.preventDefault();
    void submit();
  });

  const micButton = $("mic");
  // O foco fica na caixa de texto: é lá que a pergunta aparece.
  micButton.addEventListener("mousedown", (e) => e.preventDefault());
  micButton.addEventListener("click", () => {
    if (!box) selectAll();
    // O mesmo clique da pílula: começa a ditar, e o seguinte pára. Com esta janela aberta, o
    // texto vem para a caixa de pergunta em vez de ser colado numa app.
    void api.overlayClick().catch((e) => say(errorText(e), true));
    field.focus({ preventScroll: true });
  });

  await Promise.all([
    onMicState((state) => {
      mic = state;
      // A pergunta começou a ser dita sem área escolhida: o ecrã todo, para o texto aparecer.
      if (state === "recording" && !box) selectAll();
      renderMic();
    }),
    onMicFailure((label) => (micFailure = label)),
    onMicLevel((level) => askEl().style.setProperty("--level", String(clamp(level, 0, 1)))),
    onScreenPartial((text) => {
      const live = $("live");
      live.textContent = text;
      live.hidden = text === "";
      layoutAsk();
    }),
    onScreenTranscript(onTranscript),
  ]);

  try {
    await drawShot();
  } catch (e) {
    console.error("não foi possível desenhar a captura", e);
    await screen.report(`não foi possível desenhar a captura: ${errorText(e)}`).catch(() => {});
    await cancel();
    return;
  }
  // Só agora a janela aparece: sem um relâmpago preto antes do ecrã "congelado". Não se espera
  // por uma frame: com a janela ainda escondida o WebView pode não as desenhar.
  await screen.ready();
}

void main();
