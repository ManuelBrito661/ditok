// Página Dicionário: palavras, expansões e comandos falados. Quem os aplica é o
// backend (src-tauri/src/text.rs), depois da transcrição; aqui só se editam as
// listas, que são gravadas no config.json como as outras definições.

import { $, app, onViewChange, save, toast } from "./store";
import { api, errorText, type Config, type DictFormat, type ImportPreview } from "./api";

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
  /** Nome de uma entrada no aviso "Apagada". */
  noun: string;
}

const SPECS: ListSpec[] = [
  {
    id: "dict",
    get: () => app.view.config.dictionary.map((r) => [r.from, r.to]),
    patch: (pairs) => ({ dictionary: pairs.map(([from, to]) => ({ from, to })) }),
    // Os mesmos textos no formulário de cima (index.html) e em cada linha.
    placeholders: ["Como sai (opcional)", "Como deve ficar"],
    labels: ["Como o modelo escreve", "Como deve ficar escrito"],
    multiline: false,
    empty: "Ainda não há palavras.",
    missing: "Escreve como a palavra deve ficar.",
    noun: "Palavra apagada",
  },
  {
    id: "snippet",
    get: () => app.view.config.snippets.map((s) => [s.trigger, s.text]),
    patch: (pairs) => ({ snippets: pairs.map(([trigger, text]) => ({ trigger, text })) }),
    placeholders: ["Frase que dizes", "Texto a colar"],
    labels: ["Frase que dizes", "Texto a colar"],
    multiline: true,
    empty: "Ainda não há expansões.",
    missing: "Uma expansão precisa da frase e do texto.",
    noun: "Expansão apagada",
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
  remove.addEventListener("click", async () => {
    const removed = spec.get()[index];
    // A entrada que vinha a seguir: o "Desfazer" repõe antes dela, mesmo que entretanto
    // se tenha acrescentado outra no topo (só o índice ficaria desviado).
    const next = spec.get()[index + 1];
    if (!(await save(spec.patch(spec.get().filter((_, i) => i !== index))))) return;
    // Sem confirmação, mas com "Desfazer" (só da última entrada apagada).
    const undo = toast(spec.noun, "ok", {
      label: "Desfazer",
      run: async () => {
        const pairs = spec.get().slice();
        const at = next ? pairs.findIndex((p) => p[0] === next[0] && p[1] === next[1]) : -1;
        const pos = at >= 0 ? at : Math.min(index, pairs.length);
        pairs.splice(pos, 0, removed);
        if (!(await save(spec.patch(pairs)))) return;
        // O "Desfazer" escondeu-se: o foco vai para a entrada reposta.
        $(`${spec.id}-list`).children[pos]?.querySelector<HTMLElement>("input, textarea")?.focus();
      },
    });
    // O botão "Apagar" desapareceu com a linha: o foco vai para o "Desfazer".
    undo?.focus();
  });

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
  const error = $(`${spec.id}-error`);

  /** A mensagem fica por baixo do formulário, o campo em causa fica marcado e com o foco. */
  const invalid = (field: HTMLElement, message: string) => {
    error.textContent = message;
    error.hidden = false;
    field.setAttribute("aria-invalid", "true");
    field.setAttribute("aria-describedby", error.id);
    field.focus();
  };
  const clear = () => {
    error.hidden = true;
    for (const f of [a, b]) {
      f.removeAttribute("aria-invalid");
      f.removeAttribute("aria-describedby");
    }
  };
  a.addEventListener("input", clear);
  b.addEventListener("input", clear);

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    clear();
    const pair: Pair = [a.value.trim(), b.value.trim()];
    if (spec.multiline && !pair[0]) return invalid(a, spec.missing);
    if (!pair[1]) return invalid(b, spec.missing);
    const taken = new Set(spec.get().flatMap((p) => variants(spec, p)));
    const repeated = variants(spec, pair).find((v) => taken.has(v));
    if (repeated) return invalid(pair[0] ? a : b, `"${repeated}" já está na lista.`);
    // As novas ficam no topo, logo por baixo do formulário, onde se vêem.
    if (await save(spec.patch([pair, ...spec.get()]))) {
      a.value = "";
      b.value = "";
      a.focus();
    }
  });
  // Numa caixa de várias linhas o Enter muda de linha; Ctrl+Enter acrescenta (dito na descrição da secção).
  b.addEventListener("keydown", (e) => {
    if (e instanceof KeyboardEvent && e.key === "Enter" && (e.ctrlKey || e.metaKey)) form.requestSubmit();
  });
}

function renderAll() {
  for (const spec of SPECS) render(spec);
  $<HTMLInputElement>("voice-commands").checked = app.view.config.voice_commands;
  $<HTMLInputElement>("spoken-punctuation").checked = app.view.config.spoken_punctuation;
}

