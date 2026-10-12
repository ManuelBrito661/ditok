//! Orquestrador do ditado: uma única tarefa assíncrona que recebe eventos por um
//! canal e é a única dona do estado (máquina de estados, tray, provider de STT).
//!
//! Porquê assim (padrão "actor"): como só esta tarefa toca no estado, não precisamos
//! de `Mutex` nem de nos preocupar com duas threads a mudá-lo ao mesmo tempo. Quem
//! quer influenciar o ditado (atalho, áudio, temporizadores, processamento) envia
//! uma mensagem. O `rx.recv().await` adormece a tarefa até chegar uma mensagem:
//! não há polling.

pub mod failure;
pub mod machine;
pub mod pipeline;
pub mod selection;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tauri::async_runtime::JoinHandle;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::{
    audio::{Recorder, Recording},
    focus, hotkey,
    sound::{Cue, Sounds},
    state::{AppState, MISSING_KEY},
    stt::{SttProvider, openrouter::OpenRouter},
    tray::{Tray, TrayState},
};
use failure::FailReason;
use machine::{Action, Input, Machine, Mode, Phase};
use pipeline::{Commit, Outcome};
use selection::Selection;

/// Quanto tempo cada resultado fica visível antes de voltar a "parado". Uma falha
/// fica mais tempo: tem um motivo para ler e, às vezes, um "Tentar de novo".
const DONE_FOR: Duration = Duration::from_millis(1200);
const COPIED_FOR: Duration = Duration::from_millis(3000);
const FAILED_FOR: Duration = Duration::from_millis(4000);
/// Intervalo mínimo entre dois avisos de "ocupado" (abanão + som).
const BUSY_EVERY: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub enum DictationEvent {
    Hotkey {
        pressed: bool,
        at: Instant,
        /// Qual dos atalhos: ditado ou comando.
        mode: Mode,
    },
    Cancel,
    /// Clique na barra flutuante: começa ou termina um ditado em modo alternar,
    /// ou, numa falha com gravação guardada, tenta de novo.
    Click,
    /// "Tentar de novo" no menu do tray.
    Retry,
    /// O microfone não abriu ou desligou-se a meio.
    AudioFailed(String),
    /// `job`: qual processamento acabou. Um processamento cancelado pode ainda
    /// chegar a enviar o seu fim; o número permite ignorá-lo.
    ProcessingFinished {
        job: u64,
        outcome: Outcome,
    },
    /// O resultado número `shown` já esteve visível o suficiente.
    ResultElapsed {
        shown: u64,
    },
    /// A chave ou o modo de envio mudaram nas definições: o cliente do OpenRouter
    /// tem de ser recriado no próximo ditado.
    ProviderChanged,
    /// O texto final de um ditado (colado ou só copiado), para o "Colar o último
    /// ditado". Enviado pelo processamento, antes de `ProcessingFinished`.
    LastText(String),
    /// "Colar o último ditado" no menu do tray ou da barra.
    PasteLast,
    /// O atalho do histórico rápido: abre o menu com os últimos ditados.
    QuickHistory,
    /// Um ditado escolhido no histórico rápido: cola-o onde estavas.
    PasteText(String),
    /// A colagem pedida por `PasteLast` acabou.
    PastedAgain(Outcome),
    /// O histórico foi apagado: o último ditado também é esquecido.
    HistoryCleared,
}

/// O que o orquestrador recebe do resto da app. Agrupar num struct evita funções
/// com muitos argumentos e deixa claro o que passa a pertencer à tarefa.
pub struct Deps {
    pub tray: Tray,
    pub recorder: Recorder,
    pub sounds: Sounds,
    pub state: Arc<AppState>,
}

impl Deps {
    /// Toca um som, se estiverem ligados nas definições.
    fn cue(&self, cue: Cue) {
        if self.state.config().sounds {
            self.sounds.play(cue);
        }
    }
}

/// O processamento em curso: o número que o identifica e a "pega" da tarefa,
/// que permite abortá-la.
struct Job {
    id: u64,
    handle: JoinHandle<()>,
    /// Decide quem ganha entre cancelar e colar (ver `pipeline::Commit`).
    commit: Commit,
    /// É um "Tentar de novo"?
    is_retry: bool,
}

