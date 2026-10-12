// Página Métricas: tempos (o normal e o mais lento em cada 10), custos por mês,
// orçamento, o que resta na chave e RAM.

import { api, errorText, type Percentiles, type Stats, type UsageSummary } from "./api";
import { budgetLevel, costPerMinute, dateTime, duration, monthLabel, ms, usd } from "./format";
import { openSettings } from "./settings-dialog";
import { $, app, confirmDanger, onViewChange, save, toast } from "./store";

let stats: Stats | null = null;

/** Uma linha que ocupa a tabela toda: "ainda não há" ou o erro. */
function messageRow(table: string, text: string, failed = false) {
  const columns = $(table).closest("table")!.querySelectorAll("thead th").length;
  const tr = document.createElement("tr");
  const td = document.createElement("td");
  td.colSpan = columns;
  td.className = failed ? "table-empty failed" : "table-empty";
  td.textContent = text;
  tr.append(td);
  $(table).replaceChildren(tr);
}

/** Valor grande = o normal (p50); por baixo, o tempo que 9 em cada 10 não passam (p90). */
function timing(id: string, p: Percentiles | null) {
  $(id).textContent = p ? ms(p.p50) : "—";
  $(`${id}-sub`).textContent = p ? `normal · 9 em 10 até ${ms(p.p90)}` : "";
}

export async function renderStats() {
  let s: Stats;
  try {
    s = await api.getStats();
  } catch (e) {
    const why = `Não consegui ler as métricas: ${errorText(e)}`;
    messageRow("recent", why, true);
    messageRow("months", why, true);
    return;
  }
  stats = s;
  const month = s.months[s.current_month];
  $("m-cost").textContent = usd(month?.cost_usd ?? 0);
  $("m-count").textContent = String(month?.dictations ?? 0);
  timing("m-total", s.total);
  timing("m-stt", s.stt);
  timing("m-cleanup", s.cleanup);
  renderMoney();

  if (!s.recent.length) messageRow("recent", "Ainda não há ditados.");
  else $("recent").replaceChildren(
    ...s.recent.map((r) => {
      const tr = document.createElement("tr");
      const cost = (r.cost_usd ?? 0) + (r.cleanup_cost_usd ?? 0);
      // Um ficheiro não tem latência: mostra a duração e "ficheiro" no total.
      const audio = r.file ? duration(r.audio_ms / 1000) : ms(r.audio_ms);
      const total = r.error ? "erro" : r.file ? "ficheiro" : r.summary ? "resumo" : ms(r.total_ms);
      // Um comando é um ditado com uma instrução: o mesmo, com a marca ao lado do modelo.
      const model = r.command ? `${r.model} · comando` : r.summary ? `${r.model} · resumo` : r.model;
      const cells = [dateTime(r.at), model, audio, ms(r.stt_ms), ms(r.cleanup_ms), total,
        usd(r.cost_usd == null && r.cleanup_cost_usd == null ? null : cost)];
      cells.forEach((c, i) => {
        const td = document.createElement("td");
        td.textContent = c;
        if (i >= 2) td.className = "num";
        if (r.error && i === 5) td.title = r.error;
        tr.append(td);
      });
      if (r.error) tr.className = "row-error";
      return tr;
    }),
  );

  const months = Object.entries(s.months).reverse();
  if (!months.length) messageRow("months", "Ainda não há ditados.");
  else $("months").replaceChildren(
    ...months.map(([m, t]) => {
      const tr = document.createElement("tr");
      for (const [i, c] of [monthLabel(m), String(t.dictations), usd(t.cost_usd)].entries()) {
        const td = document.createElement("td");
        td.textContent = c;
        if (i > 0) td.className = "num";
        tr.append(td);
      }
      return tr;
    }),
  );
}

function setupLink(text: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "link";
  b.textContent = text;
  b.addEventListener("click", onClick);
  return b;
}

/** Os cartões do orçamento e da chave: dependem das métricas, das definições e
 *  do pedido à chave, por isso são redesenhados quando qualquer um muda. */
function renderMoney() {
  const spent = stats?.months[stats.current_month]?.cost_usd ?? 0;
  const budget = app.view.config.monthly_budget_usd;
  const meter = $("m-budget-meter");
  meter.hidden = !budget;
  if (budget) {
    const level = budgetLevel(spent, budget);
    $("m-budget").textContent = `${Math.floor((spent / budget) * 100)} %`;
    meter.className = `meter ${level}`;
    $("m-budget-bar").style.width = `${Math.min(100, (spent / budget) * 100)}%`;
    $("m-budget-sub").replaceChildren(`${usd(spent, 2)} de ${usd(budget, 2)}`);
  } else {
    $("m-budget").textContent = "—";
    $("m-budget-sub").replaceChildren(setupLink("Definir um orçamento", () => openSettings("transcricao")));
  }

  const key = app.key;
  const sub = $("m-key-sub");
  if (!app.view.api_key_set) {
    $("m-key").textContent = "—";
    sub.replaceChildren(setupLink("Configurar a chave", () => openSettings("transcricao")));
  } else if (!key) {
    $("m-key").textContent = "—";
    sub.textContent = "Não consegui perguntar ao OpenRouter.";
  } else if (key.limit_remaining == null) {
    $("m-key").textContent = "Sem limite";
    sub.textContent = `A chave não tem limite de gasto · gastou ${usd(key.usage ?? 0, 2)}`;
  } else {
    $("m-key").textContent = usd(key.limit_remaining, 2);
    // Só os ditados (com a limpeza, se estiver ligada), não os ficheiros.
    const dictations = (stats?.recent ?? []).filter((r) => !r.file);
    const perMin = costPerMinute(dictations, app.view.config.stt_model, true);
    const minutes = perMin ? ` · ≈ ${Math.floor(key.limit_remaining / perMin).toLocaleString("pt-PT")} min de ditado ao custo atual` : "";
    sub.textContent = `de ${usd(key.limit, 2)}${minutes}`;
  }
}

