// Separador "Estilos por app" das definições: regras "nesta app, este estilo".
// Quem as aplica é o backend (config.rs, Config::style_for), no início de cada
// ditado; aqui só se edita a lista, gravada no config.json como o resto.

import { api, type Style, type StyleRule } from "./api";
import { STYLE_LABEL } from "./format";
import { $, app, onViewChange, save, toast } from "./store";

const STYLES: Style[] = ["casual", "formal", "literal", "default"];

/** Como o backend compara nomes (config.rs, app_key): minúsculas e sem ".exe". */
const appKey = (name: string) => name.trim().toLowerCase().replace(/\.exe$/, "");

/** As apps onde ditaste nesta sessão (vêm do backend quando o separador abre). */
let recent: string[] = [];
/** O que está desenhado na lista; ver o mesmo truque em dictionary-page.ts. */
let rendered = "";

function rules(): StyleRule[] {
  return app.view.config.styles;
}

/** Gravações em fila: cada uma parte da lista que já inclui as anteriores (senão
 *  duas mudanças seguidas perdiam uma). */
let queue: Promise<unknown> = Promise.resolve();

/** Aplica `change` à lista atual e grava; `null` = não há nada a gravar. */
function edit(change: (list: StyleRule[]) => StyleRule[] | null): Promise<boolean> {
  const run = queue.then(async () => {
    const next = change(rules().slice());
    return next ? save({ styles: next }) : false;
  });
  queue = run.catch(() => false);
  return run;
}

/** Uma regra apagada, com o que é preciso para o "Desfazer". */
interface Removed {
  rule: StyleRule;
  /** A app da regra seguinte, à data do apagar (o Desfazer repõe antes dela). */
  next?: string;
  index: number;
}

/** Apaga a regra desta app (pelo nome, não pelo índice). */
async function removeRule(name: string): Promise<Removed | null> {
  const key = appKey(name);
  const found: { value?: Removed } = {};
  await edit((list) => {
    const index = list.findIndex((r) => appKey(r.app) === key);
    if (index < 0) return null;
    found.value = { rule: list[index], next: list[index + 1]?.app, index };
    return list.filter((_, i) => i !== index);
  });
  return found.value ?? null;
}

async function restoreRule({ rule, next, index }: Removed) {
  const key = appKey(rule.app);
  const place = { at: 0 };
  const ok = await edit((list) => {
    // Se entretanto voltaste a criar uma regra para a mesma app, não a duplico.
    if (list.some((r) => appKey(r.app) === key)) {
      toast(`Já há um estilo para ${rule.app}: muda-o na lista.`, "error");
      return null;
    }
    const after = next ? list.findIndex((r) => appKey(r.app) === appKey(next)) : -1;
    place.at = after >= 0 ? after : Math.min(index, list.length);
    const copy = list.slice();
    copy.splice(place.at, 0, rule);
    return copy;
  });
  if (ok) $("style-list").children[place.at]?.querySelector<HTMLElement>("select")?.focus();
}

function styleSelect(value: Style, label: string): HTMLSelectElement {
  const select = document.createElement("select");
  select.setAttribute("aria-label", label);
  for (const s of STYLES) {
    const option = document.createElement("option");
    option.value = s;
    option.textContent = STYLE_LABEL[s];
    select.append(option);
  }
  select.value = value;
  return select;
}

function row(rule: StyleRule): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "style-row";

  const name = document.createElement("code");
  name.className = "style-app";
  name.textContent = rule.app; // textContent: nunca interpretado como HTML

  const key = appKey(rule.app);
  const select = styleSelect(rule.style, `Estilo para ${rule.app}`);
  select.addEventListener("change", () => {
    const style = select.value as Style;
    void edit((list) => {
      const i = list.findIndex((r) => appKey(r.app) === key);
      return i < 0 ? null : list.map((r, j) => (j === i ? { ...r, style } : r));
    });
  });

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "icon-button remove";
  remove.setAttribute("aria-label", `Apagar o estilo de ${rule.app}`);
  remove.innerHTML = `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>`;
  remove.addEventListener("click", async () => {
    const gone = await removeRule(rule.app);
    if (!gone) return;
    const undo = toast(`Estilo de ${gone.rule.app} apagado`, "ok", {
      label: "Desfazer",
      run: () => void restoreRule(gone),
    });
    // O botão "Apagar" desapareceu com a linha: o foco vai para o "Desfazer".
    undo?.focus();
  });

  li.append(name, select, remove);
  return li;
}

function renderList() {
  const list = rules();
  const json = JSON.stringify(list);
  if (json === rendered) return;
  rendered = json;
  const ul = $("style-list");
  if (!list.length) {
    const li = document.createElement("li");
    li.className = "pairs-empty muted";
    li.textContent = "Ainda não há estilos: em todas as apps uso o nível de limpeza escolhido.";
    ul.replaceChildren(li);
    return;
  }
  ul.replaceChildren(...list.map(row));
}

/** Botões com as apps recentes que ainda não têm regra: um clique preenche o nome. */
function renderRecent() {
  const taken = new Set(rules().map((r) => appKey(r.app)));
  const free = recent.filter((a) => !taken.has(appKey(a)));
  $("style-recent").hidden = free.length === 0;
  $("style-recent-apps").replaceChildren(
    ...free.map((name) => {
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "chip";
      chip.textContent = name;
      chip.addEventListener("click", () => {
        $<HTMLInputElement>("style-app").value = name;
        clearError();
        // Falta só escolher o estilo.
        $("style-new").focus();
      });
      return chip;
    }),
  );
}

function render() {
  $("styles-off").hidden = app.view.config.cleanup_level !== "off";
  renderList();
  renderRecent();
}

function clearError() {
  $("style-error").hidden = true;
  const input = $("style-app");
  input.removeAttribute("aria-invalid");
  input.removeAttribute("aria-describedby");
}

/** A mensagem fica por baixo do formulário e o campo fica marcado e com o foco. */
function invalid(message: string) {
  const error = $("style-error");
  const input = $("style-app");
  error.textContent = message;
  error.hidden = false;
  input.setAttribute("aria-invalid", "true");
  input.setAttribute("aria-describedby", error.id);
  input.focus();
}

/** Volta a pedir as apps recentes (quando o separador abre). */
export async function refreshRecentApps() {
  recent = await api.recentApps().catch(() => []);
  renderRecent();
}

export function setupStyles() {
  const form = $<HTMLFormElement>("style-add");
  const input = $<HTMLInputElement>("style-app");
  const select = $<HTMLSelectElement>("style-new");
  input.addEventListener("input", clearError);

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    clearError();
    const name = input.value.trim().toLowerCase();
    if (!appKey(name)) return invalid("Escreve o nome da app (ex.: whatsapp.exe).");
    if (rules().some((r) => appKey(r.app) === appKey(name))) {
      return invalid(`"${name}" já tem um estilo: muda-o na lista.`);
    }
    // As novas ficam no topo, logo por baixo do formulário, onde se vêem.
    const style = select.value as Style;
    if (await edit((list) => [{ app: name, style }, ...list])) {
      input.value = "";
      input.focus();
    }
  });

  onViewChange(render);
  render();
}