/// Tudo o que o orquestrador guarda entre eventos, além da máquina de estados.
/// Como só a tarefa do orquestrador lhe toca, os métodos recebem `&mut self`
/// sem precisar de `Mutex`.
struct Orchestrator {
    deps: Deps,
    tx: UnboundedSender<DictationEvent>,
    machine: Machine,
    /// Criado já se a chave existir; senão, volta a tentar-se a cada ditado (assim
    /// podes guardar a chave sem reiniciar a app).
    openrouter: Option<Arc<OpenRouter>>,
    /// Janela em primeiro plano quando a gravação começou (é lá que se cola) e a
    /// app dona dela (decide o estilo).
    target: focus::Target,
    /// Modo comando: a seleção copiada (ou a ser copiada) para o comando em
    /// curso. Largá-la repõe o clipboard (ver `selection.rs`), por isso basta
    /// pôr `None` para "desistir" dela.
    selection: Option<Selection>,
    job: Option<Job>,
    next_job: u64,
    /// A última gravação cuja transcrição falhou, para o "Tentar de novo". Fica
    /// em memória (não em disco): o áudio é privado e só faz falta nesta sessão.
    failed: Option<Recording>,
    /// A falha que está à vista na barra oferece "Tentar de novo"?
    retry_offered: bool,
    /// Cada resultado mostrado recebe um número; o temporizador de um resultado
    /// antigo não apaga um mais recente.
    shown: u64,
    last_busy: Option<Instant>,
    /// O texto do último ditado, para o "Colar o último ditado". Vem do histórico
    /// no arranque e fica em memória mesmo com o histórico desligado (só nesta
    /// sessão, nunca em disco).
    last_text: Option<String>,
}

pub fn spawn(
    deps: Deps,
    tx: UnboundedSender<DictationEvent>,
    rx: UnboundedReceiver<DictationEvent>,
) {
    // `deps` e `rx` são movidos para dentro da tarefa: a partir daqui pertencem-lhe.
    tauri::async_runtime::spawn(run(deps, tx, rx));
}

async fn run(
    deps: Deps,
    tx: UnboundedSender<DictationEvent>,
    mut rx: UnboundedReceiver<DictationEvent>,
) {
    let openrouter = load_openrouter(&deps.state);
    // O ditado mais recente do histórico (ler o ficheiro é disco: `spawn_blocking`).
    let state = Arc::clone(&deps.state);
    let last_text = tauri::async_runtime::spawn_blocking(move || {
        state
            .config()
            .history_enabled
            .then(|| state.history.list().into_iter().next().map(|h| h.text))
            .flatten()
    })
    .await
    .ok()
    .flatten();
    deps.tray.set_paste_last_available(last_text.is_some());
    let mut o = Orchestrator {
        deps,
        tx,
        machine: Machine::default(),
        openrouter,
        target: focus::Target::default(),
        selection: None,
        job: None,
        next_job: 0,
        failed: None,
        retry_offered: false,
        shown: 0,
        last_busy: None,
        last_text,
    };
    while let Some(event) = rx.recv().await {
        o.handle(event);
    }
    tracing::debug!("orquestrador terminado");
}

