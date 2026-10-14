// Navegação entre as páginas da janela principal. Sem router: cada página é uma
// <section id="page-…"> e só uma está visível. Os módulos que precisam de saber quando a
// sua página aparece ou desaparece registam-se com `onPage`.

export type Page = "inicio" | "dicionario" | "ficheiros" | "notas" | "metricas" | "config";

interface Hooks {
  /** A página acabou de ficar visível (também quando já estava). */
  show?: () => void;
  /** Saíste da página para outra. */
  hide?: () => void;
}

const hooks = new Map<Page, Hooks>();
let current: Page = "inicio";

export function onPage(page: Page, h: Hooks) {
  hooks.set(page, h);
}

export const currentPage = () => current;

/** Muda de página. `focus`: leva o foco para o título da nova página, para quem
 *  usa teclado ou leitor de ecrã saber que mudou (não quando a mudança é automática). */
export function showPage(page: Page, focus = false) {
  const previous = current;
  current = page;
  for (const link of document.querySelectorAll<HTMLButtonElement>(".nav-link[data-page]")) {
    if (link.dataset.page === page) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  }
  for (const p of document.querySelectorAll<HTMLElement>(".page")) p.hidden = p.id !== `page-${page}`;
  document.querySelector("main")!.scrollTop = 0;
  if (previous !== page) hooks.get(previous)?.hide?.();
  hooks.get(page)?.show?.();
  if (focus) document.querySelector<HTMLElement>(`#page-${page} h1`)?.focus({ preventScroll: true });
}
