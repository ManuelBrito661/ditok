// Janela de definições do Ditok. Sem framework: cada secção lê o estado do backend
// via `api` e grava cada alteração logo que acontece (sem botão "Guardar" global).

import mascotUrl from "../design/mascot/parado.svg?inline";
import listeningMascotUrl from "../design/mascot/a-ouvir.svg?inline";
import thinkingMascotUrl from "../design/mascot/a-processar.svg?inline";
import readyMascotUrl from "../design/mascot/pronto.svg?inline";
import {
  api,
  errorText,
  onDictationRecorded,
  type Config,
  type KeyInfo,
  type ModelInfo,
  type ModelKind,
  type ModelList,
  type SettingsView,
} from "./api";

const $ = <T extends HTMLElement = HTMLElement>(id: string): T => {
  const el = document.getElementById(id);
  if (!el) throw new Error(`#${id} não existe no HTML`);
  return el as T;
};

let view: SettingsView;
const isMac = navigator.userAgent.includes("Mac");

// ---------------------------------------------------------------- utilitários

let toastTimer: number | undefined;
function toast(message: string, kind: "ok" | "error" = "ok") {
  const el = $("toast");
  el.textContent = message;
  el.className = kind;
  el.hidden = false;
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), kind === "error" ? 6000 : 2000);
}

/** Aplica uma alteração parcial, grava no backend e atualiza a vista. */
async function save(patch: Partial<Config>): Promise<boolean> {
  try {
    view = await api.saveSettings({ ...view.config, ...patch });
    renderConfig();
    toast("Guardado");
    return true;
  } catch (e) {
    toast(errorText(e), "error");
    renderConfig(); // repõe os controlos com o valor que está realmente guardado
    return false;
  }
}

/** Valores pequenos em decimal com 2 algarismos significativos ($0.000087), nunca em notação científica. */
const usd = (v: number | null | undefined, digits = 4) => {
  if (v == null) return "—";
  if (v === 0) return "$0";
  if (Math.abs(v) >= 1) return `$${v.toFixed(2)}`;
  if (Math.abs(v) >= 0.01) return `$${v.toFixed(digits)}`;
  return `$${v.toLocaleString("en-US", { maximumSignificantDigits: 2, maximumFractionDigits: 12 })}`;
};
const ms = (v: number | null | undefined) => (v == null ? "—" : v >= 1000 ? `${(v / 1000).toFixed(2)} s` : `${v} ms`);

function ago(iso: string): string {
  const mins = Math.round((Date.now() - new Date(iso).getTime()) / 60000);
  if (mins < 1) return "agora mesmo";
  if (mins < 60) return `há ${mins} min`;
  const h = Math.round(mins / 60);
  return h < 48 ? `há ${h} h` : `há ${Math.round(h / 24)} dias`;
}

const dateTime = (iso: string) =>
  new Date(iso).toLocaleString("pt-PT", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" });

// ---------------------------------------------------------------- atalho

const KEY_NAMES: Record<string, string> = {
  Space: "Espaço", Enter: "Enter", Backquote: "`", Minus: "-", Equal: "=", Comma: ",", Period: ".",
  Slash: "/", Backslash: "\\", Semicolon: ";", Quote: "'", BracketLeft: "[", BracketRight: "]",
  ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
};

/** "CommandOrControl+Shift+Space" → "Ctrl + Shift + Espaço" (ou ⌘ no Mac). */
function prettyShortcut(s: string): string {
  return s
    .split("+")
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
    })
    .join(" + ");
}

/** Converte uma tecla premida no formato que o backend (global-hotkey) entende. */
function shortcutFromEvent(e: KeyboardEvent): string | null {
  if (/^(Control|Shift|Alt|Meta)(Left|Right)?$/.test(e.code)) return null; // ainda só modificadores
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  return [...mods, e.code].join("+");
}

function setupShortcut() {
  const button = $<HTMLButtonElement>("shortcut-record");
  const display = $("shortcut-display");
  const error = $("shortcut-error");
  let recording = false;

  const stop = () => {
    recording = false;
    button.textContent = "Mudar";
    display.classList.remove("recording");
    display.textContent = prettyShortcut(view.config.shortcut);
    window.removeEventListener("keydown", onKey, true);
  };

  const onKey = async (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) return stop();
    const shortcut = shortcutFromEvent(e);
    if (!shortcut) {
      display.textContent = prettyShortcut([e.ctrlKey && "Control", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean).join("+")) + " + …";
      return;
    }
    stop();
    error.hidden = true;
    if (!(await save({ shortcut }))) {
      // O backend explica porquê (conflito com outra app, tecla sem modificador…).
      error.textContent = $("toast").textContent ?? "";
      error.hidden = false;
    }
  };

  button.addEventListener("click", () => {
    if (recording) return stop();
    recording = true;
    error.hidden = true;
    button.textContent = "Cancelar";
    display.classList.add("recording");
    display.textContent = "Prime a nova combinação…";
    window.addEventListener("keydown", onKey, true);
  });

  $("shortcut-reset").addEventListener("click", async () => {
    error.hidden = true;
    await save({ shortcut: view.defaults.shortcut });
  });
}