export async function renderMemory() {
  const m = await api.getMemory();
  $("m-memory").textContent = m ? `${Math.round(m.app_mb)} MB / ${Math.round(m.webview_mb)} MB` : "—";
  if (m) $("m-memory").title = `WebView: ${m.webview_processes} processos`;
}

export function setupMetrics() {
  onViewChange(renderMoney);
  setupUsage();
}

// ---------------------------------------------------------------- uso

const pt = (n: number) => n.toLocaleString("pt-PT");
const days = (n: number) => `${n} ${n === 1 ? "dia" : "dias"}`;

/** "12 de outubro de 2026" a partir de "2026-10-12", na hora do computador. */
function dayLabel(iso: string): string {
  const [y, m, d] = iso.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString("pt-PT", { day: "numeric", month: "long", year: "numeric" });
}

/** A grelha abre nas semanas mais recentes (as mais antigas ficam à esquerda, a rolar).
 *  Numa página escondida a largura é 0: por isso também se chama ao mostrá-la. */
export function scrollHeatToEnd() {
  const scroller = $("u-heat").parentElement;
  if (scroller) scroller.scrollLeft = scroller.scrollWidth;
}

export async function renderUsage() {
  let u: UsageSummary;
  try {
    u = await api.usageSummary();
  } catch (e) {
    $("u-words").textContent = "—";
    $("u-words-sub").textContent = `Não consegui ler: ${errorText(e)}`;
    return;
  }
  $("u-words").textContent = pt(u.total_words);
  $("u-words-sub").textContent = u.total_words
    ? `${pt(u.month_words)} este mês · ${pt(u.total_dictations)} ditados`
    : "Ainda não há palavras.";
  $("u-streak").textContent = days(u.streak_current);
  $("u-streak-sub").textContent = `a melhor: ${days(u.streak_best)}`;
  $("u-wpm").textContent = u.wpm ? String(Math.round(u.wpm)) : "—";
  $("u-saved").textContent = u.saved_minutes >= 1 ? duration(u.saved_minutes * 60) : "—";
  $("u-saved-sub").textContent =
    `face a escrever a ${u.typing_wpm} palavras por minuto${u.estimated ? " · estimativa" : ""}`;
  $("u-estimated").hidden = !u.estimated;

  // Uma célula por dia, semana a semana (a grelha CSS põe 7 por coluna, segunda a domingo).
  $("u-heat").replaceChildren(
    ...u.heat.map((cell) => {
      const div = document.createElement("div");
      div.className = `heat-cell l${cell.level}${cell.future ? " future" : ""}`;
      if (!cell.future) div.title = `${dayLabel(cell.date)}: ${pt(cell.words)} ${cell.words === 1 ? "palavra" : "palavras"}`;
      return div;
    }),
  );
  scrollHeatToEnd();

  const max = Math.max(1, ...u.top_apps.map((a) => a.words));
  if (!u.top_apps.length) {
    const li = document.createElement("li");
    li.className = "pairs-empty muted";
    li.textContent = "Ainda não sei em que apps ditas: conto a partir de agora.";
    $("u-apps").replaceChildren(li);
  } else {
    $("u-apps").replaceChildren(
      ...u.top_apps.map((a) => {
        const li = document.createElement("li");
        const name = document.createElement("span");
        name.textContent = a.app;
        const bar = document.createElement("span");
        bar.className = "app-bar";
        const fill = document.createElement("span");
        fill.style.width = `${Math.max(2, (a.words / max) * 100)}%`;
        bar.append(fill);
        const words = document.createElement("span");
        words.className = "app-words muted";
        words.textContent = pt(a.words);
        li.append(name, bar, words);
        return li;
      }),
    );
  }
  // Não mexe no campo enquanto escreves nele.
  const typing = $<HTMLInputElement>("u-typing");
  if (document.activeElement !== typing) typing.value = String(u.typing_wpm);
}

function setupUsage() {
  $<HTMLInputElement>("u-typing").addEventListener("change", async (e) => {
    const value = Number((e.target as HTMLInputElement).value);
    if (await save({ typing_wpm: Math.round(value) })) void renderUsage();
    else void renderUsage(); // repõe o valor guardado
  });
  $("u-clear").addEventListener("click", async () => {
    const ok = await confirmDanger(
      "Apagar as estatísticas de uso?",
      "As palavras, a sequência e o mapa de calor voltam a zero. Os custos, os tempos e o histórico ficam.",
      "Apagar",
    );
    if (!ok) return;
    try {
      await api.usageClear();
      toast("Estatísticas apagadas");
    } catch (e) {
      toast(errorText(e), "error");
    }
    void renderUsage();
  });
}
