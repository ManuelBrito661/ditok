//! A seleção copiada no modo comando, com a garantia de que o clipboard do
//! utilizador volta ao que era, aconteça o que acontecer (cancelar, falhar, sair
//! a meio).
//!
//! O problema: copiar a seleção (Ctrl+C) corre numa thread de bloqueio e mexe
//! no clipboard. Se o comando for cancelado (Esc) enquanto essa thread corre ou
//! enquanto se espera pelo LLM, a tarefa assíncrona é abortada e o código
//! "depois" nunca corre. A solução é o padrão RAII: a reposição vive no `Drop`
//! de `Selection`, que o Rust corre sempre que o valor desaparece, incluindo
//! num abort. Só a colagem "leva" a seleção (`take`) e, a partir daí, é ela que
//! trata do clipboard (repõe-no ou deixa lá o resultado, como no ditado).

use std::sync::{Arc, Mutex, PoisonError};

use tauri::AppHandle;
use tokio::sync::Notify;

use crate::{focus, paste::Captured};

/// Em que ponto está a cópia. Partilhado entre a thread que copia e quem espera.
enum Slot {
    /// A thread ainda está a copiar.
    Capturing,
    /// Copiado; o clipboard tem de ser reposto por alguém.
    Captured(Captured),
    /// A cópia falhou (o clipboard já foi reposto, ou nem foi tocado).
    Failed,
    /// A colagem levou-a: o clipboard já não é problema nosso.
    Taken,
    /// O comando acabou (ou foi cancelado) antes de a cópia terminar: a thread
    /// repõe o clipboard assim que acabar.
    Abandoned,
}

struct Shared {
    slot: Mutex<Slot>,
    /// Acorda quem espera pelo texto (`Selection::text`) quando a cópia acaba.
    ready: Notify,
}

/// A seleção de um comando. Criar uma começa logo a cópia numa thread de
/// bloqueio; largá-la sem a ter levado (`take`) repõe o clipboard.
pub struct Selection {
    shared: Arc<Shared>,
}

impl Selection {
    /// Começa a copiar a seleção da janela `target`. Se a janela em primeiro
    /// plano já não for essa, não simula o Ctrl+C (ia copiar noutra app) e conta
    /// como "sem seleção".
    pub fn capture(app: AppHandle, target: Option<focus::Window>) -> Self {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot::Capturing),
            ready: Notify::new(),
        });
        let mine = Arc::clone(&shared);
        tauri::async_runtime::spawn_blocking(move || {
            let result = if focus::should_paste(target, focus::foreground()) {
                crate::paste::capture_selection()
            } else {
                tracing::info!("a janela mudou antes de copiar a seleção; sem seleção");
                Ok(Captured::nothing())
            };
            let has_text = matches!(&result, Ok(c) if c.text.is_some());
            let mut slot = lock(&mine.slot);
            match (result, &*slot) {
                // Ninguém quer a seleção: repõe já (estamos numa thread de
                // bloqueio, por isso pode ser aqui mesmo).
                (Ok(c), Slot::Abandoned) => c.restore(),
                (Ok(c), _) => *slot = Slot::Captured(c),
                (Err(e), _) => {
                    tracing::warn!(error = %e, "não foi possível copiar a seleção");
                    *slot = Slot::Failed;
                }
            }
            // Larga o cadeado antes de acordar quem espera.
            drop(slot);
            mine.ready.notify_waiters();
            crate::overlay::emit_selection(&app, has_text);
        });
        Self { shared }
    }

    /// Espera pela cópia e devolve o texto selecionado (`None` = sem seleção).
    pub async fn text(&self) -> Option<String> {
        loop {
            // Criar o `Notified` ANTES de olhar para o estado: assim não se perde
            // um `notify_waiters` que chegue entre olhar e esperar.
            let notified = self.shared.ready.notified();
            // O bloco garante que o cadeado é largado antes do `.await` (um
            // `MutexGuard` não pode atravessar um `.await` numa tarefa `Send`).
            let done = {
                match &*lock(&self.shared.slot) {
                    Slot::Capturing => None,
                    Slot::Captured(c) => Some(c.text.clone()),
                    Slot::Failed | Slot::Taken | Slot::Abandoned => Some(None),
                }
            };
            if let Some(text) = done {
                return text;
            }
            notified.await;
        }
    }

    /// Leva a cópia para a colagem, que passa a ser responsável pelo clipboard.
    /// `None` se ainda não acabou (não acontece: `text` já esperou) ou falhou.
    pub fn take(&self) -> Option<Captured> {
        let mut slot = lock(&self.shared.slot);
        // `mem::replace` tira o valor de lá e deixa `Taken` no lugar: assim o
        // `Drop` sabe que já não tem nada a repor.
        match std::mem::replace(&mut *slot, Slot::Taken) {
            Slot::Captured(c) => Some(c),
            other => {
                *slot = other;
                None
            }
        }
    }
}

impl Drop for Selection {
    fn drop(&mut self) {
        let mut slot = lock(&self.shared.slot);
        match std::mem::replace(&mut *slot, Slot::Abandoned) {
            Slot::Captured(c) => {
                tracing::debug!("comando acabou sem colar; a repor o clipboard");
                // Mexer no clipboard é bloqueante: numa thread de bloqueio.
                tauri::async_runtime::spawn_blocking(move || c.restore());
            }
            // A thread ainda está a copiar: vê `Abandoned` e repõe ela.
            Slot::Capturing => {}
            Slot::Failed | Slot::Taken | Slot::Abandoned => {}
        }
    }
}

/// Um `Mutex` envenenado (pânico com ele trancado) ainda tem um valor válido.
fn lock(m: &Mutex<Slot>) -> std::sync::MutexGuard<'_, Slot> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
