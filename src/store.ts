// Estado partilhado da janela principal: as definições vindas do backend, o
// "toast", a confirmação de ações destrutivas e uma lista de quem quer saber quando as definições mudam (Início,
// modal de definições). Assim cada página não precisa de conhecer as outras.

import { api, errorText, type Config, type KeyInfo, type SettingsView } from "./api";

export const app = {
  /** Preenchido em `main()` antes de qualquer página ser desenhada. */
  view: undefined as unknown as SettingsView,
  /** Gasto e limite da chave do OpenRouter; `null` sem chave ou sem rede. */
  key: null as KeyInfo | null,
};

/** Volta a pedir ao OpenRouter o que resta na chave (no arranque e depois de cada ditado). */
export async function refreshKey() {
  app.key = app.view.api_key_set ? await api.testApiKey().catch(() => null) : null;
  notifyViewChange();
}

const listeners: Array<() => void> = [];

/** Regista uma função a chamar sempre que `app.view` muda. */
export function onViewChange(fn: () => void) {
  listeners.push(fn);
}

export function notifyViewChange() {
  for (const fn of listeners) fn();
}

/** Um botão dentro do aviso, como o "Desfazer" depois de apagar. */
export interface ToastAction {
  label: string;
  run: () => void;
}

let toastTimer: number | undefined;
let toastDelay = 2000;
let toastListening = false;

function scheduleToastHide() {
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (document.getElementById("toast")!.hidden = true), toastDelay);
}

/** Mostra um aviso em baixo. Com `action`, fica 5 s (mais, enquanto o rato ou o
 *  foco estiverem nele) e leva um botão; devolve-o para quem quiser dar-lhe o foco. */
export function toast(message: string, kind: "ok" | "error" = "ok", action?: ToastAction): HTMLButtonElement | null {
  const el = document.getElementById("toast")!;
  // Um <dialog> aberto fica por cima de tudo: o aviso tem de ir para dentro dele.
  const host = document.querySelector("dialog[open]") ?? document.body;
  if (el.parentElement !== host) host.append(el);
  el.className = kind + (action ? " has-action" : "");
  el.hidden = false;
  toastDelay = action ? 5000 : kind === "error" ? 6000 : 2000;
  if (!toastListening) {
    toastListening = true;
    // Enquanto o rato ou o foco estão no aviso, não desaparece (dá tempo de carregar no botão).
    const pause = () => window.clearTimeout(toastTimer);
    el.addEventListener("mouseenter", pause);
    // Só o foco de teclado pausa: o foco que o código dá ao botão depois de um
    // clique (sem :focus-visible) deixaria o aviso no ecrã para sempre.
    el.addEventListener("focusin", () => el.querySelector(":focus-visible") && pause());
    el.addEventListener("mouseleave", scheduleToastHide);
    el.addEventListener("focusout", scheduleToastHide);
  }

  if (!action) {
    el.textContent = message;
    scheduleToastHide();
    return null;
  }
  const text = document.createElement("span");
  text.textContent = message;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "toast-action";
  button.textContent = action.label;
  button.addEventListener("click", () => {
    window.clearTimeout(toastTimer);
    el.hidden = true;
    action.run();
  });
  el.replaceChildren(text, button);
  scheduleToastHide();
  return button;
}

/** Pede confirmação antes de uma ação destrutiva, num <dialog> com "Cancelar"
 *  (com o foco) e um botão de perigo. Esc ou clicar fora também cancelam. */
export function confirmDanger(title: string, text: string, confirmLabel: string): Promise<boolean> {
  const dlg = $<HTMLDialogElement>("confirm");
  $("confirm-title").textContent = title;
  $("confirm-text").textContent = text;
  $("confirm-ok").textContent = confirmLabel;
  dlg.returnValue = "";
  const back = document.activeElement as HTMLElement | null;
  dlg.showModal();
  return new Promise((resolve) => {
    const onClick = (e: MouseEvent) => e.target === dlg && dlg.close("cancel");
    dlg.addEventListener("click", onClick);
    dlg.addEventListener(
      "close",
      () => {
        dlg.removeEventListener("click", onClick);
        back?.focus();
        resolve(dlg.returnValue === "ok");
      },
      { once: true },
    );
  });
}

/** Aplica uma alteração parcial, grava no backend e atualiza as vistas.
 *  Só avisa quando falha: o controlo já mostra o novo valor. */
export async function save(patch: Partial<Config>): Promise<boolean> {
  try {
    app.view = await api.saveSettings({ ...app.view.config, ...patch });
    notifyViewChange();
    return true;
  } catch (e) {
    toast(errorText(e), "error");
    notifyViewChange(); // repõe os controlos com o valor que está realmente guardado
    return false;
  }
}

export const $ = <T extends HTMLElement = HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`#${id} não existe no HTML`);
  return el as T;
};
