// Formatação partilhada pelas páginas da janela principal.

export const isMac = navigator.userAgent.includes("Mac");

/** Valores pequenos em decimal com 2 algarismos significativos ($0.000087), nunca em notação científica. */
export const usd = (v: number | null | undefined, digits = 4) => {
  if (v == null) return "—";
  if (v === 0) return "$0";
  if (Math.abs(v) >= 1) return `$${v.toFixed(2)}`;
  if (Math.abs(v) >= 0.01) return `$${v.toFixed(digits)}`;
  return `$${v.toLocaleString("en-US", { maximumSignificantDigits: 2, maximumFractionDigits: 12 })}`;
};

export const ms = (v: number | null | undefined) =>
  v == null ? "—" : v >= 1000 ? `${(v / 1000).toFixed(2)} s` : `${v} ms`;

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
