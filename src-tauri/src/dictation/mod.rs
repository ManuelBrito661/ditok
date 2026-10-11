//! Orquestrador do ditado: uma única tarefa assíncrona que recebe eventos por um
//! canal e é a única dona do estado (máquina de estados + tray).
//!
//! Porquê assim (padrão "actor"): como só esta tarefa toca no estado, não precisamos
//! de `Mutex` nem de nos preocupar com duas threads a mudá-lo ao mesmo tempo. Quem
//! quer influenciar o ditado (atalho, temporizadores, mais tarde o áudio e a rede)
//! envia uma mensagem. O `rx.recv().await` adormece a tarefa até chegar uma
//! mensagem: não há polling.

pub mod machine;

use std::time::{Duration, Instant};

use tauri::AppHandle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    hotkey,
    tray::{Tray, TrayState},
};
use machine::{Action, Input, Machine, Phase};

/// Quanto tempo o ícone "pronto" fica visível depois de colar.
const DONE_ICON_FOR: Duration = Duration::from_millis(1200);

#[derive(Debug)]
pub enum DictationEvent {
    Hotkey { pressed: bool, at: Instant },
    Cancel,
    ProcessingFinished,
    DoneIconElapsed,
}

pub fn spawn(
    app: AppHandle,
    tray: Tray,
    tx: UnboundedSender<DictationEvent>,
    rx: UnboundedReceiver<DictationEvent>,
) {
    // `tray` e `rx` são movidos para dentro da tarefa: a partir daqui pertencem-lhe.
    tauri::async_runtime::spawn(run(app, tray, tx, rx));
}

async fn run(
    app: AppHandle,
    mut tray: Tray,
    tx: UnboundedSender<DictationEvent>,
    mut rx: UnboundedReceiver<DictationEvent>,
) {
    let mut machine = Machine::default();
    let mut recording_since: Option<Instant> = None;

    while let Some(event) = rx.recv().await {
        let (input, at) = match event {
            DictationEvent::Hotkey { pressed: true, at } => (Input::Pressed, at),
            DictationEvent::Hotkey { pressed: false, at } => (Input::Released, at),
            DictationEvent::Cancel => (Input::Cancel, Instant::now()),
            DictationEvent::ProcessingFinished => (Input::ProcessingFinished, Instant::now()),
            DictationEvent::DoneIconElapsed => {
                // Só volta a "parado" se entretanto não começou outro ditado.
                if tray.state() == TrayState::Done {
                    tray.set(TrayState::Idle);
                }
                continue;
            }
        };

        let before = machine.phase();
        match machine.handle(input, at) {
            Some(Action::StartRecording) => {
                recording_since = Some(at);
                tray.set(TrayState::Recording);
                hotkey::set_cancel_enabled(&app, true);
                tracing::info!("a gravar");
            }
            Some(Action::StopAndProcess) => {
                hotkey::set_cancel_enabled(&app, false);
                let held = recording_since
                    .take()
                    .map(|t| at.saturating_duration_since(t));
                tray.set(TrayState::Processing);
                tracing::info!(
                    duration_ms = held.map(|d| d.as_millis()),
                    "gravação terminada"
                );

                // Fase 1: simula a transcrição. Na fase 3 é aqui que entram o áudio,
                // o pedido STT e a colagem, numa tarefa à parte que no fim envia
                // `ProcessingFinished` pelo mesmo canal.
                let tx = tx.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let _ = tx.send(DictationEvent::ProcessingFinished);
                });
            }
            Some(Action::CancelRecording) => {
                hotkey::set_cancel_enabled(&app, false);
                recording_since = None;
                tray.set(TrayState::Idle);
                tracing::info!("gravação cancelada");
            }
            None if before == Phase::Processing && machine.phase() == Phase::Idle => {
                tray.set(TrayState::Done);
                tracing::info!("ditado concluído");
                let tx = tx.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(DONE_ICON_FOR).await;
                    let _ = tx.send(DictationEvent::DoneIconElapsed);
                });
            }
            None if machine.phase() == Phase::Latched && before != Phase::Latched => {
                tracing::info!("modo alternar: prime o atalho outra vez para terminar");
            }
            None => tracing::trace!(?input, phase = ?machine.phase(), "evento ignorado"),
        }
    }
    tracing::debug!("orquestrador terminado");
}