impl Orchestrator {
    fn handle(&mut self, event: DictationEvent) {
        let app = self.deps.state.app.clone();
        let (input, at) = match event {
            DictationEvent::Hotkey {
                pressed: true,
                at,
                mode,
            } => (Input::Pressed(mode), at),
            DictationEvent::Hotkey {
                pressed: false,
                at,
                mode,
            } => (Input::Released(mode), at),
            // A processar, o cancelamento só vale se a colagem ainda não começou. A
            // decisão é atómica (`try_cancel`) e vem ANTES da máquina de estados,
            // para a máquina nunca ficar "parada" com um ditado ainda a colar.
            DictationEvent::Cancel
                if self.machine.phase() == Phase::Processing
                    && self.job.as_ref().is_some_and(|j| !j.commit.try_cancel()) =>
            {
                tracing::info!("já a colar (ou acabado); cancelamento ignorado");
                return;
            }
            DictationEvent::Cancel => (Input::Cancel, Instant::now()),
            // Clicar numa falha que oferece "Tentar de novo" tenta de novo; nos
            // outros casos é o clique normal (começar/terminar).
            DictationEvent::Click
                if self.deps.tray.state() == TrayState::Failed
                    && self.retry_offered
                    && self.failed.is_some() =>
            {
                (Input::Retry, Instant::now())
            }
            DictationEvent::Click => (Input::Click, Instant::now()),
            DictationEvent::Retry => {
                if self.failed.is_none() {
                    tracing::debug!("tentar de novo pedido sem gravação guardada");
                    return;
                }
                (Input::Retry, Instant::now())
            }
            DictationEvent::AudioFailed(reason) => {
                self.audio_failed(&reason);
                return;
            }
            DictationEvent::ProviderChanged => {
                // Um ditado em curso já tem o seu `Arc` e termina com o cliente antigo;
                // o próximo cria um novo.
                self.openrouter = None;
                return;
            }
            DictationEvent::LastText(text) => {
                self.last_text = Some(text);
                self.deps.tray.set_paste_last_available(true);
                return;
            }
            DictationEvent::HistoryCleared => {
                self.last_text = None;
                self.deps.tray.set_paste_last_available(false);
                return;
            }
            DictationEvent::PasteLast => {
                self.paste_last();
                return;
            }
            DictationEvent::QuickHistory => {
                // A meio de um ditado o atalho não faz nada: o alvo seria o próprio menu.
                if self.machine.phase() == Phase::Idle {
                    crate::quick_history::open(&app);
                } else {
                    tracing::info!("histórico rápido pedido a meio de um ditado; ignorado");
                }
                return;
            }
            DictationEvent::PasteText(text) => {
                self.paste_text(text);
                return;
            }
            DictationEvent::PastedAgain(outcome) => {
                // Se entretanto começaste a ditar, a barra mostra o ditado novo.
                if self.machine.phase() != Phase::Idle {
                    return;
                }
                match outcome {
                    Outcome::Pasted => self.show_result(TrayState::Done, DONE_FOR),
                    Outcome::Copied => self.show_result(TrayState::Copied, COPIED_FOR),
                    Outcome::Failed { reason, .. } => self.show_failure(reason, false),
                    Outcome::Nothing => {}
                }
                return;
            }
            DictationEvent::ProcessingFinished { job, outcome } => {
                // Um processamento cancelado (ou de um ditado anterior) já não conta.
                if self.job.as_ref().map(|j| j.id) != Some(job) {
                    tracing::debug!(job, "fim de um processamento cancelado; ignorado");
                    return;
                }
                let is_retry = self.job.take().is_some_and(|j| j.is_retry);
                self.machine
                    .handle(Input::ProcessingFinished, Instant::now());
                hotkey::set_cancel_enabled(&app, false);
                self.finish_processing(outcome, is_retry);
                return;
            }
            DictationEvent::ResultElapsed { shown } => {
                // Só volta a "parado" se entretanto não aconteceu mais nada.
                if shown == self.shown
                    && matches!(
                        self.deps.tray.state(),
                        TrayState::Done
                            | TrayState::Copied
                            | TrayState::Failed
                            | TrayState::NothingHeard
                    )
                {
                    self.retry_offered = false;
                    self.deps.tray.set(TrayState::Idle);
                }
                return;
            }
        };

        let before = self.machine.phase();
        match self.machine.handle(input, at) {
            Some(Action::StartRecording(mode)) => self.start_recording(at, mode),
            Some(Action::StopAndProcess) => self.stop_and_process(at),
            Some(Action::Retry) => self.retry(),
            Some(Action::CancelRecording) => {
                // Um comando cancelado: o clipboard volta ao que era.
                self.selection = None;
                self.deps.recorder.cancel();
                self.deps.cue(Cue::Cancel);
                hotkey::set_cancel_enabled(&app, false);
                self.deps.tray.set(TrayState::Idle);
                tracing::info!("gravação cancelada");
            }
            Some(Action::CancelProcessing) => {
                // `abort` pára a tarefa no próximo `.await`: o pedido HTTP é largado
                // e nada é colado. Só chegamos aqui se `try_cancel` ganhou, ou seja,
                // a colagem ainda não tinha começado (ver `pipeline::Commit`).
                if let Some(job) = self.job.take() {
                    job.handle.abort();
                }
                self.deps.cue(Cue::Cancel);
                hotkey::set_cancel_enabled(&app, false);
                self.deps.tray.set(TrayState::Idle);
                tracing::info!("processamento cancelado; nada foi colado");
            }
            Some(Action::Busy) => self.busy(),
            None if self.machine.phase() == Phase::Latched && before != Phase::Latched => {
                tracing::info!("modo alternar: prime o atalho outra vez para terminar");
            }
            None => tracing::trace!(?input, phase = ?self.machine.phase(), "evento ignorado"),
        }
        // Em modo alternar, a barra mostra os botões ✕ (cancelar) e ■ (parar). Vem
        // depois de `tray.set` para chegar à barra a seguir ao estado "a gravar".
        if self.machine.phase() == Phase::Latched && before != Phase::Latched {
            crate::overlay::emit_latched(&app);
            // Modo comando alternado: a tecla acabou de ser largada, por isso é a
            // altura segura de copiar a seleção (ver `capture_selection`).
            if self.machine.mode() == Mode::Command && self.selection.is_none() {
                self.selection = Some(Selection::capture(app.clone(), self.target.window));
            }
        }
    }

