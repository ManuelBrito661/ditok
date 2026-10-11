// Janela principal do Ditok. Sem framework: cada módulo trata de uma parte
// (Início, Métricas, modal de definições) e o estado partilhado está em store.ts.

import "@fontsource/instrument-serif/latin-400.css";
import mascotUrl from "../design/mascot/parado.svg?inline";
import { api, errorText, onDictationRecorded, onOpenRequest, onSettingsChanged, type OpenTarget } from "./api";
import { setupDictionary } from "./dictionary-page";
import { setupFiles } from "./files-page";
import { clearHomeHistory, refreshHome, setupHome } from "./home";
import { renderMemory, renderStats, setupMetrics } from "./metrics-page";
import { isOnboardingOpen, openOnboarding, setupOnboarding } from "./onboarding";
import { openKeySettings, openSettings, openShortcutSettings, setupSettings } from "./settings-dialog";
import { $, app, notifyViewChange, onViewChange, refreshKey, toast } from "./store";

type Page = "inicio" | "dicionario" | "ficheiros" | "metricas";

/** Muda de página. `focus`: leva o foco para o título da nova página, para quem
 *  usa teclado ou leitor de ecrã saber que mudou (não quando a mudança é automática). */
function showPage(page: Page, focus = false) {
  for (const link of document.querySelectorAll<HTMLButtonElement>(".nav-link[data-page]")) {
    if (link.dataset.page === page) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  for (const p of document.querySelectorAll<HTMLElement>(".page")) p.hidden = p.id !== `page-${page}`;
  document.querySelector("main")!.scrollTop = 0;
  if (focus) document.querySelector<HTMLElement>(`#page-${page} h1`)?.focus({ preventScroll: true });
  if (page === "metricas") void renderMemory().catch(() => {});
}

function setupNav() {
  for (const link of document.querySelectorAll<HTMLButtonElement>(".nav-link[data-page]")) {
    link.addEventListener("click", () => showPage(link.dataset.page as Page, true));
  }
  $("open-settings").addEventListener("click", () => openSettings());
}

/** Ponto verde/âmbar na barra lateral: o ditado está pronto a usar? */
function renderStatus() {
  const { api_key_set: key, shortcut_ok: shortcut } = app.view;
  $("status-dot").className = `dot ${key && shortcut ? "ok" : "warn"}`;
  $("status-text").textContent = !key ? "Falta a chave" : !shortcut ? "Atalho ocupado" : "Pronto a ditar";
}

/** A pílula (ou outro sítio fora da janela) pediu para mostrar algo. */
function openTarget(target: OpenTarget) {
  if (target === "shortcut") {
    openShortcutSettings();
  } else if (isOnboardingOpen() || !app.view.config.onboarding_done) {
    openOnboarding("key");
  } else {
    openKeySettings();
  }
}

/** Vai buscar o sítio pendente (se houver) e mostra-o. */
async function takeOpenRequest() {
  const target = await api.takeOpen().catch(() => null);
  if (target) openTarget(target);
}

/// Mostra o cartão "Nova versão" se o GitHub tiver uma versão mais recente.
/// Falhas (sem rede, ainda sem Releases) ficam em silêncio: não são do utilizador.
async function checkUpdate() {
  const info = await api.checkUpdate().catch(() => null);
  if (!info) return;
  $("update-text").textContent = `${info.current} → ${info.version}`;
  $("update").hidden = false;
  const button = $<HTMLButtonElement>("update-install");
  button.addEventListener("click", async () => {
    button.disabled = true;
    button.textContent = "A atualizar…";
    try {
      await api.installUpdate(); // se correr bem, a app reinicia e não volta aqui
    } catch (e) {
      toast(`Não foi possível atualizar: ${errorText(e)}`, "error");
      button.disabled = false;
      button.textContent = "Atualizar";
    }
  });
}

/// Tira o menu do WebView2 (Anterior, Atualizar, Imprimir…), que denuncia que é
/// uma página web. Fica nas caixas de texto, onde Copiar/Colar dá jeito, e em
/// `npm run tauri dev` para continuar a haver "Inspecionar".
/// (Na barra flutuante, o clique direito abre o menu do Ditok: ver overlay.ts.)
function blockContextMenu() {
  if (import.meta.env.DEV) return;
  document.addEventListener("contextmenu", (e) => {
    const target = e.target as HTMLElement;
    if (target.closest("input, textarea, [contenteditable='true']")) return;
    e.preventDefault();
  });
}

/** Sem definições nada funciona: em vez das páginas vazias, um ecrã de erro com
 *  o motivo e "Tentar de novo". Resolve quando as definições chegam. */
async function loadSettings(): Promise<void> {
  const error = $("boot-error");
  for (;;) {
    try {
      app.view = await api.getSettings();
      break;
    } catch (e) {
      for (const p of document.querySelectorAll<HTMLElement>(".page")) p.hidden = p !== error;
      $("boot-error-text").textContent = errorText(e);
      $("status-dot").className = "dot warn";
      $("status-text").textContent = "Sem definições";
      error.hidden = false;
      const retry = $<HTMLButtonElement>("boot-retry");
      retry.disabled = false;
      $("boot-error-title").focus();
      await new Promise((resolve) => retry.addEventListener("click", resolve, { once: true }));
      retry.disabled = true;
      $("status-text").textContent = "A carregar…";
      $("status-dot").className = "dot";
    }
  }
  if (!error.hidden) {
    error.hidden = true;
    showPage("inicio");
  }
}

async function main() {
  blockContextMenu();
  $<HTMLImageElement>("brand-mascot").src = mascotUrl;
  for (const img of document.querySelectorAll<HTMLImageElement>(".boot-error .empty-mascot")) img.src = mascotUrl;
  await loadSettings();
  setupNav();
  setupSettings(clearHomeHistory);
  setupHome();
  setupDictionary();
  setupMetrics();
  setupOnboarding();
  onViewChange(renderStatus);
  renderStatus();

  // Primeira execução: o guia por cima de tudo. Depois, se a janela foi aberta
  // por um clique num aviso da pílula, vai a esse sítio.
  if (!app.view.config.onboarding_done) openOnboarding();
  await takeOpenRequest();
  void onOpenRequest(() => void takeOpenRequest());
  // Mudaram por fora (ex.: "Silenciar sons" no tray): sem isto, a próxima
  // alteração aqui gravava por cima com o valor antigo.
  void onSettingsChanged(async () => {
    const view = await api.getSettings().catch(() => null);
    if (!view) return;
    app.view = view;
    notifyViewChange();
  });

  // Em paralelo: nenhuma destas chamadas depende das outras.
  await Promise.allSettled([
    setupFiles(() => showPage("ficheiros")),
    refreshHome(),
    renderStats(),
    renderMemory(),
    refreshKey(),
    checkUpdate(),
  ]);

  // Atualiza o Início e as métricas quando acaba um ditado, sem polling.
  await onDictationRecorded(() => {
    void refreshHome();
    void renderStats();
    void refreshKey(); // o gasto da chave mudou
  });
}

void main();
