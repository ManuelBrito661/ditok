// Estado partilhado da janela principal: as definições vindas do backend, o
// "toast" e uma lista de quem quer saber quando as definições mudam (Início,
// modal de definições). Assim cada página não precisa de conhecer as outras.

import { api, errorText, type Config, type SettingsView } from "./api";

export const app = {
  /** Preenchido em `main()` antes de qualquer página ser desenhada. */
  view: undefined as unknown as SettingsView,
};

const listeners: Array<() => void> = [];

/** Regista uma função a chamar sempre que `app.view` muda. */
export function onViewChange(fn: () => void) {
  listeners.push(fn);
}

export function notifyViewChange() {
  for (const fn of listeners) fn();
}

let toastTimer: number | undefined;
export function toast(message: string, kind: "ok" | "error" = "ok") {
  const el = document.getElementById("toast")!;
  // Um <dialog> aberto fica por cima de tudo: o aviso tem de ir para dentro dele.
  const host = document.querySelector("dialog[open]") ?? document.body;
  if (el.parentElement !== host) host.append(el);
  el.textContent = message;
  el.className = kind;
  el.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), kind === "error" ? 6000 : 2000);
}

/** Aplica uma alteração parcial, grava no backend e atualiza as vistas. */
export async function save(patch: Partial<Config>): Promise<boolean> {
  try {
    app.view = await api.saveSettings({ ...app.view.config, ...patch });
    notifyViewChange();
    toast("Guardado");
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