    fn start_recording(&mut self, at: Instant, mode: Mode) {
        let app = self.deps.state.app.clone();
        if self.openrouter.is_none() {
            self.openrouter = load_openrouter(&self.deps.state);
        }
        sync_key_set(&self.deps.state);
        let Some(provider) = &self.openrouter else {
            // Sem chave não vale a pena gravar: volta a "parado" e explica porquê.
            self.machine.handle(Input::Cancel, at);
            self.deps.state.notify(MISSING_KEY);
            self.show_failure(FailReason::NoKey, false);
            return;
        };
        // A janela onde estás a escrever, antes de o Ditok fazer o que quer que seja.
        let window = focus::foreground();
        // Primeiro o microfone (é o que tem latência), depois o som, a
        // ligação HTTP (corre enquanto falas) e o resto.
        let config = self.deps.state.config();
        self.deps
            .recorder
            .start(at, config.sounds, config.input_device.clone());
        self.deps.cue(Cue::Start);
        // O nome da app da janela já capturada (microssegundos; depois do
        // microfone para não o atrasar).
        self.target = focus::Target::of(window);
        if let Some(app) = &self.target.app {
            self.deps.state.note_app(app);
        }
        // A pílula mostra o estilo enquanto ouve, se não for o Predefinido. No
        // modo comando não há estilo: a pílula diz "Comando".
        let style = config.style_for(self.target.app.as_deref());
        let shown = match mode {
            Mode::Dictation => crate::cleanup::shown_style(config.cleanup_level, style),
            Mode::Command => None,
        };
        crate::overlay::emit_mode(&app, mode);
        crate::overlay::emit_style(&app, shown.map(|s| s.label()));
        // Uma seleção de um comando anterior que tenha ficado para trás (não
        // devia) é largada já: repõe o clipboard antes de copiar outra.
        self.selection = None;
        // A barra segue o monitor onde está o cursor (como no Wispr Flow).
        crate::overlay::reposition_for(&app);
        let provider = Arc::clone(provider);
        tauri::async_runtime::spawn(async move { provider.prewarm().await });
        self.retry_offered = false;
        self.deps.tray.set(TrayState::Recording);
        hotkey::set_cancel_enabled(&app, true);
        tracing::info!(?mode, "a gravar");
    }

    fn stop_and_process(&mut self, released_at: Instant) {
        // `stop()` envia já o pedido à thread de áudio; o future que devolve
        // só entrega a gravação quando estiver pronta.
        let recording = self.deps.recorder.stop();
        self.deps.cue(Cue::Stop);
        // O Esc continua registado: também cancela durante o processamento.
        self.deps.tray.set(TrayState::Processing);

        let Some(provider) = self.openrouter.clone() else {
            // Só acontece se a chave mudou a meio da gravação; o compilador
            // obriga-nos a decidir o que fazer.
            self.selection = None;
            self.deps.recorder.cancel();
            self.spawn_job(Commit::default(), false, async {
                Outcome::Failed {
                    reason: FailReason::NoKey,
                    retry: None,
                }
            });
            return;
        };
        let target = self.target.clone();
        let state = Arc::clone(&self.deps.state);
        let commit = Commit::default();
        if self.machine.mode() == Mode::Command {
            // Em modo alternar a seleção já foi copiada ao largar a tecla; a
            // segurar, copia-se agora (a tecla acabou de ser largada). Corre
            // ao mesmo tempo que o fim da gravação e a transcrição.
            let selection = self
                .selection
                .take()
                .unwrap_or_else(|| Selection::capture(self.deps.state.app.clone(), target.window));
            self.spawn_job(commit.clone(), false, async move {
                pipeline::command(
                    state,
                    provider,
                    recording,
                    released_at,
                    target,
                    selection,
                    commit,
                )
                .await
            });
            return;
        }
        self.spawn_job(commit.clone(), false, async move {
            pipeline::process(state, provider, recording, released_at, target, commit).await
        });
    }

