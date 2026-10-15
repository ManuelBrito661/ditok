// Funções puras do assistente, sem DOM: partir uma resposta em texto e blocos de código,
// o nome de uma app e o agrupamento das conversas por dia. Ficam à parte para se
// poderem testar sem browser.

export type Block = { kind: "text"; paragraphs: string[] } | { kind: "code"; lang: string; code: string };

const FENCE_OPEN = /^ {0,3}```([\w+#.-]*)[ \t]*$/;
const FENCE_CLOSE = /^ {0,3}```[ \t]*$/;

/** Parte uma resposta em parágrafos e blocos de código (```). Só o código tem formato:
 *  o resto fica tal e qual, porque é isso que se copia e se cola. Uma cerca por fechar
 *  (a resposta ainda está a chegar) conta como código até ao fim. */
export function parseBody(text: string): Block[] {
  const blocks: Block[] = [];
  let prose: string[] = [];
  let code: string[] | null = null;
  let lang = "";

  const flushProse = () => {
    const paragraphs = prose
      .join("\n")
      .split(/\n{2,}/)
      .map((p) => p.replace(/^\n+|\n+$/g, ""))
      .filter((p) => p.trim() !== "");
    if (paragraphs.length) blocks.push({ kind: "text", paragraphs });
    prose = [];
  };

  for (const line of text.replace(/\r\n?/g, "\n").split("\n")) {
    if (code === null) {
      const open = FENCE_OPEN.exec(line);
      if (open) {
        flushProse();
        code = [];
        lang = open[1];
      } else {
        prose.push(line);
      }
    } else if (FENCE_CLOSE.test(line)) {
      blocks.push({ kind: "code", lang, code: code.join("\n") });
      code = null;
    } else {
      code.push(line);
    }
  }
  if (code !== null) blocks.push({ kind: "code", lang, code: code.join("\n") });
  else flushProse();
  return blocks;
}

/** "outlook.exe" → "Outlook"; "microsoft outlook" → "Microsoft Outlook". */
export function prettyApp(app: string): string {
  const name = app.trim().replace(/\.exe$/i, "");
  return name.replace(/(^|[\s_-])(\p{L})/gu, (_, sep: string, letter: string) => sep + letter.toUpperCase());
}

export type DayGroup = "Hoje" | "Ontem" | "Últimos 7 dias" | "Últimos 30 dias" | "Mais antigas";

/** Em que grupo da lista cai uma conversa, pelo dia (no fuso do computador) em que foi mexida. */
export function dayGroup(iso: string, now = new Date()): DayGroup {
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((start(now) - start(new Date(iso))) / 86_400_000);
  if (days <= 0) return "Hoje";
  if (days === 1) return "Ontem";
  if (days <= 7) return "Últimos 7 dias";
  if (days <= 30) return "Últimos 30 dias";
  return "Mais antigas";
}

/** "14:32" se foi hoje, senão "12 out" (com o ano se não for o atual). */
export function shortWhen(iso: string, now = new Date()): string {
  const d = new Date(iso);
  if (dayGroup(iso, now) === "Hoje") {
    return d.toLocaleTimeString("pt-PT", { hour: "2-digit", minute: "2-digit" });
  }
  const month = d.toLocaleDateString("pt-PT", { month: "short" }).replace(/\.$/, "");
  const year = d.getFullYear() === now.getFullYear() ? "" : ` ${d.getFullYear()}`;
  return `${d.getDate()} ${month}${year}`;
}
