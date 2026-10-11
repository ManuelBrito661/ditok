// Página Métricas: percentis, custos por mês e RAM.

import { api } from "./api";
import { dateTime, ms, usd } from "./format";
import { $ } from "./store";

export async function renderStats() {
  const s = await api.getStats();
  const month = s.months[s.current_month];
  $("m-cost").textContent = usd(month?.cost_usd ?? 0);
  $("m-count").textContent = String(month?.dictations ?? 0);
  const pair = (p: typeof s.total) => (p ? `${ms(p.p50)} / ${ms(p.p90)}` : "—");
  $("m-total").textContent = pair(s.total);
  $("m-stt").textContent = pair(s.stt);
  $("m-cleanup").textContent = pair(s.cleanup);

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

export async function renderMemory() {
  const m = await api.getMemory();
  $("m-memory").textContent = m ? `${m.app_mb.toFixed(0)} MB / ${m.webview_mb.toFixed(0)} MB` : "—";
  if (m) $("m-memory").title = `WebView: ${m.webview_processes} processos`;
}