    /// Volta a enviar a gravação falhada. A janela de destino é a que está em
    /// primeiro plano agora (o ditado original já foi há algum tempo), e o
    /// estilo é o da app dela, porque é lá que o texto vai parar.
    fn retry(&mut self) {
        let app = self.deps.state.app.clone();
        if self.openrouter.is_none() {
            self.openrouter = load_openrouter(&self.deps.state);
        }
        // `clone` barato (o WAV é `Bytes`): se cancelares, a gravação continua guardada.
        let (Some(provider), Some(rec)) = (self.openrouter.clone(), self.failed.clone()) else {
            self.machine.handle(Input::Cancel, Instant::now());
            if self.openrouter.is_none() {
                self.deps.state.notify(MISSING_KEY);
                self.show_failure(FailReason::NoKey, false);
            }
            return;
        };
        self.retry_offered = false;
        // Só os ditados se tentam de novo: a pílula sai do aspeto de comando.
        crate::overlay::emit_mode(&app, Mode::Dictation);
        self.deps.tray.set(TrayState::Processing);
        hotkey::set_cancel_enabled(&app, true);
        let target = focus::Target::of(focus::foreground_for_retry());
        let state = Arc::clone(&self.deps.state);
        let commit = Commit::default();
        self.spawn_job(commit.clone(), true, async move {
            pipeline::retry(state, provider, rec, target, commit).await
        });
    }

    /// "Colar o último ditado": cola-o na janela onde estavas antes de abrir o
    /// menu; se não houver nenhuma (ex.: o menu foi aberto pelo teclado), fica só
    /// no clipboard e a barra diz "Copiado".
    fn paste_last(&mut self) {
        let Some(text) = self.last_text.clone() else {
            tracing::debug!("colar o último ditado pedido sem ditado guardado");
            return;
        };
        self.paste_text(text);
    }

    /// Cola `text` (o último ditado ou uma entrada do histórico rápido) na janela
    /// onde estavas antes de abrir o menu.
    fn paste_text(&mut self, text: String) {
        if self.machine.phase() != Phase::Idle {
            tracing::info!("colagem pedida a meio de um ditado; ignorada");
            return;
        }
        let target = crate::tray::take_menu_target().or_else(focus::foreground_for_retry);
        let state = Arc::clone(&self.deps.state);
        let tx = self.tx.clone();
        tauri::async_runtime::spawn(async move {
            let outcome = pipeline::paste_again(state, text, target).await;
            let _ = tx.send(DictationEvent::PastedAgain(outcome));
        });
    }

    /// O processamento corre numa tarefa à parte para o orquestrador continuar a
    /// responder (ao Esc, ao "ocupado"…). Guardamos a "pega" para a podermos abortar.
    fn spawn_job(
        &mut self,
        commit: Commit,
        is_retry: bool,
        work: impl Future<Output = Outcome> + Send + 'static,
    ) {
        self.next_job += 1;
        let id = self.next_job;
        let tx = self.tx.clone();
        let finished = commit.clone();
        let handle = tauri::async_runtime::spawn(async move {
            let outcome = work.await;
            // Acabar também "compromete": um Esc que chegue a seguir já não
            // finge cancelar um ditado que terminou. Se foi cancelado, ninguém espera
            // por este resultado.
            if finished.try_commit() {
                let _ = tx.send(DictationEvent::ProcessingFinished { job: id, outcome });
            }
        });
        self.job = Some(Job {
            id,
            handle,
            commit,
            is_retry,
        });
    }

