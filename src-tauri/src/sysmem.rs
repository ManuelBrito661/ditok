//! Medição da RAM da app e do WebView da janela de definições.
//!
//! No Windows, o WebView2 corre em processos `msedgewebview2.exe` que são filhos
//! (diretos ou indiretos) do processo do Ditok, por isso somamos a árvore de
//! processos. No macOS, os processos do WKWebView são lançados pelo sistema (XPC) e
//! não são filhos da app, por isso aí só medimos o processo principal.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MemoryReport {
    /// Working set do processo do Ditok, em MB.
    pub app_mb: f64,
    /// Soma dos processos descendentes (WebView2), em MB.
    pub webview_mb: f64,
    pub webview_processes: usize,
}

/// Bloqueante (lista todos os processos do sistema, ~10–30 ms): chamar fora do
/// runtime assíncrono.
pub fn report() -> Option<MemoryReport> {
    let me = sysinfo::get_current_pid().ok()?;
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory(),
    );
    let processes = sys.processes();

    // Mapa pai → filhos, para percorrer a árvore a partir do nosso processo.
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, p) in processes {
        if let Some(parent) = p.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut descendants = HashSet::new();
    let mut stack = vec![me];
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).into_iter().flatten() {
            // `insert` devolve false se já lá estava: protege contra ciclos de PIDs
            // reutilizados.
            if descendants.insert(child) {
                stack.push(child);
            }
        }
    }

    let mb = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    let app = processes.get(&me).map_or(0, |p| p.memory());
    let webview: u64 = descendants
        .iter()
        .filter_map(|pid| processes.get(pid))
        .map(|p| p.memory())
        .sum();
    Some(MemoryReport {
        app_mb: mb(app),
        webview_mb: mb(webview),
        webview_processes: descendants.len(),
    })
}

/// Mede e regista no log, numa thread de bloqueio, ao fim de `delay`.
pub fn log_after(delay: std::time::Duration, context: &'static str) {
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(delay);
        match report() {
            Some(r) => tracing::info!(
                context,
                app_mb = format!("{:.1}", r.app_mb),
                webview_mb = format!("{:.1}", r.webview_mb),
                webview_processes = r.webview_processes,
                "memória"
            ),
            None => tracing::warn!(context, "não foi possível medir a memória"),
        }
    });
}