export function setupDictionary() {
  for (const spec of SPECS) setupAdd(spec);
  $<HTMLInputElement>("voice-commands").addEventListener("change", (e) =>
    void save({ voice_commands: (e.target as HTMLInputElement).checked }));
  $<HTMLInputElement>("spoken-punctuation").addEventListener("change", (e) =>
    void save({ spoken_punctuation: (e.target as HTMLInputElement).checked }));
  setupImportExport();
  onViewChange(renderAll);
  renderAll();
}

// ---------------------------------------------------------------- importar e exportar

/** O ficheiro escolhido e as listas que existiam nesse momento (ver `confirmImport`). */
let pending: { preview: ImportPreview; baseline: string } | null = null;

const listsKey = () => JSON.stringify([app.view.config.dictionary, app.view.config.snippets]);

const importMode = (): "keep" | "replace" =>
  document.querySelector<HTMLInputElement>('input[name="dict-conflict-mode"]:checked')?.value === "replace"
    ? "replace"
    : "keep";

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** Um valor comprido (o texto de uma expansão) cabe numa linha do aviso. */
const short = (s: string) => {
  const line = s.replace(/\s+/g, " ").trim();
  return line.length > 40 ? `${line.slice(0, 40)}…` : line;
};

/** O resumo e o botão seguem o modo escolhido para os conflitos. */
function renderImport() {
  if (!pending) return;
  const { preview } = pending;
  const mode = importMode();
  const merged = preview[mode];
  const parts = [plural(merged.new, "nova", "novas"), plural(merged.duplicate, "já existia", "já existiam")];
  if (merged.replaced) parts.push(plural(merged.replaced, "trocada", "trocadas"));
  if (merged.conflicts.length && mode === "keep") {
    parts.push(plural(merged.conflicts.length, "em conflito fica como está", "em conflito ficam como estão"));
  }
  if (preview.invalid) parts.push(plural(preview.invalid, "ignorada por não servir", "ignoradas por não servirem"));
  $("dict-import-summary").textContent = parts.join(" · ");
  $<HTMLButtonElement>("dict-import-confirm").disabled = merged.new + merged.replaced === 0;
}

function openImport(preview: ImportPreview) {
  pending = { preview, baseline: listsKey() };
  $("dict-import-title").textContent = preview.file_name;

  const conflicts = preview.keep.conflicts;
  $("dict-import-conflicts").hidden = conflicts.length === 0;
  $("dict-conflict-count").textContent = String(conflicts.length);
  const shown = conflicts.slice(0, 8).map((c) => {
    const li = document.createElement("li");
    li.textContent = `${short(c.key)}: tens “${short(c.mine)}”, o ficheiro tem “${short(c.theirs)}”`;
    return li;
  });
  if (conflicts.length > shown.length) {
    const more = document.createElement("li");
    more.className = "muted";
    more.textContent = `… e mais ${conflicts.length - shown.length}`;
    shown.push(more);
  }
  $("dict-conflict-list").replaceChildren(...shown);
  const keep = document.querySelector<HTMLInputElement>('input[name="dict-conflict-mode"][value="keep"]');
  if (keep) keep.checked = true;

  renderImport();
  $("dict-import-panel").hidden = false;
  $("dict-import-title").focus();
}

function closeImport() {
  pending = null;
  $("dict-import-panel").hidden = true;
}

async function confirmImport() {
  if (!pending) return;
  const { preview, baseline } = pending;
  const merged = preview[importMode()];
  closeImport();
  // As listas foram calculadas quando escolheste o ficheiro: se entretanto mudaste
  // alguma coisa, gravá-las apagava essa mudança.
  if (listsKey() !== baseline) {
    toast("As listas mudaram desde que escolheste o ficheiro. Importa outra vez.", "error");
    return;
  }
  const before = { dictionary: app.view.config.dictionary, snippets: app.view.config.snippets };
  if (!(await save({ dictionary: merged.dictionary, snippets: merged.snippets }))) return;
  const added = merged.new + merged.replaced;
  const undo = toast(plural(added, "entrada importada", "entradas importadas"), "ok", {
    label: "Desfazer",
    run: () => void save(before),
  });
  undo?.focus();
}

function setupImportExport() {
  const exportButton = $<HTMLButtonElement>("dict-export");
  exportButton.addEventListener("click", async () => {
    const format = $<HTMLSelectElement>("dict-export-format").value as DictFormat;
    const withSnippets = $<HTMLInputElement>("dict-export-snippets").checked;
    exportButton.disabled = true;
    try {
      if (await api.dictionaryExport(format, withSnippets)) toast("Dicionário exportado");
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      exportButton.disabled = false;
    }
  });

  const importButton = $<HTMLButtonElement>("dict-import");
  importButton.addEventListener("click", async () => {
    importButton.disabled = true;
    try {
      const preview = await api.dictionaryImportPreview();
      if (preview) openImport(preview);
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      importButton.disabled = false;
    }
  });

  $("dict-import-confirm").addEventListener("click", () => void confirmImport());
  $("dict-import-cancel").addEventListener("click", () => {
    closeImport();
    importButton.focus();
  });
  for (const radio of document.querySelectorAll('input[name="dict-conflict-mode"]')) {
    radio.addEventListener("change", renderImport);
  }
}
