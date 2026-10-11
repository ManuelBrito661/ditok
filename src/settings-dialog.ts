// Modal de definições, com separadores à esquerda como no Flow. Cada alteração é
// gravada logo que acontece (sem botão "Guardar" global).

import {
  api,
  errorText,
  type Config,
  type KeyInfo,
  type ModelInfo,
  type ModelKind,
  type ModelList,
} from "./api";
import { ago, prettyShortcut, renderKeys, shortcutFromEvent, usd } from "./format";
import { $, app, confirmDanger, notifyViewChange, onViewChange, save, toast } from "./store";

export type Tab = "geral" | "transcricao" | "limpeza" | "dados";

const dialog = () => $<HTMLDialogElement>("settings");
let models: Array<{ load: (refresh: boolean) => Promise<void>; loaded: boolean }> = [];

/** Abre o modal (opcionalmente num separador) e carrega as listas de modelos na primeira vez. */
export function openSettings(tab: Tab = "geral") {
  selectTab(tab);
  if (!dialog().open) dialog().showModal();
  for (const m of models) {
    if (!m.loaded) {
      m.loaded = true;
      void m.load(false);
    }
  }
}

function selectTab(tab: Tab) {
  for (const t of document.querySelectorAll<HTMLButtonElement>('#settings [role="tab"]')) {
    const selected = t.id === `tab-${tab}`;
    t.setAttribute("aria-selected", String(selected));
    t.tabIndex = selected ? 0 : -1;
    $(t.getAttribute("aria-controls")!).hidden = !selected;
  }
}

function setupTabs() {
  const tabs = [...document.querySelectorAll<HTMLButtonElement>('#settings [role="tab"]')];
  const idOf = (t: HTMLButtonElement) => t.id.slice(4) as Tab;
  for (const [i, t] of tabs.entries()) {
    t.addEventListener("click", () => selectTab(idOf(t)));
    // Setas cima/baixo mudam de separador (padrão ARIA para tablist vertical).
    t.addEventListener("keydown", (e) => {
      const step = e.key === "ArrowDown" ? 1 : e.key === "ArrowUp" ? -1 : 0;
      if (!step) return;
      e.preventDefault();
      const next = tabs[(i + step + tabs.length) % tabs.length];
      selectTab(idOf(next));
      next.focus();
    });
  }
  $("settings-close").addEventListener("click", () => dialog().close());
  // Clicar no fundo escurecido (fora da caixa) também fecha.
  dialog().addEventListener("click", (e) => {
    if (e.target === dialog()) dialog().close();
  });
}

// ---------------------------------------------------------------- atalho

let recordingShortcut = false;

function setupShortcut() {
  const button = $<HTMLButtonElement>("shortcut-record");
  const display = $("shortcut-display");
  const error = $("shortcut-error");

  const stop = () => {
    recordingShortcut = false;
    button.textContent = "Mudar";
    display.classList.remove("recording");
    renderKeys(display, app.view.config.shortcut);
    window.removeEventListener("keydown", onKey, true);
  };

  const onKey = async (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation(); // impede também que o Esc feche o modal
    if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) return stop();
    const shortcut = shortcutFromEvent(e);
    if (!shortcut) {
      display.textContent =
        prettyShortcut([e.ctrlKey && "Control", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean).join("+")) + " + …";
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
    if (recordingShortcut) return stop();
    recordingShortcut = true;
    error.hidden = true;
    button.textContent = "Cancelar";
    display.classList.add("recording");
    display.textContent = "Prime a nova combinação…";
    window.addEventListener("keydown", onKey, true);
  });

  // Fechar o modal a meio da gravação não pode deixar o teclado "capturado".
  dialog().addEventListener("close", () => recordingShortcut && stop());

  $("shortcut-reset").addEventListener("click", async () => {
    error.hidden = true;
    await save({ shortcut: app.view.defaults.shortcut });
  });
}

// ---------------------------------------------------------------- chave

