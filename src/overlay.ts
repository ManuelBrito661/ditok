// Barra flutuante com a mascote. Reage a eventos do backend (overlay.rs):
// - "overlay:state": idle | recording | processing | done | copied | failed | nothing-heard
// - "overlay:failure": motivo curto da falha ("Sem rede"…) e se há "Tentar de novo";
//   chega logo antes do estado "failed"
// - "overlay:busy": premiste o atalho a meio do processamento (a pílula abana)
// - "overlay:level": nível do microfone entre 0 e 1, ~20 vezes por segundo
// - "overlay:latched": o ditado está em modo alternar (mostra ✕ e ■)
// - "overlay:style": o estilo deste ditado ("Formal"…) ou null; chega logo antes
//   do estado "recording" e aparece antes do tempo

// - "overlay:hover": o rato entrou/saiu da pílula (o Rust vigia o cursor)
// - "overlay:notice": falta a chave / o atalho está ocupado (aviso persistente em
//   repouso); também pedido com overlay_status quando a página carrega
// e responde com os comandos overlay_click / overlay_cancel / overlay_hit_rect /
// overlay_open (clique num aviso) / overlay_menu (clique direito).
// Não há polling nem temporizadores a correr em repouso: as animações são CSS, e
// o único temporizador (o "a demorar…") só existe enquanto se processa.

import { listen } from "@tauri-apps/api/event";
import parado from "../design/mascot/parado.svg?inline";
import aOuvir from "../design/mascot/a-ouvir.svg?inline";
import aProcessar from "../design/mascot/a-processar.svg?inline";
import pronto from "../design/mascot/pronto.svg?inline";
import { api, type OpenTarget, type Status } from "./api";
import { renderKeys } from "./format";

type State =
  | "idle"
  | "recording"
  | "processing"
  | "done"
  | "copied"
  | "failed"
  | "nothing-heard";

interface Failure {
  label: string;
  retry: boolean;
}

const IS_MAC = /Mac/i.test(navigator.userAgent);
const PASTE_KEYS = IS_MAC ? "⌘V" : "Ctrl+V";
/** Ao fim deste tempo a processar, a pílula diz "a demorar…". */
const SLOW_AFTER_MS = 5000;

const MASCOT: Record<State, string> = {
  idle: parado,
  recording: aOuvir,
  processing: aProcessar,
  done: pronto,
  copied: pronto,
  failed: parado,
  "nothing-heard": parado,
};
const LABEL: Record<State, string> = {
  idle: "",
  recording: "0:00",
  processing: "",
  done: "Colado",
  copied: `Copiado · ${PASTE_KEYS}`,
  failed: "Falhou",
  "nothing-heard": "Não ouvi nada",
};
/** O que o leitor de ecrã anuncia em cada estado (vazio = nada a dizer). */
const ANNOUNCE: Record<State, string> = {
  idle: "",
  recording: "A ouvir",
  processing: "A transcrever",
  done: "Texto colado",
  copied: `Texto copiado, cola com ${PASTE_KEYS}`,
  failed: "O ditado falhou",
  "nothing-heard": "Não ouvi nada",
};
const BAR_COUNT = 14;

/** Texto curto na pílula e dica por cima dela, para cada aviso persistente. */
const NOTICE: Record<OpenTarget, { label: string; tip: string; spoken: string }> = {
  key: {
    label: "Falta a chave",
    tip: "Falta a chave do OpenRouter · clica para a colar",
    spoken: "Falta a chave do OpenRouter. Clica na barra para a colar.",
  },
  shortcut: {
    label: "Atalho ocupado",
    tip: "Outra app está a usar o atalho · clica para escolher outro",
    spoken: "O atalho de ditado está ocupado por outra app. Clica na barra para escolher outro.",
  },
};

const pill = document.getElementById("pill")!;
const mascot = document.getElementById("mascot") as HTMLImageElement;
const label = document.getElementById("label")!;
const announce = document.getElementById("announce")!;
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
/** O estilo do ditado em curso, se não for o Predefinido (ex.: "Formal"). */
let styleTag: string | null = null;

