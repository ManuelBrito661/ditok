// Formatação partilhada pelas páginas da janela principal.

import type { MetricsRow, Style } from "./api";

export const isMac = navigator.userAgent.includes("Mac");

const money = (options: Intl.NumberFormatOptions) =>
  new Intl.NumberFormat("pt-PT", { style: "currency", currency: "USD", ...options });
const USD_WHOLE = money({ maximumFractionDigits: 0 });
const USD_2 = money({ minimumFractionDigits: 2, maximumFractionDigits: 2 });
const USD_SMALL = money({ maximumSignificantDigits: 2 });
const usdFixed = new Map<number, Intl.NumberFormat>();

/** Dólares à portuguesa ("1,23 US$"). Valores pequenos ficam em decimal com 2
 *  algarismos significativos ("0,000087 US$"), nunca em notação científica. */
export const usd = (v: number | null | undefined, digits = 4) => {
  if (v == null) return "—";
  if (v === 0) return USD_WHOLE.format(0);
  if (Math.abs(v) >= 1) return USD_2.format(v);
  if (Math.abs(v) >= 0.01) {
    if (!usdFixed.has(digits)) usdFixed.set(digits, money({ minimumFractionDigits: digits, maximumFractionDigits: digits }));
    return usdFixed.get(digits)!.format(v);
  }
  return USD_SMALL.format(v);
};

const SECONDS = new Intl.NumberFormat("pt-PT", { minimumFractionDigits: 1, maximumFractionDigits: 1 });

/** "850 ms" ou "1,2 s". */
export const ms = (v: number | null | undefined) =>
  v == null ? "—" : v >= 1000 ? `${SECONDS.format(v / 1000)} s` : `${Math.round(v)} ms`;

/** "2026-10" → "outubro de 2026". */
export function monthLabel(key: string): string {
  const [y, m] = key.split("-").map(Number);
  if (!y || !m) return key;
  return new Date(y, m - 1, 1).toLocaleDateString("pt-PT", { month: "long", year: "numeric" });
}

/** "Bom dia", "Boa tarde" ou "Boa noite", pela hora do computador. */
export function greeting(now = new Date()): string {
  const h = now.getHours();
  return h >= 6 && h < 12 ? "Bom dia" : h >= 12 && h < 20 ? "Boa tarde" : "Boa noite";
}

export function ago(iso: string): string {
  const mins = Math.round((Date.now() - new Date(iso).getTime()) / 60000);
  if (mins < 1) return "agora mesmo";
  if (mins < 60) return `há ${mins} min`;
  const h = Math.round(mins / 60);
  return h < 48 ? `há ${h} h` : `há ${Math.round(h / 24)} dias`;
}

export const dateTime = (iso: string) =>
  new Date(iso).toLocaleString("pt-PT", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" });

export const timeOfDay = (iso: string) =>
  new Date(iso).toLocaleTimeString("pt-PT", { hour: "2-digit", minute: "2-digit" });

/** "Hoje", "Ontem" ou "3 de outubro" (com o ano só se não for o atual). */
export function dayLabel(d: Date): string {
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((start(new Date()) - start(d)) / 86_400_000);
  if (days === 0) return "Hoje";
  if (days === 1) return "Ontem";
  const sameYear = d.getFullYear() === new Date().getFullYear();
  return d.toLocaleDateString("pt-PT", { day: "numeric", month: "long", ...(sameYear ? {} : { year: "numeric" }) });
}

export const countWords = (text: string) => text.split(/\s+/).filter(Boolean).length;

/** Para pesquisar sem distinguir maiúsculas nem acentos: "Ação" → "acao". O NFD
 *  separa cada letra do acento (combinado, U+0300–U+036F), que depois se tira. */
export const foldText = (text: string) => text.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

const KEY_NAMES: Record<string, string> = {
  Space: "Espaço", Enter: "Enter", Backquote: "`", Minus: "-", Equal: "=", Comma: ",", Period: ".",
  Slash: "/", Backslash: "\\", Semicolon: ";", Quote: "'", BracketLeft: "[", BracketRight: "]",
  ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
};

/** "CommandOrControl+Shift+Space" → ["Ctrl", "Shift", "Espaço"] (ou ⌘ no Mac). */
export function shortcutKeys(s: string): string[] {
  return s
    .split("+")
    .filter(Boolean)
    .map((part) => {
      switch (part.toLowerCase()) {
        case "commandorcontrol": case "cmdorctrl": case "commandorctrl": case "cmdorcontrol":
          return isMac ? "⌘" : "Ctrl";
        case "control": case "ctrl": return "Ctrl";
        case "super": case "command": case "cmd": case "meta": return isMac ? "⌘" : "Win";
        case "alt": case "option": return isMac ? "⌥" : "Alt";
        case "shift": return "Shift";
      }
      if (part.startsWith("Key")) return part.slice(3);
      if (part.startsWith("Digit")) return part.slice(5);
      return KEY_NAMES[part] ?? part;
    });
}

export const prettyShortcut = (s: string) => shortcutKeys(s).join(" + ");

/** Preenche um elemento com uma tecla <kbd> por parte do atalho, como no Flow. */
export function renderKeys(el: HTMLElement, shortcut: string) {
  el.replaceChildren(
    ...shortcutKeys(shortcut).map((k) => {
      const kbd = document.createElement("kbd");
      kbd.textContent = k;
      return kbd;
    }),
  );
}

/** Converte uma tecla premida no formato que o backend (global-hotkey) entende. */
export function shortcutFromEvent(e: KeyboardEvent): string | null {
  if (/^(Control|Shift|Alt|Meta)(Left|Right)?$/.test(e.code)) return null; // ainda só modificadores
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  return [...mods, e.code].join("+");
}

/** Gasto em relação ao orçamento, como classe CSS: "warn" a partir de 80 % e
 *  "over" a partir de 100 % (os mesmos limites do aviso em metrics.rs). */
export function budgetLevel(spent: number, budget: number | null): "" | "warn" | "over" {
  if (!budget) return "";
  return spent >= budget ? "over" : spent >= budget * 0.8 ? "warn" : "";
}

/** Custo por minuto de áudio nos últimos registos com `model`, ou `null` se não
 *  houver nenhum. `withCleanup`: soma o custo da limpeza (os ficheiros não a têm). */
export function costPerMinute(rows: MetricsRow[], model: string, withCleanup: boolean): number | null {
  let cost = 0;
  let audioMs = 0;
  for (const r of rows) {
    if (r.model !== model || r.cost_usd == null) continue;
    cost += r.cost_usd + (withCleanup ? r.cleanup_cost_usd ?? 0 : 0);
    audioMs += r.audio_ms;
  }
  return audioMs > 0 && cost > 0 ? (cost / audioMs) * 60_000 : null;
}

/** "4 min 21 s", "62 min", "1 h 05 min". */
export function duration(seconds: number): string {
  const s = Math.round(seconds);
  if (s < 60) return `${s} s`;
  if (s < 600) return `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, "0")} s`;
  const m = Math.round(s / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} min`;
}

export const megabytes = (bytes: number) =>
  `${(bytes / 1_048_576).toLocaleString("pt-PT", { maximumFractionDigits: bytes < 10_485_760 ? 1 : 0 })} MB`;

/** O nome de cada estilo na UI (config.rs, Style::label). */
export const STYLE_LABEL: Record<Style, string> = {
  default: "Predefinido",
  casual: "Casual",
  formal: "Formal",
  literal: "Literal",
};
