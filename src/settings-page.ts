// Página de definições, com separadores horizontais. Cada alteração é gravada logo
// que acontece (sem botão "Guardar" global).

import {
  api,
  errorText,
  type CleanupLevel,
  type CommandOutput,
  type Config,
  type ModelInfo,
  type ModelList,
} from "./api";
import { bindDeviceSelect, loadDevices, syncDeviceSelect } from "./devices";
import { ago, describeAccess, heldModifiers, prettyShortcut, renderKeys, shortcutFromEvent, usd } from "./format";
import { onPage, showPage } from "./nav";
import { $, app, confirmDanger, notifyViewChange, onViewChange, save, toast } from "./store";
import { renderOutputDevices } from "./notes-page";
import { refreshRecentApps, setupStyles } from "./styles-panel";

export type Tab = "geral" | "transcricao" | "limpeza" | "estilos" | "dados";

let models: Array<{ load: (refresh: boolean) => Promise<void>; loaded: boolean }> = [];
/** Pára os captadores de atalho que estejam à espera de teclas (ao sair da página). */
const shortcutStoppers: Array<() => void> = [];

/** Vai à página das definições, opcionalmente a um separador. */
export function openSettings(tab?: Tab) {
  if (tab) selectTab(tab);
  showPage("config");
}

/** Quando a página aparece: as listas de dispositivos refrescam-se (entretanto podes ter
 *  ligado ou desligado um headset) e as de modelos pedem-se na primeira vez. */
function onShown() {
  void loadDevices($<HTMLSelectElement>("input-device"));
  void renderOutputDevices();
  for (const m of models) {
    if (!m.loaded) {
      m.loaded = true;
      void m.load(false);
    }
  }
}

function selectTab(tab: Tab) {
  for (const t of document.querySelectorAll<HTMLButtonElement>('#page-config [role="tab"]')) {
    const selected = t.id === `tab-${tab}`;
    t.setAttribute("aria-selected", String(selected));
    t.tabIndex = selected ? 0 : -1;
    $(t.getAttribute("aria-controls")!).hidden = !selected;
  }
  // As apps recentes mudam a cada ditado: pede-as sempre que o separador abre.
  if (tab === "estilos") void refreshRecentApps();
}

/** Abre no separador Geral com o foco no botão "Mudar" do atalho (ex.: o atalho
 *  está ocupado e clicaste no aviso da pílula). */
export function openShortcutSettings() {
  openSettings("geral");
  $("shortcut-record").focus();
}

/** Abre no separador Transcrição com o foco no campo da chave. */
export function openKeySettings() {
  openSettings("transcricao");
  $("key-input").focus();
}

function setupTabs() {
  const tabs = [...document.querySelectorAll<HTMLButtonElement>('#page-config [role="tab"]')];
  const idOf = (t: HTMLButtonElement) => t.id.slice(4) as Tab;
  for (const [i, t] of tabs.entries()) {
    t.addEventListener("click", () => selectTab(idOf(t)));
    // Setas esquerda/direita, Home e End mudam de separador (padrão ARIA para tablist horizontal).
    t.addEventListener("keydown", (e) => {
      const next =
        e.key === "ArrowRight" ? tabs[(i + 1) % tabs.length]
        : e.key === "ArrowLeft" ? tabs[(i - 1 + tabs.length) % tabs.length]
        : e.key === "Home" ? tabs[0]
        : e.key === "End" ? tabs[tabs.length - 1]
        : null;
      if (!next) return;
      e.preventDefault();
      selectTab(idOf(next));
      next.focus();
    });
  }
}

// ---------------------------------------------------------------- atalho

/** Qual dos captadores de atalho está à espera de teclas (só um de cada vez). */
let recordingShortcut: "shortcut" | "command_shortcut" | "screen_shortcut" | "quick_history_shortcut" | null = null;

/** Mostra o atalho em teclas, ou "Nenhum" quando não há (o do histórico rápido começa assim). */
function showShortcut(el: HTMLElement, shortcut: string) {
  if (shortcut) renderKeys(el, shortcut);
  else el.textContent = "Nenhum";
}

/** O captador de atalho ("Mudar" → prime a combinação), para o de ditado e o de
 *  comando. `prefix` é o início dos ids no HTML; `field`, o campo do Config. */
