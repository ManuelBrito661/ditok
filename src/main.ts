// Janela principal do Ditok. Sem framework: cada módulo trata de uma parte
// (Início, Métricas, modal de definições) e o estado partilhado está em store.ts.

import "@fontsource/instrument-serif/latin-400.css";
import mascotUrl from "../design/mascot/parado.svg?inline";
import { api, errorText, onDictationRecorded } from "./api";
import { setupDictionary } from "./dictionary-page";
import { clearHomeHistory, refreshHome, setupHome } from "./home";
import { renderMemory, renderStats, setupMetrics } from "./metrics-page";
import { openSettings, setupSettings } from "./settings-dialog";
import { $, app, onViewChange, refreshKey, toast } from "./store";

type Page = "inicio" | "dicionario" | "metricas";

function showPage(page: Page) {
  for (const link of document.querySelectorAll<HTMLButtonElement>(".nav-link[data-page]")) {
    if (link.dataset.page === page) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  for (const p of document.querySelectorAll<HTMLElement>(".page")) p.hidden = p.id !== `page-${page}`;
  document.querySelector("main")!.scrollTop = 0;
  if (page === "metricas") void renderMemory().catch(() => {});
}

function setupNav() {
  for (const link of document.querySelectorAll<HTMLButtonElement>(".nav-link[data-page]")) {
    link.addEventListener("click", () => showPage(link.dataset.page as Page));
  }
  $("open-settings").addEventListener("click", () => openSettings());
}

/** Ponto verde/âmbar na barra lateral: o ditado está pronto a usar? */
function renderStatus() {
  const ok = app.view.api_key_set;
  $("status-dot").className = `dot ${ok ? "ok" : "warn"}`;
  $("status-text").textContent = ok ? "Pronto a ditar" : "Falta a chave";
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

async function main() {
  $<HTMLImageElement>("brand-mascot").src = mascotUrl;
  try {
    app.view = await api.getSettings();
  } catch (e) {
    toast(`Não foi possível ler as definições: ${errorText(e)}`, "error");
    return;
  }
  setupNav();
  setupSettings(clearHomeHistory);
  setupHome();
  setupDictionary();
  setupMetrics();
  onViewChange(renderStatus);
  renderStatus();

  // Em paralelo: nenhuma destas chamadas depende das outras.
  await Promise.allSettled([refreshHome(), renderStats(), renderMemory(), refreshKey(), checkUpdate()]);

  // Atualiza o Início e as métricas quando acaba um ditado, sem polling.
  await onDictationRecorded(() => {
    void refreshHome();
    void renderStats();
    void refreshKey(); // o gasto da chave mudou
  });
}

void main();
