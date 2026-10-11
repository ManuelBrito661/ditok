// Guia de primeira execução: três passos num <dialog> por cima da janela.
//   1. chave do OpenRouter (validada e guardada com o mesmo comando das definições)
//   2. microfone (seletor + medidor de nível ao vivo enquanto o passo está à vista)
//   3. experimentar (o atalho atual e uma caixa onde ditas pela primeira vez)
// "Saltar" está sempre à vista. Concluir ou saltar marca `onboarding_done`, para o
// guia não voltar a aparecer.

import mascotUrl from "../design/mascot/a-ouvir.svg?inline";
import { api, errorText, onMic, type MicEvent } from "./api";
import { bindDeviceSelect, loadDevices, syncDeviceSelect } from "./devices";
import { renderKeys } from "./format";
import { openShortcutSettings } from "./settings-dialog";
import { $, app, notifyViewChange, onViewChange, save, toast } from "./store";

type Step = "key" | "mic" | "try";
const STEPS: Step[] = ["key", "mic", "try"];
/** A janela renova o medidor com esta frequência (o backend desliga-o ao fim de 6 s). */
const KEEPALIVE_MS = 2000;
/** A partir deste nível (0–1, ≈ -27 dBFS) digo que te ouvi. */
const HEARD_LEVEL = 0.55;

const dialog = () => $<HTMLDialogElement>("onboarding");
let step: Step = "key";
let keepalive: number | undefined;
let smoothed = 0;
let heard = false;

export const isOnboardingOpen = () => dialog().open;

/** Abre o guia (num passo, se pedido). */
export function openOnboarding(at: Step = app.view.api_key_set ? "mic" : "key") {
  if (!dialog().open) dialog().showModal();
  go(at);
}

function go(next: Step) {
  if (step === "mic" && next !== "mic") stopMeter();
  step = next;
  const i = STEPS.indexOf(next);
  for (const [j, s] of STEPS.entries()) {
    $(`ob-step-${s}`).hidden = s !== next;
    const li = document.querySelector<HTMLElement>(`.ob-steps [data-step="${s}"]`)!;
    li.classList.toggle("done", j < i);
    if (s === next) li.setAttribute("aria-current", "step");
    else li.removeAttribute("aria-current");
  }
  $("ob-back").hidden = i === 0;
  $("ob-next").textContent = next === "try" ? "Concluir" : "Continuar";
  if (next === "key") renderKeyStep();
  if (next === "mic") void startMeter();
  if (next === "try") renderTryStep();
  // O foco vai para o título do novo passo: quem usa teclado ou leitor de ecrã
  // fica a saber onde está, e o Tab seguinte leva ao primeiro controlo.
  $(`ob-${next}-title`).focus();
}

/** Fecha o guia e não o volta a mostrar. */
async function finish(message: string) {
  stopMeter();
  if (!app.view.config.onboarding_done) await save({ onboarding_done: true });
  dialog().close();
  toast(message);
}

// ---------------------------------------------------------------- 1. chave

function renderKeyStep() {
  const msg = $("ob-key-msg");
  if (app.view.api_key_set && !msg.textContent) {
    msg.textContent = "Já tens uma chave guardada. Podes continuar, ou colar outra para a trocar.";
    msg.className = "ob-msg ok";
  }
}

async function saveKey(): Promise<boolean> {
  const input = $<HTMLInputElement>("ob-key-input");
  const msg = $("ob-key-msg");
  const button = $<HTMLButtonElement>("ob-key-save");
  if (!input.value.trim()) {
    msg.textContent = "Cola a chave antes de continuar (ou salta este passo).";
    msg.className = "ob-msg error";
    input.setAttribute("aria-invalid", "true");
    input.focus();
    return false;
  }
  button.disabled = true;
  msg.textContent = "A validar…";
  msg.className = "ob-msg";
  try {
    const info = await api.setApiKey(input.value);
    input.value = ""; // a chave não fica no DOM
    input.removeAttribute("aria-invalid");
    app.view.api_key_set = true;
    app.key = info;
    notifyViewChange();
    msg.textContent = info ? "Chave válida e guardada." : "Guardei a chave, mas não a consegui validar (sem rede?).";
    msg.className = "ob-msg ok";
    return true;
  } catch (e) {
    msg.textContent = errorText(e);
    msg.className = "ob-msg error";
    input.setAttribute("aria-invalid", "true");
    input.focus();
    return false;
  } finally {
    button.disabled = false;
  }
}

// ---------------------------------------------------------------- 2. microfone