// ---------------------------------------------------------------- chave

function describeKey(info: KeyInfo | null): string {
  if (!info) return "Guardada, mas não foi possível validar (sem rede?).";
  const parts = [];
  if (info.label) parts.push(info.label);
  if (info.usage != null) parts.push(`gasto ${usd(info.usage, 2)}`);
  if (info.limit_remaining != null) parts.push(`restam ${usd(info.limit_remaining, 2)}`);
  else if (info.limit == null) parts.push("sem limite de gasto");
  return parts.join(" · ");
}

function renderKey() {
  const status = $("key-status");
  status.textContent = view.api_key_set ? "Configurada" : "Não configurada";
  status.className = `pill ${view.api_key_set ? "ok" : "warn"}`;
  $<HTMLButtonElement>("key-test").disabled = !view.api_key_set;
  $<HTMLButtonElement>("key-delete").disabled = !view.api_key_set;
  // A barra lateral mostra se o ditado está pronto a usar.
  $("status-dot").className = `dot ${view.api_key_set ? "ok" : "warn"}`;
  $("status-text").textContent = view.api_key_set ? "Pronto a ditar" : "Falta a chave do OpenRouter";
}

function setupKey() {
  const input = $<HTMLInputElement>("key-input");
  const info = $("key-info");

  $("key-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    try {
      const result = await api.setApiKey(input.value);
      input.value = ""; // a chave não fica no DOM
      view.api_key_set = true;
      renderKey();
      info.textContent = describeKey(result);
      toast("Chave guardada");
    } catch (err) {
      toast(errorText(err), "error");
    }
  });

  $("key-test").addEventListener("click", async () => {
    info.textContent = "A testar…";
    try {
      info.textContent = `Válida · ${describeKey(await api.testApiKey())}`;
    } catch (err) {
      info.textContent = "";
      toast(errorText(err), "error");
    }
  });

  $("key-delete").addEventListener("click", async () => {
    if (!confirm("Apagar a chave do OpenRouter do cofre do sistema? O ditado deixa de funcionar até guardares outra.")) return;
    try {
      await api.deleteApiKey();
      view.api_key_set = false;
      info.textContent = "";
      renderKey();
      toast("Chave apagada");
    } catch (err) {
      toast(errorText(err), "error");
    }
  });
}

// ---------------------------------------------------------------- modelos

function price(m: ModelInfo, kind: ModelKind): string {
  if (m.prompt_price == null) return "preço variável";
  // Transcrição cobrada à duração: "prompt" é USD por segundo de áudio.
  if (kind === "transcription" && !m.completion_price) return `${usd(m.prompt_price * 60, 5)}/min`;
  const perM = (v: number | null) => (v == null ? "?" : `$${(v * 1e6).toFixed(2)}`);
  return `${perM(m.prompt_price)} / ${perM(m.completion_price)} por M tokens`;
}

