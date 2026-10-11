// Página Ficheiros: arrastar (ou escolher) um ficheiro, confirmar com a duração e
// o custo estimado, acompanhar o progresso e ficar com o texto.
// Quem trabalha é o backend (src-tauri/src/files.rs): o trabalho continua mesmo
// que a janela feche, e ao reabri-la `files.status()` devolve o estado atual.

import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, errorText, files, onFileJob, type FileJob, type FileProbe } from "./api";
import { costPerMinute, duration, megabytes, usd } from "./format";
import { $, app, toast } from "./store";

let probe: FileProbe | null = null;
let job: FileJob | null = null;

type Step = "drop" | "confirm" | "job";

function show(step: Step) {
  $("file-drop").hidden = step !== "drop";
  $("file-confirm").hidden = step !== "confirm";
  $("file-job").hidden = step !== "job";
}

// ---------------------------------------------------------------- escolher e confirmar

async function choose(path: string) {
  if (job?.status === "running") {
    toast("Já estou a transcrever um ficheiro. Espera que acabe ou para-o.", "error");
    return;
  }
  try {
    probe = await files.probe(path);
  } catch (e) {
    toast(errorText(e), "error");
    return;
  }
  show("confirm");
  await renderConfirm(probe);
}

/** Custo estimado: ao preço dos teus últimos pedidos com este modelo ou, se não
 *  houver nenhum, ao preço da lista de modelos (USD por segundo de áudio). */
async function estimate(seconds: number, recent: Parameters<typeof costPerMinute>[0]): Promise<number | null> {
  const model = app.view.config.stt_model;
  const perMin = costPerMinute(recent, model, false);
  if (perMin) return (perMin * seconds) / 60;
  const list = await api.listModels("transcription").catch(() => null);
  const m = list?.models.find((x) => x.id === model);
  return m?.prompt_price != null && !m.completion_price ? m.prompt_price * seconds : null;
}

async function renderConfirm(p: FileProbe) {
  const model = app.view.config.stt_model;
  $("fc-name").textContent = p.name;
  $("fc-meta").textContent = [p.duration_s != null ? duration(p.duration_s) : "duração desconhecida", megabytes(p.size_bytes)].join(" · ");
  $("fc-estimate").textContent = "A estimar o custo…";
  $("fc-warning").hidden = true;

  const stats = await api.getStats().catch(() => null);
  const cost = p.duration_s != null ? await estimate(p.duration_s, stats?.recent ?? []) : null;
  if (probe !== p) return; // entretanto escolheste outro ficheiro
  $("fc-estimate").textContent = cost != null
    ? `Custo estimado: ~${usd(cost, 2)} com ${model}.`
    : `Não sei estimar o custo com ${model}.`;

  // Avisos antes de gastar: o orçamento do mês e o que resta na chave.
  const warnings: string[] = [];
  const budget = app.view.config.monthly_budget_usd;
  const spent = stats?.months[stats.current_month]?.cost_usd ?? 0;
  if (cost != null && budget && spent + cost > budget) {
    warnings.push(`Com esta transcrição passas o orçamento do mês (${usd(spent, 2)} + ~${usd(cost, 2)} de ${usd(budget, 2)}).`);
  }
  const left = app.key?.limit_remaining;
  if (cost != null && left != null && cost > left) {
    warnings.push(`A chave só tem ${usd(left, 2)}: a transcrição pode ficar a meio.`);
  }
  $("fc-warning").textContent = warnings.join(" ");
  $("fc-warning").hidden = warnings.length === 0;
}

// ---------------------------------------------------------------- progresso e resultado

const STATUS_LABEL: Record<FileJob["status"], [string, string]> = {
  running: ["A transcrever", "run"],
  done: ["Pronto", "ok"],
  failed: ["Falhou", "error"],
  cancelled: ["Parado", "warn"],
};