async function startMeter() {
  heard = false;
  smoothed = 0;
  renderLevel(0);
  const msg = $("ob-mic-msg");
  msg.textContent = "A abrir o microfone…";
  msg.className = "ob-msg";
  const devices = await loadDevices($<HTMLSelectElement>("ob-device"));
  if (step !== "mic") return; // saíste do passo enquanto a lista carregava
  if (!devices.length) {
    msg.textContent = "Não encontrei nenhum microfone. Liga um e volta a este passo.";
    msg.className = "ob-msg error";
  }
  void api.micMonitor(true).catch(() => {});
  window.clearInterval(keepalive);
  keepalive = window.setInterval(() => void api.micMonitor(true).catch(() => {}), KEEPALIVE_MS);
}

function stopMeter() {
  if (keepalive === undefined) return;
  window.clearInterval(keepalive);
  keepalive = undefined;
  void api.micMonitor(false).catch(() => {});
  renderLevel(0);
}

function renderLevel(level: number) {
  $("ob-meter-bar").style.width = `${Math.round(level * 100)}%`;
}

function onMicEvent(e: MicEvent) {
  if (step !== "mic" || keepalive === undefined) return;
  const msg = $("ob-mic-msg");
  if (e.kind === "level") {
    // Sobe depressa e desce devagar, como na barra flutuante.
    smoothed = e.value > smoothed ? e.value : smoothed * 0.7 + e.value * 0.3;
    renderLevel(smoothed);
    if (!heard && smoothed >= HEARD_LEVEL) {
      heard = true;
      msg.textContent = "Estou a ouvir-te bem.";
      msg.className = "ob-msg ok";
    }
  } else if (e.kind === "opened") {
    if (!heard) {
      msg.textContent = `A ouvir: ${e.value}. Fala um pouco…`;
      msg.className = "ob-msg";
    }
  } else {
    msg.textContent = e.value;
    msg.className = "ob-msg error";
  }
}

// ---------------------------------------------------------------- 3. experimentar

function renderTryStep() {
  renderKeys($("ob-keys"), app.view.config.shortcut);
  $("ob-shortcut-busy").hidden = app.view.shortcut_ok;
}

async function onTried(e: Event) {
  // Só conta o texto colado (é assim que o Ditok escreve), não o que escreves à mão.
  if ((e as InputEvent).inputType !== "insertFromPaste") return;
  if (!$<HTMLTextAreaElement>("ob-try").value.trim()) return;
  const msg = $("ob-try-msg");
  msg.textContent = "Funcionou! A partir de agora é igual em qualquer app.";
  msg.className = "ob-msg ok";
  if (!app.view.config.onboarding_done) await save({ onboarding_done: true });
}

// ---------------------------------------------------------------- arranque

export function setupOnboarding() {
  $<HTMLImageElement>("ob-mascot").src = mascotUrl;

  $("ob-skip").addEventListener("click", () => void finish("Guia saltado. A chave e o microfone estão nas Definições."));
  $("ob-back").addEventListener("click", () => go(STEPS[Math.max(0, STEPS.indexOf(step) - 1)]));
  $("ob-next").addEventListener("click", async () => {
    if (step === "key") {
      const input = $<HTMLInputElement>("ob-key-input");
      // Com uma chave colada, "Continuar" também a valida e guarda.
      if ((input.value.trim() || !app.view.api_key_set) && !(await saveKey())) return;
      go("mic");
    } else if (step === "mic") {
      go("try");
    } else {
      void finish("Tudo pronto. Bons ditados!");
    }
  });

  $("ob-key-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    if (await saveKey()) go("mic");
  });
  $("ob-key-input").addEventListener("input", () => $("ob-key-input").removeAttribute("aria-invalid"));
  $("ob-key-link").addEventListener("click", () =>
    void api.openOpenRouterKeys().catch((e) => toast(`Não consegui abrir o browser: ${errorText(e)}`, "error")));

  bindDeviceSelect($<HTMLSelectElement>("ob-device"), () => {
    // Outro microfone: o medidor reabre com ele.
    if (step === "mic" && keepalive !== undefined) {
      heard = false;
      void api.micMonitor(true).catch(() => {});
    }
  });
  void onMic(onMicEvent);

  $("ob-try").addEventListener("input", (e) => void onTried(e));
  $("ob-shortcut-change").addEventListener("click", () => openShortcutSettings());

  // Esc não fecha o guia por engano: para sair há o "Saltar", sempre à vista.
  dialog().addEventListener("cancel", (e) => e.preventDefault());
  dialog().addEventListener("close", stopMeter);

  onViewChange(() => {
    if (!dialog().open) return;
    syncDeviceSelect($<HTMLSelectElement>("ob-device"));
    if (step === "try") renderTryStep();
  });
}