    fn finish_processing(&mut self, outcome: Outcome, is_retry: bool) {
        match outcome {
            Outcome::Pasted => {
                // Um ditado novo correu bem: a gravação falhada antiga já não interessa.
                self.set_failed(None);
                self.show_result(TrayState::Done, DONE_FOR);
            }
            Outcome::Copied => {
                self.set_failed(None);
                self.show_result(TrayState::Copied, COPIED_FOR);
            }
            // Sem isto, um ditado sem voz acabava sem dizer nada e parecia avariado.
            Outcome::Nothing => {
                // Tentar de novo e o modelo não ouviu nada: repetir não muda isso.
                if is_retry {
                    self.set_failed(None);
                }
                self.show_result(TrayState::NothingHeard, DONE_FOR);
            }
            Outcome::Failed { reason, retry } => {
                let can_retry = retry.is_some();
                // Uma falha sem gravação (ex.: microfone) não apaga a anterior.
                if can_retry {
                    self.set_failed(retry);
                }
                self.show_failure(reason, can_retry);
            }
        }
    }

    fn set_failed(&mut self, rec: Option<Recording>) {
        self.deps.tray.set_retry_available(rec.is_some());
        self.failed = rec;
    }

    fn show_failure(&mut self, reason: FailReason, retry: bool) {
        tracing::info!(?reason, retry, "ditado falhou");
        self.retry_offered = retry;
        self.deps.cue(Cue::Error);
        self.deps.tray.fail(reason, retry);
        self.schedule_reset(FAILED_FOR);
    }

    /// O resultado fica visível um instante e depois volta a "parado".
    fn show_result(&mut self, state: TrayState, visible_for: Duration) {
        self.retry_offered = false;
        self.deps.tray.set(state);
        self.schedule_reset(visible_for);
    }

    fn schedule_reset(&mut self, after: Duration) {
        self.shown += 1;
        let shown = self.shown;
        let tx = self.tx.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(after).await;
            let _ = tx.send(DictationEvent::ResultElapsed { shown });
        });
    }

    /// Premiste o atalho a meio do processamento: a pílula abana e ouve-se um som
    /// curto, para não parecer que o atalho deixou de funcionar.
    fn busy(&mut self) {
        let now = Instant::now();
        if self
            .last_busy
            .is_some_and(|t| now.saturating_duration_since(t) < BUSY_EVERY)
        {
            return;
        }
        self.last_busy = Some(now);
        tracing::info!("atalho premido a meio do processamento; ainda ocupado");
        crate::overlay::emit_busy(&self.deps.state.app);
        self.deps.cue(Cue::Busy);
    }

    fn audio_failed(&mut self, reason: &str) {
        let app = self.deps.state.app.clone();
        match self.machine.phase() {
            Phase::Holding { .. } | Phase::Latched => {
                tracing::error!(%reason, "falha de áudio durante a gravação");
                // Para a máquina é igual a cancelar; a barra mostra a falha em vez
                // de continuar a fingir que grava.
                self.machine.handle(Input::Cancel, Instant::now());
                self.selection = None;
                self.deps.recorder.cancel();
                hotkey::set_cancel_enabled(&app, false);
                self.deps.state.notify(reason);
                self.show_failure(FailReason::NoMicrophone, false);
            }
            Phase::Processing => {
                // Largaste a tecla antes de o microfone falhar: o processamento vai
                // receber "não havia gravação" e mostra a falha. Aqui só se explica.
                tracing::error!(%reason, "falha de áudio; o processamento mostra o erro");
                self.deps.state.notify(reason);
            }
            Phase::Idle => {
                // Nada a gravar (ex.: um erro atrasado de uma gravação já cancelada):
                // não mexemos na barra nem no tray.
                tracing::warn!(%reason, "falha de áudio sem gravação em curso");
            }
        }
    }
}

/// A chave pode ter sido trocada ou apagada fora da app (bin/set-key.rs): a barra
/// e o tray seguem o cofre ao começar um ditado. Se não der para ler, fica como está.
fn sync_key_set(state: &AppState) {
    let Ok(key) = crate::secrets::get() else {
        return;
    };
    let set = key.is_some();
    if state.key_set() != set {
        tracing::info!(set, "a chave mudou fora da app; a atualizar o estado");
        state.set_key_set(set); // publica a barra e o tray
    }
}

fn load_openrouter(state: &AppState) -> Option<Arc<OpenRouter>> {
    match state.load_openrouter() {
        Ok(Some(p)) => {
            tracing::info!(provider = p.name(), "provider de STT pronto");
            Some(p)
        }
        Ok(None) => {
            tracing::warn!("{MISSING_KEY}");
            None
        }
        Err(e) => {
            tracing::error!(error = %e, "não foi possível ler a chave do cofre");
            None
        }
    }
}