/** Um seletor de modelos com pesquisa (transcrição ou limpeza). */
function modelPicker(kind: ModelKind, prefix: "stt" | "cleanup", field: "stt_model" | "cleanup_model") {
  const list = $<HTMLUListElement>(`${prefix}-models`);
  const search = $<HTMLInputElement>(`${prefix}-search`);
  const meta = $(`${prefix}-meta`);
  let data: ModelList | null = null;
  const MAX_SHOWN = 80;

  const render = () => {
    $(`${prefix}-current`).textContent = `Atual: ${view.config[field]}`;
    if (!data) return;
    const q = search.value.trim().toLowerCase();
    const matches = data.models.filter((m) => !q || m.id.toLowerCase().includes(q) || m.name.toLowerCase().includes(q));
    // O modelo selecionado aparece sempre primeiro.
    matches.sort((a, b) => Number(b.id === view.config[field]) - Number(a.id === view.config[field]));
    list.replaceChildren(
      ...matches.slice(0, MAX_SHOWN).map((m) => {
        const li = document.createElement("li");
        const selected = m.id === view.config[field];
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(selected));
        li.tabIndex = 0;
        li.innerHTML = `<span class="model-name"></span><code class="model-id"></code><span class="model-price"></span>`;
        (li.children[0] as HTMLElement).textContent = m.name;
        (li.children[1] as HTMLElement).textContent = m.id;
        (li.children[2] as HTMLElement).textContent = price(m, kind);
        const choose = () => !selected && save({ [field]: m.id } as Partial<Config>).then(render);
        li.addEventListener("click", choose);
        li.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), choose()));
        return li;
      }),
    );
    const more = matches.length > MAX_SHOWN ? ` (a mostrar ${MAX_SHOWN}; pesquisa para filtrar)` : "";
    meta.textContent =
      `${matches.length} de ${data.models.length} modelos · lista de ${ago(data.fetched_at)}` +
      (data.stale ? " · sem ligação, a usar a cópia guardada" : "") + more;
  };

  const load = async (refresh: boolean) => {
    meta.textContent = "A carregar modelos…";
    try {
      data = await api.listModels(kind, refresh);
      render();
    } catch (e) {
      meta.textContent = `Não foi possível obter a lista: ${errorText(e)}`;
    }
  };

  search.addEventListener("input", render);
  $(`${prefix}-refresh`).addEventListener("click", () => load(true));
  return { load, render };
}

// ---------------------------------------------------------------- histórico e métricas

async function renderHistory() {
  const entries = await api.getHistory();
  const list = $("history-list");
  $("history-empty").hidden = entries.length > 0;
  list.replaceChildren(
    ...entries.map((h) => {
      const li = document.createElement("li");
      const meta = document.createElement("span");
      meta.className = "muted";
      meta.textContent = `${dateTime(h.at)} · ${ms(h.total_ms)} · ${h.model}`;
      const text = document.createElement("p");
      text.textContent = h.text; // textContent: o texto ditado nunca é interpretado como HTML
      li.append(meta, text);
      if (h.raw_text) {
        const raw = document.createElement("details");
        raw.innerHTML = "<summary>Antes da limpeza</summary>";
        const p = document.createElement("p");
        p.textContent = h.raw_text;
        raw.append(p);
        li.append(raw);
      }
      return li;
    }),
  );
}

async function renderStats() {
  const s = await api.getStats();
  const month = s.months[s.current_month];
  $("m-cost").textContent = usd(month?.cost_usd ?? 0);
  $("m-count").textContent = String(month?.dictations ?? 0);
  const pair = (p: typeof s.total) => (p ? `${ms(p.p50)} / ${ms(p.p90)}` : "—");
  $("m-total").textContent = pair(s.total);
  $("m-stt").textContent = pair(s.stt);
  $("m-cleanup").textContent = pair(s.cleanup);

  $("recent").replaceChildren(
    ...s.recent.map((r) => {
      const tr = document.createElement("tr");
      const cost = (r.cost_usd ?? 0) + (r.cleanup_cost_usd ?? 0);
      const cells = [dateTime(r.at), r.model, ms(r.audio_ms), ms(r.stt_ms), ms(r.cleanup_ms),
        r.error ? "erro" : ms(r.total_ms), usd(r.cost_usd == null && r.cleanup_cost_usd == null ? null : cost)];
      cells.forEach((c, i) => {
        const td = document.createElement("td");
        td.textContent = c;
        if (i >= 2) td.className = "num";
        if (r.error && i === 5) td.title = r.error;
        tr.append(td);
      });
      if (r.error) tr.className = "row-error";
      return tr;
    }),
  );

  $("months").replaceChildren(
    ...Object.entries(s.months).reverse().map(([m, t]) => {
      const tr = document.createElement("tr");
      for (const [i, c] of [m, String(t.dictations), usd(t.cost_usd)].entries()) {
        const td = document.createElement("td");
        td.textContent = c;
        if (i > 0) td.className = "num";
        tr.append(td);
      }
      return tr;
    }),
  );
}

async function renderMemory() {
  const m = await api.getMemory();
  $("m-memory").textContent = m
    ? `${m.app_mb.toFixed(0)} MB / ${m.webview_mb.toFixed(0)} MB`
    : "—";
  if (m) $("m-memory").title = `WebView: ${m.webview_processes} processos`;
}

// ---------------------------------------------------------------- render geral

