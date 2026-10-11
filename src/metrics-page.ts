// Página Métricas: percentis, custos por mês, orçamento, o que resta na chave e RAM.

import { api, type MetricsRow, type Stats } from "./api";
import { budgetLevel, dateTime, ms, usd } from "./format";
import { openSettings } from "./settings-dialog";
import { $, app, onViewChange } from "./store";

let stats: Stats | null = null;

export async function renderStats() {
  const s = await api.getStats();
  stats = s;
  const month = s.months[s.current_month];
  $("m-cost").textContent = usd(month?.cost_usd ?? 0);
  $("m-count").textContent = String(month?.dictations ?? 0);
  const pair = (p: typeof s.total) => (p ? `${ms(p.p50)} / ${ms(p.p90)}` : "—");
  $("m-total").textContent = pair(s.total);
  $("m-stt").textContent = pair(s.stt);
  $("m-cleanup").textContent = pair(s.cleanup);
  renderMoney();

  $("recent").replaceChildren(
    ...s.recent.map((r) => {
      const tr = document.createElement("tr");
      const cost = (r.cost_usd ?? 0) + (r.cleanup_cost_usd ?? 0);
      const cells = [dateTime(r.at), r.model, ms(r.audio_ms), ms(r.stt_ms), ms(r.cleanup_ms),
        r.error ? "erro" : ms(r.total_ms), usd(r.cost_usd == null && r.cleanup_cost_usd == null ? null : cost)];
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

  $("months").replaceChildren(
    ...Object.entries(s.months).reverse().map(([m, t]) => {
      const tr = document.createElement("tr");
      for (const [i, c] of [m, String(t.dictations), usd(t.cost_usd)].entries()) {
        const td = document.createElement("td");
        td.textContent = c;
        if (i > 0) td.className = "num";
        tr.append(td);
      }
      return tr;
    }),
  );
}

/** Custo por minuto de áudio nos últimos ditados com o modelo atual (transcrição
 *  + limpeza), ou `null` se ainda não houver nenhum. */
function costPerMinute(rows: MetricsRow[]): number | null {
  const model = app.view.config.stt_model;
  let cost = 0;
  let audioMs = 0;
  for (const r of rows) {
    if (r.model !== model || r.cost_usd == null) continue;
    cost += r.cost_usd + (r.cleanup_cost_usd ?? 0);
    audioMs += r.audio_ms;
  }
  return audioMs > 0 && cost > 0 ? (cost / audioMs) * 60_000 : null;
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
    sub.textContent = "Não foi possível perguntar ao OpenRouter.";
  } else if (key.limit_remaining == null) {
    $("m-key").textContent = "Sem limite";
    sub.textContent = `A chave não tem limite de gasto · gastou ${usd(key.usage ?? 0, 2)}`;
  } else {
    $("m-key").textContent = usd(key.limit_remaining, 2);
    const perMin = costPerMinute(stats?.recent ?? []);
    const minutes = perMin ? ` · ≈ ${Math.floor(key.limit_remaining / perMin)} min de ditado ao custo atual` : "";
    sub.textContent = `de ${usd(key.limit, 2)}${minutes}`;
  }
}

export async function renderMemory() {
  const m = await api.getMemory();
  $("m-memory").textContent = m ? `${m.app_mb.toFixed(0)} MB / ${m.webview_mb.toFixed(0)} MB` : "—";
  if (m) $("m-memory").title = `WebView: ${m.webview_processes} processos`;
}

export function setupMetrics() {
  onViewChange(renderMoney);
}