function setupShortcut(
  prefix: "shortcut" | "command-shortcut" | "screen-shortcut" | "quick-history",
  field: "shortcut" | "command_shortcut" | "screen_shortcut" | "quick_history_shortcut",
) {
  const button = $<HTMLButtonElement>(`${prefix}-record`);
  const display = $(`${prefix}-display`);
  const error = $(`${prefix}-error`);

  /** O maior conjunto de modificadores premidos sem nenhuma outra tecla: se o
   *  utilizador largar tudo assim, é um atalho só de modificadores (Ctrl+Win). */
  let peak: string[] = [];

  const stop = () => {
    recordingShortcut = null;
    peak = [];
    button.textContent = "Mudar";
    display.classList.remove("recording");
    showShortcut(display, app.view.config[field]);
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("keyup", onKeyUp, true);
  };

  const commit = async (shortcut: string) => {
    stop();
    error.hidden = true;
    if (!(await save({ [field]: shortcut } as Partial<Config>))) {
      // O backend explica porquê (conflito com outra app, tecla sem modificador,
      // igual ao outro atalho…).
      error.textContent = $("toast").textContent ?? "";
      error.hidden = false;
    }
  };

  const onKey = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation(); // o Esc cancela a captura e não chega a mais nada
    if (e.code === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey) return stop();
    const shortcut = shortcutFromEvent(e);
    if (!shortcut) {
      const held = heldModifiers(e);
      if (held.length > peak.length) peak = held;
      display.textContent = prettyShortcut(held.join("+")) + " + …";
      return;
    }
    void commit(shortcut);
  };

  const onKeyUp = (e: KeyboardEvent) => {
    e.preventDefault();
    e.stopPropagation();
    // Largou tudo sem carregar numa tecla "normal": serve a combinação de modificadores.
    if (heldModifiers(e).length === 0 && peak.length >= 2) void commit(peak.join("+"));
  };

  button.addEventListener("click", () => {
    if (recordingShortcut === field) return stop();
    if (recordingShortcut) return; // o outro captador está à espera
    recordingShortcut = field;
    error.hidden = true;
    button.textContent = "Cancelar";
    display.classList.add("recording");
    display.textContent = "Prime a nova combinação…";
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKeyUp, true);
  });

  // Sair da página a meio da captura não pode deixar o teclado "capturado".
  shortcutStoppers.push(() => recordingShortcut === field && stop());

  $(`${prefix}-reset`).addEventListener("click", async () => {
    error.hidden = true;
    await save({ [field]: app.view.defaults[field] } as Partial<Config>);
  });
}

// ---------------------------------------------------------------- chave

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

  $("key-console").addEventListener("click", () =>
    void api.openXaiConsole().catch((e) => toast(`Não consegui abrir o browser: ${errorText(e)}`, "error")));

  $("key-form").addEventListener("submit", async (e) => {
    e.preventDefault();
    try {
      const access = await api.setApiKey(input.value);
      input.value = ""; // a chave não fica no DOM
      app.view.api_key_set = true;
      notifyViewChange();
      const { text, ok } = describeAccess(access);
      info.textContent = `Guardada. ${text}`;
      toast(ok ? "Chave da xAI guardada" : "Chave guardada, mas com permissões em falta", ok ? "ok" : "error");
    } catch (err) {
      toast(errorText(err), "error");
    }
  });

  $("key-test").addEventListener("click", async () => {
    info.textContent = "A testar…";
    try {
      info.textContent = `Válida. ${describeAccess(await api.testApiKey()).text}`;
    } catch (err) {
      info.textContent = "";
      toast(errorText(err), "error");
    }
  });

  $("key-delete").addEventListener("click", async () => {
    const ok = await confirmDanger(
      "Apagar a chave da xAI?",
      "Tiro-a do cofre do sistema. Não consigo transcrever nem tratar texto até guardares outra.",
      "Apagar chave",
    );
    if (!ok) return;
    try {
      await api.deleteApiKey();
      app.view.api_key_set = false;
      info.textContent = "";
      notifyViewChange();
      toast("Chave da xAI apagada");
    } catch (err) {
      toast(errorText(err), "error");
    }
  });
}

// ---------------------------------------------------------------- modelos

function price(m: ModelInfo): string {
  if (m.prompt_price == null) return "preço variável";
  const perM = (v: number | null) => (v == null ? "?" : usd(v * 1e6, 2));
  return `${perM(m.prompt_price)} / ${perM(m.completion_price)} por M tokens`;
}

/** Um seletor de modelos de texto com pesquisa (limpeza, resumos ou capturas do ecrã). Os das
 *  capturas só mostram os que veem imagens. */
