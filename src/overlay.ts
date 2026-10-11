// Barra flutuante com a mascote. Só reage a eventos do backend (overlay.rs):
// - "overlay:state": idle | recording | processing | done | failed
// - "overlay:level": nível do microfone entre 0 e 1, ~20 vezes por segundo
// Não há polling nem temporizadores a correr em repouso: as animações são CSS.

import { listen } from "@tauri-apps/api/event";
import parado from "../design/mascot/parado.svg";
import aOuvir from "../design/mascot/a-ouvir.svg";
import aProcessar from "../design/mascot/a-processar.svg";
import pronto from "../design/mascot/pronto.svg";

type State = "idle" | "recording" | "processing" | "done" | "failed";

const MASCOT: Record<State, string> = {
  idle: parado,
  recording: aOuvir,
  processing: aProcessar,
  done: pronto,
  failed: parado,
};
const LABEL: Record<State, string> = {
  idle: "",
  recording: "0:00",
  processing: "",
  done: "Colado",
  failed: "Falhou",
};
const BAR_COUNT = 14;

const pill = document.getElementById("pill")!;
const mascot = document.getElementById("mascot") as HTMLImageElement;
const label = document.getElementById("label")!;
const barsEl = document.getElementById("bars")!;

// Pré-carrega as quatro poses para a troca ser instantânea.
for (const src of new Set(Object.values(MASCOT))) new Image().src = src;

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

setState("idle");
void listen<State>("overlay:state", (e) => setState(e.payload));
void listen<number>("overlay:level", (e) => onLevel(e.payload));
