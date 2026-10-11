// Barra flutuante com a mascote. Reage a eventos do backend (overlay.rs):
// - "overlay:state": idle | recording | processing | done | failed | nothing-heard
// - "overlay:level": nível do microfone entre 0 e 1, ~20 vezes por segundo
// - "overlay:latched": o ditado está em modo alternar (mostra ✕ e ■)
// - "overlay:hover": o rato entrou/saiu da pílula (o Rust vigia o cursor)
// e responde com os comandos overlay_click / overlay_cancel / overlay_hit_rect.
// Não há polling nem temporizadores a correr em repouso: as animações são CSS.

import { listen } from "@tauri-apps/api/event";
import parado from "../design/mascot/parado.svg?inline";
import aOuvir from "../design/mascot/a-ouvir.svg?inline";
import aProcessar from "../design/mascot/a-processar.svg?inline";
import pronto from "../design/mascot/pronto.svg?inline";
import { api } from "./api";
import { renderKeys } from "./format";

type State = "idle" | "recording" | "processing" | "done" | "failed" | "nothing-heard";

const MASCOT: Record<State, string> = {
  idle: parado,
  recording: aOuvir,
  processing: aProcessar,
  done: pronto,
  failed: parado,
  "nothing-heard": parado,
};
const LABEL: Record<State, string> = {
  idle: "",
  recording: "0:00",
  processing: "",
  done: "Colado",
  failed: "Falhou",
  "nothing-heard": "Não ouvi nada",
};
const BAR_COUNT = 14;

const pill = document.getElementById("pill")!;
const mascot = document.getElementById("mascot") as HTMLImageElement;
const label = document.getElementById("label")!;
const barsEl = document.getElementById("bars")!;

// As poses vêm embutidas (?inline, data URI): a troca não depende da rede, o que
// importa no "a processar", que dura menos de um segundo. Pré-descodifica-as também.
const preloaded = [...new Set(Object.values(MASCOT))].map((src) => {
  const img = new Image();
  img.src = src;
  void img.decode().catch(() => {});
  return img; // manter a referência evita que o browser as descarte
});
void preloaded;

// Barras da forma de onda: as mais recentes à direita, a deslizar para a esquerda.
const bars = Array.from({ length: BAR_COUNT }, () => {
  const bar = document.createElement("span");
  barsEl.append(bar);
  return bar;
});
const history: number[] = new Array(BAR_COUNT).fill(0);
let smoothed = 0;
let recordingSince = 0;

function setState(state: State) {
  pill.dataset.state = state;
  pill.classList.remove("latched"); // o backend volta a avisar se for o caso
  mascot.src = MASCOT[state];
  label.textContent = LABEL[state];
  if (state === "recording") {
    recordingSince = performance.now();
    history.fill(0);
    smoothed = 0;
    renderBars();
  }
}

function renderBars() {
  bars.forEach((bar, i) => {
    // Mínimo de 12% para as barras nunca desaparecerem por completo no silêncio.
    bar.style.transform = `scaleY(${0.12 + history[i] * 0.88})`;
  });
}

function onLevel(level: number) {
  if (pill.dataset.state !== "recording") return;
  // Sobe depressa e desce devagar, como um VU-meter: a fala fica mais legível.
  smoothed = level > smoothed ? level : smoothed * 0.6 + level * 0.4;
  history.shift();
  history.push(smoothed);
  renderBars();

  const secs = Math.floor((performance.now() - recordingSince) / 1000);
  label.textContent = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
  // Aviso visual perto do limite de 60 s de processamento do OpenRouter.
  pill.classList.toggle("long", secs >= 45);
}

// ---------------------------------------------------------------- rato

/** Diz ao Rust onde está a pílula, uma vez por frame no máximo (ela anima de tamanho). */
let rectQueued = false;
function reportHitRect() {
  if (rectQueued) return;
  rectQueued = true;
  requestAnimationFrame(() => {
    rectQueued = false;
    const r = pill.getBoundingClientRect();
    void api.overlayHitRect({ x: r.x, y: r.y, width: r.width, height: r.height }).catch(() => {});
  });
}
new ResizeObserver(reportHitRect).observe(pill);

async function onHover(inside: boolean) {
  pill.classList.toggle("hover", inside);
  if (inside && pill.dataset.state === "idle") {
    // Lido a cada entrada: o atalho pode ter mudado nas definições.
    const view = await api.getSettings().catch(() => null);
    if (view) renderKeys(document.getElementById("tip-keys")!, view.config.shortcut);
  }
}

pill.addEventListener("click", (e) => {
  // Os botões tratam de si; aqui só o clique na pílula em repouso.
  if ((e.target as Element).closest(".pill-button")) return;
  if (pill.dataset.state === "idle") void api.overlayClick();
});
document.getElementById("stop")!.addEventListener("click", () => void api.overlayClick());
document.getElementById("cancel")!.addEventListener("click", () => void api.overlayCancel());

setState("idle");
reportHitRect();
void listen<State>("overlay:state", (e) => setState(e.payload));
void listen<number>("overlay:level", (e) => onLevel(e.payload));
void listen("overlay:latched", () => pill.classList.add("latched"));
void listen<boolean>("overlay:hover", (e) => void onHover(e.payload));
