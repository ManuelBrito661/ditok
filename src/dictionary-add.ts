// Acrescentar uma palavra ao dicionário a partir de outro sítio (o editor do
// histórico). A página Dicionário tem as suas próprias verificações; as regras
// são as mesmas: sem repetidas, e as formas procuradas comparam-se em minúsculas
// (ver `Replacement::variants` em text.rs).

import { app, confirmDanger, save, toast } from "./store";

/** As formas que o dicionário procura: as de `from`, ou a própria palavra se estiver vazio. */
function variantsOf(from: string, to: string): string[] {
  const forms = from.split(",").map((f) => f.trim().toLowerCase()).filter(Boolean);
  return forms.length ? forms : [to.trim().toLowerCase()];
}

/** Quantas vezes `word` aparece, como palavra inteira, nos textos dados. */
export function countWordIn(texts: string[], word: string): number {
  const escaped = word.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(`(^|[^\\p{L}\\p{N}_])${escaped}($|[^\\p{L}\\p{N}_])`, "iu");
  return texts.filter((t) => re.test(t)).length;
}

/**
 * Acrescenta "como sai" → "como deve ficar". `from` pode ter várias formas separadas
 * por vírgulas; vazio serve para ensinar a palavra e acertar as maiúsculas.
 * `occurrences`: em quantos ditados do histórico a forma errada aparece, para avisar
 * quando o dicionário a ia trocar em muitos sítios (ele troca-a SEMPRE, em qualquer frase).
 * Devolve se acrescentou.
 */
export async function addDictionaryEntry(
  fromText: string,
  toText: string,
  occurrences: (form: string) => number,
): Promise<boolean> {
  const to = toText.trim();
  const from = fromText.split(",").map((f) => f.trim()).filter(Boolean).join(", ");
  if (!to) {
    toast("Escreve como a palavra deve ficar.", "error");
    return false;
  }
  const dictionary = app.view.config.dictionary;
  const forms = variantsOf(from, to);
  for (const entry of dictionary) {
    const taken = variantsOf(entry.from, entry.to);
    const clash = forms.find((f) => taken.includes(f));
    if (!clash) continue;
    toast(
      entry.to === to
        ? "Já está no dicionário."
        : `"${clash}" já está no dicionário como “${entry.to}”. Edita-a na página Dicionário.`,
      "error",
    );
    return false;
  }
  // Uma forma curta ou comum trocava-se em todos os ditados, certos ou errados.
  if (from) {
    const risky = forms.find((f) => f.length <= 3 || occurrences(f) >= 3);
    if (risky) {
      const n = occurrences(risky);
      const ok = await confirmDanger(
        `Trocar “${risky}” em todos os ditados?`,
        `O dicionário troca esta palavra sempre, em qualquer frase${
          n ? ` (aparece em ${n} ${n === 1 ? "ditado" : "ditados"} do histórico)` : ""
        }. Só compensa se o modelo a escreve sempre mal.`,
        "Adicionar",
      );
      if (!ok) return false;
    }
  }
  const before = dictionary;
  if (!(await save({ dictionary: [{ from, to }, ...dictionary] }))) return false;
  const undo = toast(`“${to}” adicionada ao dicionário`, "ok", {
    label: "Desfazer",
    run: () => void save({ dictionary: before }),
  });
  undo?.focus();
  return true;
}

/**
 * Adivinha como o modelo escreveu a palavra que acabaste de corrigir: se o texto
 * original e o corrigido têm o mesmo número de palavras, é a que estava na mesma
 * posição. Com um número diferente (acrescentaste ou tiraste palavras) não adivinha.
 */
export function suggestFrom(original: string, edited: string, start: number, end: number): string {
  const selected = edited.slice(start, end).trim();
  if (!selected || /\s/.test(selected)) return "";
  // A seleção tem de começar numa palavra, para contar quantas há antes dela.
  if (start > 0 && !/\s/.test(edited[start - 1])) return "";
  const words = (s: string) => s.split(/\s+/).filter(Boolean);
  const before = words(original);
  const after = words(edited);
  if (before.length !== after.length) return "";
  const edge = /^[^\p{L}\p{N}]+|[^\p{L}\p{N}]+$/gu;
  const index = words(edited.slice(0, start)).length;
  const was = (before[index] ?? "").replace(edge, "");
  const now = selected.replace(edge, "");
  return was && was.toLowerCase() !== now.toLowerCase() ? was : "";
}