function renderJob(j: FileJob) {
  const running = j.status === "running";
  $("fj-name").textContent = j.name;
  const [label, kind] = STATUS_LABEL[j.status];
  $("fj-status").textContent = label;
  $("fj-status").className = `pill ${kind}`;

  // Barra: proporção do ficheiro já transcrita. Sem duração conhecida, ou antes
  // do primeiro pedaço voltar, fica "indeterminada" (a deslizar).
  const fraction = j.total_s ? Math.min(1, j.done_s / j.total_s) : null;
  const meter = $("fj-meter");
  const indeterminate = running && (fraction == null || j.done_s === 0);
  meter.className = `meter big ${indeterminate ? "indeterminate" : ""} ${j.status === "failed" ? "over" : ""}`;
  $("fj-bar").style.width = indeterminate ? "" : `${Math.round((fraction ?? 1) * 100)}%`;
  if (fraction != null) meter.setAttribute("aria-valuenow", String(Math.round(fraction * 100)));
  else meter.removeAttribute("aria-valuenow");

  const parts: string[] = [];
  if (running && j.done_s === 0) {
    parts.push("A preparar e a enviar os primeiros trechos…");
  } else if (running) {
    parts.push(j.total_s ? `${duration(j.done_s)} de ${duration(j.total_s)}` : `${duration(j.done_s)} transcritos`);
    // Tempo que falta, ao ritmo a que tem ido.
    if (j.total_s && j.elapsed_s > 0) {
      const left = ((j.total_s - j.done_s) * j.elapsed_s) / j.done_s;
      parts.push(`faltam ~${duration(Math.max(left, 1))}`);
    }
  } else {
    parts.push(`${duration(j.done_s)} em ${duration(j.elapsed_s)}`);
  }
  if (j.cost_usd > 0 || !running) parts.push(usd(j.cost_usd, 2));
  parts.push(j.model);
  $("fj-detail").textContent = parts.join(" · ");

  const message = $("fj-message");
  message.textContent = j.message ?? "";
  message.hidden = !j.message;
  message.className = j.status === "failed" ? "error" : "muted";
  // Uma falha é anunciada logo pelo leitor de ecrã; o resto fica em silêncio.
  if (j.status === "failed" && j.message) message.setAttribute("role", "alert");
  else message.removeAttribute("role");

  const text = $("fj-text");
  text.textContent = j.text ?? ""; // textContent: o texto nunca é interpretado como HTML
  text.hidden = !j.text;

  $("fj-stop").hidden = !running;
  $("fj-copy").hidden = running || !j.text;
  $("fj-save").hidden = running || !j.text;
  $("fj-new").hidden = running;
  $("fj-saved").textContent = j.saved_to ? `Cópia guardada em ${j.saved_to}` : "";

  // Na barra lateral, a percentagem enquanto trabalha (para quem está noutra página).
  const badge = $("nav-file-progress");
  badge.hidden = !running;
  badge.textContent = indeterminate || fraction == null ? "…" : `${Math.round(fraction * 100)} %`;
}

function onJob(j: FileJob | null) {
  job = j;
  if (!j) return show("drop");
  renderJob(j);
  // Se estás a confirmar outro ficheiro, não te tira do que estás a fazer.
  if ($("file-confirm").hidden) show("job");
}

// ---------------------------------------------------------------- arrastar

/** `showPage` muda para esta página (vem de main.ts, para não haver imports circulares). */
export async function setupFiles(showPage: () => void) {
  const overlay = $("drop-overlay");
  await getCurrentWebview().onDragDropEvent((e) => {
    const p = e.payload;
    if (p.type === "enter" || p.type === "over") {
      overlay.hidden = false;
    } else if (p.type === "leave") {
      overlay.hidden = true;
    } else {
      overlay.hidden = true;
      if (!p.paths.length) return;
      if (p.paths.length > 1) toast("Transcrevo um ficheiro de cada vez: fico com o primeiro.");
      showPage();
      void choose(p.paths[0]);
    }
  });

  $("file-pick").addEventListener("click", async () => {
    try {
      const path = await files.pick();
      if (path) await choose(path);
    } catch (e) {
      toast(errorText(e), "error");
    }
  });
  $("fc-back").addEventListener("click", () => {
    probe = null;
    show("drop");
  });
  $("fc-start").addEventListener("click", async () => {
    if (!probe) return;
    const button = $<HTMLButtonElement>("fc-start");
    button.disabled = true;
    try {
      const started = await files.start(probe.path);
      probe = null;
      show("job");
      onJob(started);
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      button.disabled = false;
    }
  });
  $("fj-stop").addEventListener("click", () => void files.cancel());
  $("fj-copy").addEventListener("click", async () => {
    if (!job?.text) return;
    try {
      await api.copyText(job.text);
      toast("Texto copiado");
    } catch (e) {
      toast(errorText(e), "error");
    }
  });
  $("fj-save").addEventListener("click", async () => {
    try {
      // Sem aviso: a linha por baixo dos botões já diz onde ficou.
      const path = await files.saveBeside();
      $("fj-saved").textContent = `Guardado em ${path}`;
    } catch (e) {
      toast(errorText(e), "error");
    }
  });
  $("fj-new").addEventListener("click", async () => {
    await files.clear().catch(() => {});
    onJob(null);
  });

  await onFileJob(onJob);
  onJob(await files.status().catch(() => null));
}