function modelPicker(prefix: "cleanup" | "summary" | "vision", field: "cleanup_model" | "summary_model" | "vision_model") {
  const list = $<HTMLUListElement>(`${prefix}-models`);
  const search = $<HTMLInputElement>(`${prefix}-search`);
  const meta = $(`${prefix}-meta`);
  let data: ModelList | null = null;

  const render = () => {
    const current = app.view.config[field];
    // Os resumos e as capturas sem modelo próprio usam o da limpeza.
    $(`${prefix}-current`).textContent = current
      ? `Atual: ${current}`
      : `Atual: o mesmo da limpeza (${app.view.config.cleanup_model})`;
    if (!data) return;
    const q = search.value.trim().toLowerCase();
    // O escolhido não sai da lista das capturas só porque a lista (antiga) não diz que vê imagens.
    const matches = data.models.filter(
      (m) => (!q || m.id.toLowerCase().includes(q)) && (prefix !== "vision" || m.vision || m.id === current),
    );
    // O modelo selecionado aparece sempre primeiro; depois os que não raciocinam (mais rápidos).
    matches.sort((a, b) => Number(b.id === current) - Number(a.id === current) || Number(a.reasoning) - Number(b.reasoning));
    // Se o foco estava na lista, volta para a opção selecionada depois de a redesenhar.
    const hadFocus = list.contains(document.activeElement);
    // Tabindex rotativo: só uma opção entra na ordem do Tab (a selecionada, ou a
    // primeira); as setas, Home e End andam entre as outras.
    const tabStop = Math.max(0, matches.findIndex((m) => m.id === current));
    list.replaceChildren(
      ...matches.map((m, i) => {
        const li = document.createElement("li");
        const selected = m.id === current;
        li.setAttribute("role", "option");
        li.setAttribute("aria-selected", String(selected));
        li.tabIndex = i === tabStop ? 0 : -1;
        li.innerHTML = `<span class="model-name"></span><code class="model-id"></code><span class="model-price"></span>`;
        (li.children[0] as HTMLElement).textContent = m.id;
        (li.children[1] as HTMLElement).textContent = m.reasoning ? "raciocina: mais lento" : "responde logo";
        (li.children[2] as HTMLElement).textContent = price(m);
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
    meta.textContent =
      `${matches.length} de ${data.models.length} modelos · lista de ${ago(data.fetched_at)}` +
      (data.stale ? " · sem ligação, a usar a cópia guardada" : "");
  };

  const load = async (refresh: boolean) => {
    meta.textContent = "A carregar modelos…";
    try {
      data = await api.listModels(refresh);
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
  if (recordingShortcut !== "shortcut") renderKeys($("shortcut-display"), c.shortcut);
  if (recordingShortcut !== "command_shortcut") renderKeys($("command-shortcut-display"), c.command_shortcut);
  if (recordingShortcut !== "screen_shortcut") renderKeys($("screen-shortcut-display"), c.screen_shortcut);
  if (recordingShortcut !== "quick_history_shortcut") showShortcut($("quick-history-display"), c.quick_history_shortcut);
  // Outra app ocupou o atalho: diz porquê ele não funciona até escolheres outro.
  $("shortcut-busy").hidden = app.view.shortcut_ok;
  $<HTMLInputElement>("command-enabled").checked = c.command_enabled;
  $("command-shortcut-busy").hidden = !c.command_enabled || app.view.command_ok;
  // Desligado, o atalho fica à vista (e pode ser mudado), mas esbatido.
  $("command-shortcut-row").classList.toggle("is-off", !c.command_enabled);
  $<HTMLInputElement>("screen-enabled").checked = c.screen_enabled;
  $("screen-shortcut-busy").hidden = !c.screen_enabled || app.view.screen_ok;
  $("screen-shortcut-row").classList.toggle("is-off", !c.screen_enabled);
  $("quick-history-busy").hidden = !c.quick_history_shortcut || app.view.quick_ok;
  syncDeviceSelect($<HTMLSelectElement>("input-device"));
  $<HTMLSelectElement>("language").value = c.language ?? "";
  $<HTMLInputElement>("autostart").checked = app.view.autostart;
  $<HTMLInputElement>("save-last-recording").checked = c.save_last_recording;
  $<HTMLInputElement>("show-overlay").checked = c.show_overlay;
  $<HTMLInputElement>("keep-in-clipboard").checked = c.keep_in_clipboard;
  $<HTMLInputElement>("sounds").checked = c.sounds;
  // Não mexer no campo enquanto o estás a escrever (a vista muda depois de cada ditado).
  const budget = $<HTMLInputElement>("budget");
  if (document.activeElement !== budget) budget.value = c.monthly_budget_usd == null ? "" : String(c.monthly_budget_usd);
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="cleanup-level"]')) r.checked = r.value === c.cleanup_level;
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="command-output"]')) r.checked = r.value === c.command_output;
  $("command-output-row").classList.toggle("is-off", !c.command_enabled);
  $<HTMLInputElement>("history-enabled").checked = c.history_enabled;
  $<HTMLInputElement>("history-limit").value = String(c.history_limit);
  $<HTMLInputElement>("keep-audio").checked = c.keep_audio;
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
  checkbox("history-enabled", "history_enabled");
  checkbox("command-enabled", "command_enabled");
  checkbox("screen-enabled", "screen_enabled");
  // Desligar apaga o áudio já guardado (commands.rs, save_settings): digo-o.
  $<HTMLInputElement>("keep-audio").addEventListener("change", async (e) => {
    const on = (e.target as HTMLInputElement).checked;
    if ((await save({ keep_audio: on })) && !on) toast("Apaguei o áudio guardado dos ditados");
  });
  bindDeviceSelect($<HTMLSelectElement>("input-device"));

  $<HTMLSelectElement>("language").addEventListener("change", (e) =>
    save({ language: (e.target as HTMLSelectElement).value || null }));
  $<HTMLInputElement>("history-limit").addEventListener("change", (e) =>
    save({ history_limit: Number((e.target as HTMLInputElement).value) }));
  $<HTMLInputElement>("budget").addEventListener("change", (e) => {
    const raw = (e.target as HTMLInputElement).value.trim();
    // Vazio = sem orçamento. O backend recusa 0 e negativos com uma mensagem.
    void save({ monthly_budget_usd: raw === "" ? null : Number(raw) });
  });
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="cleanup-level"]')) {
    r.addEventListener("change", () => r.checked && save({ cleanup_level: r.value as CleanupLevel }));
  }
  for (const r of document.querySelectorAll<HTMLInputElement>('input[name="command-output"]')) {
    r.addEventListener("change", () => r.checked && save({ command_output: r.value as CommandOutput }));
  }
  // Ligações entre os dois separadores (o nível e os estilos dependem um do outro).
  $("cleanup-open-styles").addEventListener("click", () => {
    selectTab("estilos");
    $("tab-estilos").focus();
  });
  $("styles-open-cleanup").addEventListener("click", () => {
    selectTab("limpeza");
    document.querySelector<HTMLInputElement>('input[name="cleanup-level"]:checked')?.focus();
  });

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
      "Removo todos os ditados e as conversas do assistente guardados neste computador. Não dá para desfazer.",
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

/// Procura uma versão nova a pedido. Mostra o resultado aqui; a instalação fica num
/// botão que só aparece quando há uma versão.
function setupUpdates() {
  const check = $<HTMLButtonElement>("update-check");
  const apply = $<HTMLButtonElement>("update-apply");
  const status = $("update-status");
  check.addEventListener("click", async () => {
    check.disabled = true;
    apply.hidden = true;
    status.textContent = "A procurar…";
    try {
      const info = await api.checkUpdate();
      status.textContent = info
        ? `Há a versão ${info.version} (tens a ${info.current}).`
        : "Já tens a versão mais recente.";
      apply.hidden = !info;
    } catch (e) {
      status.textContent = `Não consegui verificar: ${errorText(e)}`;
    } finally {
      check.disabled = false;
    }
  });
  apply.addEventListener("click", async () => {
    apply.disabled = true;
    apply.textContent = "A atualizar…";
    try {
      await api.installUpdate(); // se correr bem, a app reinicia
    } catch (e) {
      toast(`Não foi possível atualizar: ${errorText(e)}`, "error");
      apply.disabled = false;
      apply.textContent = "Atualizar";
    }
  });
}

export function setupSettings(onHistoryCleared: () => void) {
  setupTabs();
  setupUpdates();
  setupShortcut("shortcut", "shortcut");
  setupShortcut("command-shortcut", "command_shortcut");
  setupShortcut("screen-shortcut", "screen_shortcut");
  setupShortcut("quick-history", "quick_history_shortcut");
  setupKey();
  bindSimpleControls(onHistoryCleared);
  setupStyles();
  onViewChange(renderControls);
  renderControls();
  // As listas de modelos só são pedidas quando a página aparece pela primeira vez.
  $("summary-reset").addEventListener("click", () => void save({ summary_model: "" }));
  $("vision-reset").addEventListener("click", () => void save({ vision_model: "" }));
  models = [
    modelPicker("cleanup", "cleanup_model"),
    modelPicker("summary", "summary_model"),
    modelPicker("vision", "vision_model"),
  ];
  onPage("config", {
    show: onShown,
    hide: () => {
      for (const stop of shortcutStoppers) stop();
    },
  });
}
