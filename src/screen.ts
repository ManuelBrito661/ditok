// A seleção da captura do ecrã: mostra a fotografia "congelada" só enquanto escolhes a área com
// o rato (ou o ecrã todo com Enter ou um clique). Ao largar o rato, a área segue anexada para o
// popup do assistente, onde se escreve (ou dita) a pergunta, e esta janela fecha: o ecrã deixa
// de estar congelado. O backend (src-tauri/src/screen/) guarda a fotografia e faz o recorte:
// esta página só desenha e diz que área.
//
// A janela cobre o monitor inteiro e está por cima de tudo, por isso tem de se poder fechar de
// várias maneiras: Esc, o ✕, ou perder o foco (o backend trata disso).

import { errorText, screen } from "./api";
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

type Mode = "idle" | "selecting" | "sending";

let mode: Mode = "idle";
let box: Box | null = null;
/** Onde o arrasto começou (`null` sem botão premido). */
let origin: { x: number; y: number } | null = null;
let imageSize = { width: 1, height: 1 };
let closing = false;

const stage = () => $("stage");
const selEl = () => $("sel");

const clamp = (v: number, lo: number, hi: number) => Math.min(Math.max(v, lo), hi);

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

/** Desenha a área e o tamanho que segue para o modelo. */
function paint() {
  const sel = selEl();
  sel.hidden = !box;
  if (!box) return;
  Object.assign(sel.style, { left: `${box.x}px`, top: `${box.y}px`, width: `${box.w}px`, height: `${box.h}px` });
  // O ecrã todo escolhido: sem dimming (não há nada a escurecer).
  sel.toggleAttribute("data-full", box.w >= window.innerWidth - 1 && box.h >= window.innerHeight - 1);
  // A etiqueta passa para dentro da área quando não cabe por cima.
  sel.toggleAttribute("data-top", box.y < 32);
  // Em píxeis da fotografia (não da janela).
  const rx = imageSize.width / window.innerWidth;
  const ry = imageSize.height / window.innerHeight;
  $("size").textContent = `${Math.round(box.w * rx)} × ${Math.round(box.h * ry)}`;
}

function say(text: string, isError = false) {
  const status = $("status");
  status.textContent = text;
  status.hidden = text === "";
  status.classList.toggle("error", isError);
}

/** Anexa a área ao assistente. A janela fecha-se sozinha; se falhar, fica aberta com o motivo
 *  e dá para escolher outra vez. */
async function send() {
  if (!box || mode === "sending" || closing) return;
  setMode("sending");
  paint();
  say("A preparar a captura…");
  try {
    await screen.submit(
      { x: box.x, y: box.y, width: box.w, height: box.h },
      { width: window.innerWidth, height: window.innerHeight },
    );
  } catch (e) {
    setMode("idle");
    say(errorText(e), true);
  }
}

function selectAll() {
  box = { x: 0, y: 0, w: window.innerWidth, h: window.innerHeight };
  void send();
}

// ---------------------------------------------------------------- rato

function onPointerDown(e: PointerEvent) {
  if (e.button !== 0 || mode === "sending") return;
  if ((e.target as HTMLElement).closest("#close")) return;
  stage().setPointerCapture(e.pointerId);
  origin = { x: e.clientX, y: e.clientY };
  say("");
}

function onPointerMove(e: PointerEvent) {
  if (!origin) return;
  // Só passa a ser uma seleção quando o rato se afasta um pouco: um clique não pisca.
  if (mode !== "selecting" && Math.hypot(e.clientX - origin.x, e.clientY - origin.y) < 3) return;
  setMode("selecting");
  box = fromCorners(origin.x, origin.y, e.clientX, e.clientY);
  paint();
}

function onPointerUp(e: PointerEvent) {
  if (!origin) return;
  origin = null;
  if (stage().hasPointerCapture(e.pointerId)) stage().releasePointerCapture(e.pointerId);
  // Um clique, ou um arrasto minúsculo: o ecrã todo.
  if (!box || mode !== "selecting" || box.w < MIN || box.h < MIN) return selectAll();
  void send();
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
  // Uma tecla do atalho ainda em baixo (a repetir) quando a janela abre não conta.
  if (e.isComposing || e.repeat || mode === "sending") return;
  if (e.key === "Enter" && !(e.target as HTMLElement).closest("button")) {
    e.preventDefault();
    selectAll();
  }
}

/** Tira o menu do WebView (Anterior, Atualizar, Imprimir…). Fica em `tauri dev`. */
function blockContextMenu() {
  if (import.meta.env.DEV) return;
  document.addEventListener("contextmenu", (e) => e.preventDefault());
}

// ---------------------------------------------------------------- arranque

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

  const stageEl = stage();
  stageEl.addEventListener("pointerdown", onPointerDown);
  stageEl.addEventListener("pointermove", onPointerMove);
  stageEl.addEventListener("pointerup", onPointerUp);
  stageEl.addEventListener("pointercancel", onPointerUp);
  document.addEventListener("keydown", onKey);
  $("close").addEventListener("click", () => void cancel());

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