function renderConfig() {
  const c = view.config;
  $("shortcut-display").textContent = prettyShortcut(c.shortcut);
  $("hero-shortcut").textContent = prettyShortcut(c.shortcut);
  $<HTMLSelectElement>("language").value = c.language ?? "";
  $<HTMLInputElement>("autostart").checked = view.autostart;
  $<HTMLInputElement>("save-last-recording").checked = c.save_last_recording;
  $<HTMLInputElement>("show-overlay").checked = c.show_overlay;
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="transport"]')) r.checked = r.value === c.transport;
  $<HTMLInputElement>("cleanup-enabled").checked = c.cleanup_enabled;
  $<HTMLInputElement>("history-enabled").checked = c.history_enabled;
  $<HTMLInputElement>("history-limit").value = String(c.history_limit);
  $("paths").textContent = `Configuração: ${view.paths.config} · Dados: ${view.paths.data} · Logs: ${view.paths.logs}`;
  renderKey();
}

function bindSimpleControls() {
  $<HTMLSelectElement>("language").addEventListener("change", (e) =>
    save({ language: (e.target as HTMLSelectElement).value || null }));
  $<HTMLInputElement>("save-last-recording").addEventListener("change", (e) =>
    save({ save_last_recording: (e.target as HTMLInputElement).checked }));
  $<HTMLInputElement>("show-overlay").addEventListener("change", (e) =>
    save({ show_overlay: (e.target as HTMLInputElement).checked }));
  $<HTMLInputElement>("cleanup-enabled").addEventListener("change", (e) =>
    save({ cleanup_enabled: (e.target as HTMLInputElement).checked }));
  $<HTMLInputElement>("history-enabled").addEventListener("change", (e) =>
    save({ history_enabled: (e.target as HTMLInputElement).checked }));
  $<HTMLInputElement>("history-limit").addEventListener("change", (e) =>
    save({ history_limit: Number((e.target as HTMLInputElement).value) }));
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="transport"]')) {
    r.addEventListener("change", () => r.checked && save({ transport: r.value as Config["transport"] }));
  }

  $<HTMLInputElement>("autostart").addEventListener("change", async (e) => {
    const input = e.target as HTMLInputElement;
    try {
      view.autostart = await api.setAutostart(input.checked);
      toast(view.autostart ? "Arranque automático ativado" : "Arranque automático desativado");
    } catch (err) {
      toast(errorText(err), "error");
    }
    input.checked = view.autostart;
  });

  $("history-clear").addEventListener("click", async () => {
    if (!confirm("Apagar todo o histórico de ditados deste computador?")) return;
    try {
      await api.clearHistory();
      await renderHistory();
      toast("Histórico apagado");
    } catch (err) {
      toast(errorText(err), "error");
    }
  });
}

/** A mascote do cartão de boas-vindas passa pelas quatro poses, como num ditado. */
function cycleHeroMascot() {
  const img = $<HTMLImageElement>("hero-mascot");
  const poses = [mascotUrl, listeningMascotUrl, thinkingMascotUrl, readyMascotUrl];
  let i = 0;
  img.src = poses[0];
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  window.setInterval(() => {
    if (document.hidden) return; // janela minimizada: não vale a pena animar
    img.classList.add("swap");
    window.setTimeout(() => {
      i = (i + 1) % poses.length;
      img.src = poses[i];
      img.classList.remove("swap");
    }, 180);
  }, 2600);
}

/** Marca na navegação a secção visível. */
function setupNav() {
  const links = [...document.querySelectorAll<HTMLAnchorElement>(".nav-link")];
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        for (const l of links) l.toggleAttribute("aria-current", l.hash === `#${entry.target.id}`);
      }
    },
    { rootMargin: "-30% 0px -60% 0px" },
  );
  document.querySelectorAll("section.panel").forEach((s) => observer.observe(s));
}

async function main() {
  $<HTMLImageElement>("mascot").src = mascotUrl;
  $<HTMLImageElement>("empty-mascot").src = mascotUrl;
  cycleHeroMascot();
  try {
    view = await api.getSettings();
  } catch (e) {
    toast(`Não foi possível ler as definições: ${errorText(e)}`, "error");
    return;
  }
  renderConfig();
  setupShortcut();
  setupKey();
  bindSimpleControls();
  setupNav();

  const stt = modelPicker("transcription", "stt", "stt_model");
  const cleanup = modelPicker("text", "cleanup", "cleanup_model");
  stt.render();
  cleanup.render();
  // Em paralelo: nenhuma destas chamadas depende das outras.
  await Promise.allSettled([stt.load(false), cleanup.load(false), renderHistory(), renderStats(), renderMemory()]);

  // Atualiza métricas e histórico quando acaba um ditado, sem polling.
  await onDictationRecorded(() => {
    void renderStats();
    void renderHistory();
  });
}

void main();