function describeKey(info: KeyInfo | null): string {
  if (!info) return "Guardada, mas não a consegui validar (sem rede?).";
  const parts = [];
  if (info.label) parts.push(info.label);
  if (info.usage != null) parts.push(`gasto ${usd(info.usage, 2)}`);
  if (info.limit_remaining != null) parts.push(`restam ${usd(info.limit_remaining, 2)}`);
  else if (info.limit == null) parts.push("sem limite de gasto");
  return parts.join(" · ");
}

function renderKey() {
  const set = app.view.api_key_set;
  const status = $("key-status");
  status.textContent = set ? "Configurada" : "Não configurada";
  status.className = `pill ${set ? "ok" : "warn"}`;
  $<HTMLButtonElement>("key-test").disabled = !set;
  $<HTMLButtonElement>("key-delete").disabled = !set;
}

function setupKey() {
  const input = $<HTMLInputElement>("key-input");
  const info = $("key-info");

  $("key-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    try {
      const result = await api.setApiKey(input.value);
      input.value = ""; // a chave não fica no DOM
      app.view.api_key_set = true;
      app.key = result;
      notifyViewChange();
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
    const ok = await confirmDanger(
      "Apagar a chave do OpenRouter?",
      "Tiro-a do cofre do sistema. Não consigo transcrever até guardares outra.",
      "Apagar chave",
    );
    if (!ok) return;
    try {
      await api.deleteApiKey();
      app.view.api_key_set = false;
      app.key = null;
      info.textContent = "";
      notifyViewChange();
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
  const perM = (v: number | null) => (v == null ? "?" : usd(v * 1e6, 2));
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
    const current = app.view.config[field];
    $(`${prefix}-current`).textContent = `Atual: ${current}`;
    if (!data) return;
    const q = search.value.trim().toLowerCase();
    const matches = data.models.filter((m) => !q || m.id.toLowerCase().includes(q) || m.name.toLowerCase().includes(q));
    // O modelo selecionado aparece sempre primeiro.
    matches.sort((a, b) => Number(b.id === current) - Number(a.id === current));
    // Se o foco estava na lista, volta para a opção selecionada depois de a redesenhar.
    const hadFocus = list.contains(document.activeElement);
    const shown = matches.slice(0, MAX_SHOWN);
    // Tabindex rotativo: só uma opção entra na ordem do Tab (a selecionada, ou a
    // primeira); as setas, Home e End andam entre as outras.
    const tabStop = Math.max(0, shown.findIndex((m) => m.id === current));
    list.replaceChildren(
      ...shown.map((m, i) => {
        const li = document.createElement("li");
        const selected = m.id === current;
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(selected));
        li.tabIndex = i === tabStop ? 0 : -1;
        li.innerHTML = `<span class="model-name"></span><code class="model-id"></code><span class="model-price"></span>`;
        (li.children[0] as HTMLElement).textContent = m.name;
        (li.children[1] as HTMLElement).textContent = m.id;
        (li.children[2] as HTMLElement).textContent = price(m, kind);
        const choose = () => !selected && save({ [field]: m.id } as Partial<Config>);
        li.addEventListener("click", choose);
        li.addEventListener("keydown", (e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            void choose();
            return;
          }
          const items = [...list.children] as HTMLElement[];
          const at = items.indexOf(li);
          const to =
            e.key === "ArrowDown" ? Math.min(at + 1, items.length - 1)
            : e.key === "ArrowUp" ? Math.max(at - 1, 0)
            : e.key === "Home" ? 0
            : e.key === "End" ? items.length - 1
            : -1;
          if (to < 0) return;
          e.preventDefault();
          li.tabIndex = -1;
          items[to].tabIndex = 0;
          items[to].focus();
        });
        return li;
      }),
    );
    if (hadFocus) (list.children[tabStop] as HTMLElement | undefined)?.focus();
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
      meta.textContent = `Não consegui obter a lista: ${errorText(e)}`;
    }
  };

  search.addEventListener("input", render);
  $(`${prefix}-refresh`).addEventListener("click", () => load(true));
  onViewChange(render);
  render();
  return { load, loaded: false };
}