/** O texto da pílula a ouvir: o estilo (se houver) e o tempo. */
function recordingLabel(secs: number): string {
  const time = `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
  return styleTag ? `${styleTag} · ${time}` : time;
}
/** A última falha recebida; usada quando o estado passa a "failed". */
let failure: Failure | null = null;
let slowTimer: number | undefined;
/** O que o backend diz que impede o ditado (null até a primeira resposta). */
let status: Status | null = null;

/** A chave vem primeiro: sem ela, um atalho a funcionar não serve de nada. */
function currentNotice(): OpenTarget | null {
  if (!status) return null;
  if (status.key_missing) return "key";
  if (!status.shortcut_ok) return "shortcut";
  return null;
}

/** Mostra (ou tira) o aviso persistente. Só se vê em repouso; nos outros estados
 *  o ditado tem prioridade e o aviso volta quando a pílula voltar a repousar. */
function renderNotice() {
  const notice = currentNotice();
  const before = pill.dataset.notice ?? "";
  pill.dataset.notice = notice ?? "";
  const tipNotice = document.getElementById("tip-notice")!;
  document.getElementById("tip-dictate")!.hidden = notice !== null;
  tipNotice.hidden = notice === null;
  tipNotice.textContent = notice ? NOTICE[notice].tip : "";
  // A dica normal nunca anuncia um atalho que não funciona.
  if (status && !notice) renderKeys(document.getElementById("tip-keys")!, status.shortcut);
  if (pill.dataset.state !== "idle") return;
  label.textContent = notice ? NOTICE[notice].label : "";
  if (notice && notice !== before) announce.textContent = NOTICE[notice].spoken;
}

function onStatus(s: Status) {
  status = s;
  renderNotice();
}

function setState(state: State) {
  pill.dataset.state = state;
  // O backend volta a avisar se for o caso.
  pill.classList.remove("latched", "slow", "retry", "busy");
  window.clearTimeout(slowTimer);
  mascot.src = MASCOT[state];
  label.textContent = LABEL[state];
  let spoken = ANNOUNCE[state];

  if (state === "recording") {
    recordingSince = performance.now();
    label.textContent = recordingLabel(0);
    pill.classList.toggle("styled", styleTag !== null);
    if (styleTag) spoken += `, estilo ${styleTag}`;
    history.fill(0);
    smoothed = 0;
    renderBars();
  } else if (state === "processing") {
    slowTimer = window.setTimeout(() => {
      if (pill.dataset.state !== "processing") return;
      pill.classList.add("slow");
      label.textContent = "a demorar…";
      announce.textContent = "Ainda a transcrever";
    }, SLOW_AFTER_MS);
  } else if (state === "failed" && failure) {
    label.textContent = failure.label;
    pill.classList.toggle("retry", failure.retry);
    spoken = `O ditado falhou: ${failure.label}`;
    if (failure.retry) spoken += ". Clica na barra para tentar de novo.";
  }
  if (state !== "failed") failure = null;
  announce.textContent = spoken;
  if (state === "idle") renderNotice();
}

function onFailure(f: Failure) {
  failure = f;
  // Duas falhas seguidas: o estado já é "failed", por isso atualiza já.
  if (pill.dataset.state === "failed") setState("failed");
}

function onBusy() {
  // Reinicia a animação mesmo que ainda esteja a meio de um abanão anterior.
  pill.classList.remove("busy");
  void pill.offsetWidth; // força o browser a "ver" a remoção antes de voltar a pôr
  pill.classList.add("busy");
}
pill.addEventListener("animationend", () => pill.classList.remove("busy"));

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
  label.textContent = recordingLabel(secs);
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

function onHover(inside: boolean) {
  pill.classList.toggle("hover", inside);
}

pill.addEventListener("click", (e) => {
  // Os botões tratam de si; aqui só o clique na pílula em repouso ou numa falha
  // que oferece "Tentar de novo" (o backend decide qual dos dois é).
  if ((e.target as Element).closest(".pill-button")) return;
  const state = pill.dataset.state;
  const notice = currentNotice();
  if (state === "idle" && notice) {
    // Com um aviso, o clique leva-te a resolvê-lo (ditar não funcionaria).
    void api.overlayOpen(notice);
  } else if (state === "idle" || (state === "failed" && pill.classList.contains("retry"))) {
    void api.overlayClick();
  }
});
// Clique direito: o mesmo menu do tray (abrir, colar o último ditado, sons…), em
// vez do menu do WebView. O Rust só deixa o rato chegar aqui por cima da pílula.
document.addEventListener("contextmenu", (e) => {
  e.preventDefault();
  void api.overlayMenu();
});
document.getElementById("stop")!.addEventListener("click", () => void api.overlayClick());
document.getElementById("cancel")!.addEventListener("click", () => void api.overlayCancel());

setState("idle");
reportHitRect();
void listen<State>("overlay:state", (e) => setState(e.payload));
void listen<Failure>("overlay:failure", (e) => onFailure(e.payload));
void listen("overlay:busy", () => onBusy());
void listen<number>("overlay:level", (e) => onLevel(e.payload));
void listen("overlay:latched", () => pill.classList.add("latched"));
void listen<string | null>("overlay:style", (e) => (styleTag = e.payload));
void listen<boolean>("overlay:hover", (e) => onHover(e.payload));
void listen<Status>("overlay:notice", (e) => onStatus(e.payload));
// A barra pode nascer depois do último aviso: pede o estado atual.
void api.overlayStatus().then(onStatus, () => {});
