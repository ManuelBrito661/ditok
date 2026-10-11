// Página Dicionário: palavras, expansões e comandos falados. Quem os aplica é o
// backend (src-tauri/src/text.rs), depois da transcrição; aqui só se editam as
// listas, que são gravadas no config.json como as outras definições.

import { $, app, onViewChange, save, toast } from "./store";
import type { Config } from "./api";

/** As duas listas têm a mesma forma: pares de texto. */
type Pair = [string, string];

interface ListSpec {
  id: "dict" | "snippet";
  get: () => Pair[];
  patch: (pairs: Pair[]) => Partial<Config>;
  placeholders: Pair;
  labels: Pair;
  /** O segundo campo pode ter várias linhas (o texto de uma expansão). */
  multiline: boolean;
  empty: string;
  /** Mensagem quando falta um campo obrigatório. */
  missing: string;
}

const SPECS: ListSpec[] = [
  {
    id: "dict",
    get: () => app.view.config.dictionary.map((r) => [r.from, r.to]),
    patch: (pairs) => ({ dictionary: pairs.map(([from, to]) => ({ from, to })) }),
    placeholders: ["a própria palavra", "Escrito"],
    labels: ["Como o modelo escreve", "Como deve ficar escrito"],
    multiline: false,
    empty: "Ainda não há palavras.",
    missing: "Escreve como a palavra deve ficar.",
  },
  {
    id: "snippet",
    get: () => app.view.config.snippets.map((s) => [s.trigger, s.text]),
    patch: (pairs) => ({ snippets: pairs.map(([trigger, text]) => ({ trigger, text })) }),
    placeholders: ["Frase", "Texto"],
    labels: ["Frase que dizes", "Texto que é colado"],
    multiline: true,
    empty: "Ainda não há expansões.",
    missing: "Uma expansão precisa da frase e do texto.",
  },
];

/** O que está desenhado em cada lista. Depois de uma edição feita aqui, a lista
 *  no ecrã já está certa: redesenhá-la tirava o foco do campo seguinte. */
const rendered: Record<ListSpec["id"], string> = { dict: "", snippet: "" };

/** As formas procuradas no texto, como em `Replacement::variants` (text.rs). */
function variants(spec: ListSpec, p: Pair): string[] {
  const forms = spec.multiline ? [p[0]] : p[0].split(",");
  const clean = forms.map((f) => f.trim().toLowerCase()).filter(Boolean);
  return clean.length ? clean : [p[1].trim().toLowerCase()];
}

function field(spec: ListSpec, which: 0 | 1, value: string): HTMLInputElement | HTMLTextAreaElement {
  const el = which === 1 && spec.multiline ? document.createElement("textarea") : document.createElement("input");
  if (el instanceof HTMLTextAreaElement) el.rows = 1;
  el.value = value; // .value: o texto nunca é interpretado como HTML
  el.placeholder = spec.placeholders[which];
  el.setAttribute("aria-label", spec.labels[which]);
  el.spellcheck = false;
  return el;
}

function arrow(): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = "arrow";
  span.setAttribute("aria-hidden", "true");
  span.textContent = "→";
  return span;
}

function row(spec: ListSpec, pair: Pair, index: number): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "pair";
  const a = field(spec, 0, pair[0]);
  const b = field(spec, 1, pair[1]);

  const commit = async () => {
    const pairs = spec.get().slice();
    pairs[index] = [a.value.trim(), b.value.trim()];
    if (JSON.stringify(pairs) === JSON.stringify(spec.get())) return;
    // Se a gravação falhar, a vista volta a ser desenhada com o que está guardado.
    rendered[spec.id] = JSON.stringify(pairs);
    await save(spec.patch(pairs));
  };
  a.addEventListener("change", commit);
  b.addEventListener("change", commit);

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "icon-button remove";
  remove.setAttribute("aria-label", `Apagar "${pair[0] || pair[1]}"`);
  remove.innerHTML = `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6 6 18"/></svg>`;
  remove.addEventListener("click", () => void save(spec.patch(spec.get().filter((_, i) => i !== index))));

  li.append(a, arrow(), b, remove);
  return li;
}

function render(spec: ListSpec) {
  const pairs = spec.get();
  $(`${spec.id}-count`).textContent = pairs.length ? String(pairs.length) : "";
  const json = JSON.stringify(pairs);
  if (json === rendered[spec.id]) return;
  rendered[spec.id] = json;

  const list = $(`${spec.id}-list`);
  if (!pairs.length) {
    const li = document.createElement("li");
    li.className = "pairs-empty muted";
    li.textContent = spec.empty;
    list.replaceChildren(li);
    return;
  }
  list.replaceChildren(...pairs.map((p, i) => row(spec, p, i)));
}

function setupAdd(spec: ListSpec) {
  const form = $<HTMLFormElement>(`${spec.id}-add`);
  const a = $<HTMLInputElement>(`${spec.id}-a`);
  const b = $<HTMLInputElement | HTMLTextAreaElement>(`${spec.id}-b`);

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const pair: Pair = [a.value.trim(), b.value.trim()];
    if (!pair[1] || (spec.multiline && !pair[0])) {
      toast(spec.missing, "error");
      return;
    }
    const taken = new Set(spec.get().flatMap((p) => variants(spec, p)));
    const repeated = variants(spec, pair).find((v) => taken.has(v));
    if (repeated) {
      toast(`"${repeated}" já está na lista.`, "error");
      return;
    }
    // As novas ficam no topo, logo por baixo do formulário, onde se vêem.
    if (await save(spec.patch([pair, ...spec.get()]))) {
      a.value = "";
      b.value = "";
      a.focus();
    }
  });
  // Numa caixa de várias linhas o Enter muda de linha; Ctrl+Enter acrescenta.
  b.addEventListener("keydown", (e) => {
    if (e instanceof KeyboardEvent && e.key === "Enter" && (e.ctrlKey || e.metaKey)) form.requestSubmit();
  });
}

function renderAll() {
  for (const spec of SPECS) render(spec);
  $<HTMLInputElement>("voice-commands").checked = app.view.config.voice_commands;
}

export function setupDictionary() {
  for (const spec of SPECS) setupAdd(spec);
  $<HTMLInputElement>("voice-commands").addEventListener("change", (e) =>
    void save({ voice_commands: (e.target as HTMLInputElement).checked }));
  onViewChange(renderAll);
  renderAll();
}