// ---------------------------------------------------------------- controlos simples

function renderControls() {
  const c = app.view.config;
  if (!recordingShortcut) renderKeys($("shortcut-display"), c.shortcut);
  $<HTMLSelectElement>("language").value = c.language ?? "";
  $<HTMLInputElement>("autostart").checked = app.view.autostart;
  $<HTMLInputElement>("save-last-recording").checked = c.save_last_recording;
  $<HTMLInputElement>("show-overlay").checked = c.show_overlay;
  $<HTMLInputElement>("keep-in-clipboard").checked = c.keep_in_clipboard;
  $<HTMLInputElement>("sounds").checked = c.sounds;
  // Não mexer no campo enquanto o estás a escrever (a vista muda depois de cada ditado).
  const budget = $<HTMLInputElement>("budget");
  if (document.activeElement !== budget) budget.value = c.monthly_budget_usd == null ? "" : String(c.monthly_budget_usd);
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="transport"]')) r.checked = r.value === c.transport;
  $<HTMLInputElement>("cleanup-enabled").checked = c.cleanup_enabled;
  $<HTMLInputElement>("history-enabled").checked = c.history_enabled;
  $<HTMLInputElement>("history-limit").value = String(c.history_limit);
  const p = app.view.paths;
  $("paths").textContent = `Configuração: ${p.config} · Dados: ${p.data} · Logs: ${p.logs}`;
  renderKey();
}

function bindSimpleControls(onHistoryCleared: () => void) {
  const checkbox = (id: string, field: keyof Config) =>
    $<HTMLInputElement>(id).addEventListener("change", (e) =>
      save({ [field]: (e.target as HTMLInputElement).checked } as Partial<Config>));
  checkbox("save-last-recording", "save_last_recording");
  checkbox("show-overlay", "show_overlay");
  checkbox("keep-in-clipboard", "keep_in_clipboard");
  checkbox("sounds", "sounds");
  checkbox("cleanup-enabled", "cleanup_enabled");
  checkbox("history-enabled", "history_enabled");

  $<HTMLSelectElement>("language").addEventListener("change", (e) =>
    save({ language: (e.target as HTMLSelectElement).value || null }));
  $<HTMLInputElement>("history-limit").addEventListener("change", (e) =>
    save({ history_limit: Number((e.target as HTMLInputElement).value) }));
  $<HTMLInputElement>("budget").addEventListener("change", (e) => {
    const raw = (e.target as HTMLInputElement).value.trim();
    // Vazio = sem orçamento. O backend recusa 0 e negativos com uma mensagem.
    void save({ monthly_budget_usd: raw === "" ? null : Number(raw) });
  });
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="transport"]')) {
    r.addEventListener("change", () => r.checked && save({ transport: r.value as Config["transport"] }));
  }

  $<HTMLInputElement>("autostart").addEventListener("change", async (e) => {
    const input = e.target as HTMLInputElement;
    try {
      // Sem aviso quando corre bem: o interruptor já mostra o novo estado.
      app.view.autostart = await api.setAutostart(input.checked);
    } catch (err) {
      toast(errorText(err), "error");
    }
    input.checked = app.view.autostart;
  });

  $("history-clear").addEventListener("click", async () => {
    const ok = await confirmDanger(
      "Apagar todo o histórico?",
      "Removo todos os ditados guardados neste computador. Não dá para desfazer.",
      "Apagar histórico",
    );
    if (!ok) return;
    try {
      await api.clearHistory();
      onHistoryCleared();
      toast("Histórico apagado");
    } catch (err) {
      toast(errorText(err), "error");
    }
  });
}

export function setupSettings(onHistoryCleared: () => void) {
  setupTabs();
  setupShortcut();
  setupKey();
  bindSimpleControls(onHistoryCleared);
  onViewChange(renderControls);
  renderControls();
  // As listas de modelos só são pedidas quando o modal abre pela primeira vez.
  models = [
    modelPicker("transcription", "stt", "stt_model"),
    modelPicker("text", "cleanup", "cleanup_model"),
  ];
}
