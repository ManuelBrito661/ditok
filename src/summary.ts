// O que a página Ficheiros e o histórico partilham nos resumos: os tipos que se
// podem pedir, o tamanho do texto (que decide se vale a pena) e a chamada.

import type { SummaryKind, SummaryOut } from "./api";
import { usd } from "./format";
import { app } from "./store";

/** O mesmo limite de summarize.rs (MAX_CHARS). */
export const MAX_CHARS = 120_000;

export const SUMMARY_KINDS: { value: SummaryKind; label: string }[] = [
  { value: "lesson", label: "Apontamentos de aula" },
  { value: "meeting", label: "Notas de reunião" },
  { value: "keypoints", label: "Pontos-chave" },
  { value: "short", label: "Resumo curto" },
];

export const wordCount = (text: string) => text.split(/\s+/).filter(Boolean).length;

const pt = (n: number) => n.toLocaleString("pt-PT");

/** O modelo que vai resumir: o escolhido nas definições ou o da limpeza. */
export const summaryModel = () => app.view.config.summary_model.trim() || app.view.config.cleanup_model;

/** "1 250 palavras (~1 700 tokens) · modelo x", ou o motivo de não se poder resumir. */
export function describeText(text: string): { hint: string; ok: boolean } {
  const chars = text.length;
  if (!text.trim()) return { hint: "Não há texto para resumir.", ok: false };
  if (chars > MAX_CHARS) {
    return { hint: `O texto tem ${pt(chars)} carateres e o limite são ${pt(MAX_CHARS)}.`, ok: false };
  }
  // Cerca de 4 carateres por token (uma estimativa, serve para ter uma ideia).
  const tokens = Math.max(100, Math.round(chars / 4 / 100) * 100);
  return {
    hint: `${pt(wordCount(text))} palavras (~${pt(tokens)} tokens) · modelo ${summaryModel()}. O texto vai à xAI.`,
    ok: true,
  };
}

/** "0,003 US$ · 8 s". */
export function describeResult(out: SummaryOut): string {
  const parts = [`modelo ${out.model}`, `${Math.max(1, Math.round(out.seconds))} s`];
  if (out.cost_usd != null) parts.push(usd(out.cost_usd, 4));
  return parts.join(" · ");
}
