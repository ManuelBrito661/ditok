//! Medição da RAM da app e do WebView da janela de definições.
//!
//! No Windows, o WebView2 corre em processos `msedgewebview2.exe` que são filhos
//! (diretos ou indiretos) do processo do Ditok, por isso somamos a árvore de
//! processos. No macOS, os processos do WKWebView são lançados pelo sistema (XPC) e
//! não são filhos da app, por isso aí só medimos o processo principal.
//!
//! Que número usar: no Windows lemos a memória **privada** de cada processo (a que
//! o Gestor de Tarefas mostra). O "working set" do `sysinfo` inclui as DLLs
//! partilhadas, que somadas pelos ~6 processos do WebView2 davam ~3× mais do que o
//! consumo real.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Debug, Clone, Copy, Serialize)]
pub struct MemoryReport {
    /// Memória privada do processo do Ditok, em MB (fora do Windows: RSS).
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
    let memory = |pid: &Pid| private_bytes(*pid).or_else(|| processes.get(pid).map(|p| p.memory()));
    let app = memory(&me).unwrap_or(0);
    let webview: u64 = descendants.iter().filter_map(memory).sum();
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

/// Memória privada (bytes) de um processo, via `GetProcessMemoryInfo`.
#[cfg(windows)]
fn private_bytes(pid: Pid) -> Option<u64> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };

    // SAFETY: chamadas FFI à API do Windows. O compilador não consegue verificar
    // código C, por isso o `unsafe` diz "eu garanto as regras":
    // - o handle só é usado se não for nulo e é sempre fechado (CloseHandle);
    // - a estrutura é inicializada a zeros, tem o `cb` com o tamanho certo e vive
    //   na stack durante toda a chamada;
    // - PROCESS_MEMORY_COUNTERS_EX começa com os mesmos campos que
    //   PROCESS_MEMORY_COUNTERS, por isso o cast do ponteiro é o que a API espera.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid.as_u32());
        if handle.is_null() {
            return None;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let ok = GetProcessMemoryInfo(
            handle,
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        );
        CloseHandle(handle);
        (ok != 0).then_some(counters.PrivateUsage as u64)
    }
}

#[cfg(not(windows))]
fn private_bytes(_pid: Pid) -> Option<u64> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn reads_private_memory_of_this_process() {
        let me = sysinfo::get_current_pid().unwrap();
        let bytes = super::private_bytes(me).expect("devia conseguir ler o próprio processo");
        // O processo de testes usa de certeza entre 100 KB e 4 GB.
        assert!(
            (100 * 1024..4 * 1024 * 1024 * 1024).contains(&bytes),
            "{bytes}"
        );
    }
}
