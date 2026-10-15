mod assistant;
mod audio;
mod cleanup;
mod command;
mod commands;
mod config;
mod dict_io;
mod dictation;
mod files;
mod focus;
mod history;
mod hotkey;
mod logging;
mod metrics;
mod modkeys;
pub mod notes;
mod overlay;
mod paste;
mod quick_history;
mod screen;
pub mod secrets;
mod settings_window;
mod sound;
mod state;
mod summarize;
mod sysmem;
mod text;
mod tray;
mod updater;
mod usage;
mod xai;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tauri::{Emitter, Manager, RunEvent};

pub fn run() {
    let started = Instant::now();

    // Canal do orquestrador. Os `Sender` vão para o atalho, o áudio e as tarefas de
    // processamento; o `Receiver` tem um único dono: a tarefa do orquestrador.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    let app = tauri::Builder::default()
        // Tem de ser o primeiro plugin: uma segunda instância lutaria pelo atalho.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // Abrir o Ditok outra vez (atalho do menu Iniciar, ícone…) com ele já a
            // correr: em vez de não acontecer nada, mostro a janela principal.
            tracing::info!(?argv, "segunda instância: a abrir a janela");
            settings_window::open(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // Updates assinados vindos das Releases do GitHub; `process` serve para reiniciar.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Diálogo para escolher um ficheiro a transcrever (usado só a partir do Rust).
        .plugin(tauri_plugin_dialog::init())
        .plugin(hotkey::plugin(tx.clone()))
        .setup(move |app| {
            let paths = app.path();
            logging::init(&paths.app_log_dir()?)?;
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "Ditok a arrancar");

            let config_dir = paths.app_config_dir()?;
            // Tem de ser lido antes de `load_or_create`, que pode criar o ficheiro.
            let predates_onboarding = config::Config::predates_onboarding(&config_dir);
            let mut config = config::Config::load_or_create(&config_dir);
            tracing::info!(path = %config::Config::path(&config_dir).display(), ?config, "configuração");

            let key = match secrets::get() {
                Ok(key) => key,
                Err(e) => {
                    tracing::error!(error = %e, "não foi possível ler a chave da xAI do cofre");
                    None
                }
            };
            let key_set = key.is_some();
            // Quem já usava o Ditok (config.json de antes do guia e chave da xAI guardada)
            // não precisa do guia de primeira execução.
            if predates_onboarding && key_set && !config.onboarding_done {
                config.onboarding_done = true;
                if let Err(e) = config.save(&config_dir) {
                    tracing::warn!(error = %e, "não foi possível marcar o guia como feito");
                }
            }
            let show_window = !config.onboarding_done || !key_set;

            let tray = tray::Tray::new(app.handle(), tx.clone(), &config)?;
            // `Invalid` = o texto do atalho no config não se lê (não é culpa de outra app).
            let mut shortcut_invalid = false;
            // `Unavailable`: o sistema não deixou ouvir um atalho só de modificadores
            // (no Mac, falta a permissão); guarda o motivo para o aviso.
            let mut shortcut_unavailable = None;
            let shortcut_ok = match hotkey::register_dictation(app.handle(), &config.shortcut) {
                Ok(()) => true,
                Err(e) => {
                    tracing::error!(error = %e, "sem atalho de ditado");
                    shortcut_invalid = matches!(e, hotkey::HotkeyError::Invalid { .. });
                    if let hotkey::HotkeyError::Unavailable { reason, .. } = &e {
                        shortcut_unavailable = Some(reason.clone());
                    }
                    tray.set_tooltip(if shortcut_invalid {
                        "Ditok: atalho inválido no config"
                    } else if shortcut_unavailable.is_some() {
                        "Ditok: não consigo ouvir o atalho"
                    } else {
                        "Ditok: atalho ocupado por outra app"
                    });
                    false
                }
            };
            // O modo comando é um extra: se o atalho estiver ocupado, fica só o
            // aviso nas definições e no menu do tray (sem notificação, para não
            // incomodar quem nunca o usa).
            let command_ok = !config.command_enabled
                || match hotkey::register_command(app.handle(), &config.command_shortcut) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(error = %e, "sem atalho de comando");
                        false
                    }
                };

            // O histórico rápido é opcional: sem atalho no config, nada a registar; com
            // o atalho ocupado, fica só o aviso nas definições.
            let quick = config.quick_history_shortcut.trim().to_owned();
            let quick_ok = quick.is_empty()
                || match hotkey::register_quick(app.handle(), &quick) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(error = %e, "sem atalho do histórico rápido");
                        false
                    }
                };

            // Perguntar sobre o ecrã também é um extra: atalho ocupado = só o aviso nas definições.
            let screen_ok = !config.screen_enabled
                || match hotkey::register_screen(app.handle(), &config.screen_shortcut) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(error = %e, "sem atalho de perguntar sobre o ecrã");
                        false
                    }
                };

            let dirs = state::Dirs {
                config: config_dir,
                data: paths.app_data_dir()?,
                cache: paths.app_cache_dir()?,
                log: paths.app_log_dir()?,
            };
            let state = Arc::new(state::AppState::new(
                app.handle().clone(),
                config,
                dirs,
                xai::http_client()?,
                tx.clone(),
            ));
            // Os comandos da janela recebem o mesmo `Arc` via `State<Arc<AppState>>`.
            app.manage(Arc::clone(&state));
            // Passa as contas antigas para as estatísticas de uso e apara metrics.jsonl
            // (fora da thread de arranque: lê um ficheiro).
            let usage_state = Arc::clone(&state);
            tauri::async_runtime::spawn_blocking(move || usage_state.usage.startup());
            // Sessões de notas que ficaram abertas (a app fechou a meio) fecham-se como interrompidas.
            let notes_dir = state.data_dir.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let fixed = notes::store::NotesStore::new(&notes_dir).recover(None);
                if !fixed.is_empty() {
                    tracing::warn!(?fixed, "sessões de notas interrompidas recuperadas");
                }
            });
            // O menu do tray nasce com a linha do atalho certa; isto só a marca
            // "ocupado" se for o caso (a barra pede o estado quando carregar).
            state.set_key(key);
            state.set_command_ok(command_ok);
            state.set_quick_ok(quick_ok);
            state.set_screen_ok(screen_ok);
            state.set_shortcut_ok(shortcut_ok);
            if !shortcut_ok {
                let shortcut = hotkey::pretty(&state.config().shortcut);
                state.notify(&if shortcut_invalid {
                    format!("O atalho {shortcut} do config não é válido. Escolhe outro nas Definições.")
                } else if let Some(reason) = shortcut_unavailable {
                    format!("Não consegui ouvir o atalho {shortcut}: {reason}.")
                } else {
                    format!(
                        "Não consegui usar o atalho {shortcut}: outra app já o ocupa. Clica na barra ou abre o Ditok para escolher outro."
                    )
                });
            }
            if state.config().show_overlay {
                // Criar um WebView demora ~300 ms. Fazemo-lo numa thread à parte,
                // depois de o tray e o atalho estarem prontos: o arranque continua
                // instantâneo e a barra aparece um instante depois.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || overlay::create(&handle));
            }
            // Arruma o áudio guardado dos ditados: um apagado cujo "Desfazer" não
            // chegou a expirar (a app fechou antes) ou a opção desligada à mão no
            // config.json. É disco, por isso numa thread à parte.
            let state_ = Arc::clone(&state);
            tauri::async_runtime::spawn_blocking(move || {
                let result = if state_.config().keep_audio {
                    state_.history.prune_audio(history::AUDIO_KEEP).map(drop)
                } else {
                    state_.history.clear_audio()
                };
                if let Err(e) = result {
                    tracing::warn!(error = %e, "não foi possível arrumar o áudio dos ditados");
                }
            });
            // O nível do microfone vai diretamente da thread de áudio para a barra.
            let handle = app.handle().clone();
            let level_sink: audio::LevelSink = Box::new(move |level| overlay::emit_level(&handle, level));
            // O medidor do guia (fora de um ditado) vai para a janela principal.
            let handle = app.handle().clone();
            let mic_sink: audio::MicSink = Box::new(move |event| {
                let _ = handle.emit_to(settings_window::LABEL, "mic:level", event);
            });
            let recorder = audio::Recorder::spawn(tx.clone(), level_sink, mic_sink)?;
            // Uma cópia para os comandos do medidor (`State<Recorder>`).
            app.manage(recorder.clone());
            let sounds = sound::Sounds::spawn()?;
            updater::spawn_background_checks(app.handle().clone(), Arc::clone(&state));
            let deps = dictation::Deps {
                tray,
                recorder,
                sounds,
                state,
            };
            dictation::spawn(deps, tx, rx);

            // Primeira execução (ou sem chave): sem a janela, ninguém saberia o que
            // fazer a seguir. Abre já no guia ou no aviso da chave.
            if show_window {
                settings_window::open(app.handle());
            }

            tracing::info!(elapsed_ms = started.elapsed().as_millis(), "pronto");
            sysmem::log_after(Duration::from_secs(5), "em repouso, sem janela");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::set_api_key,
            commands::delete_api_key,
            commands::test_api_key,
            commands::list_models,
            commands::get_stats,
            commands::get_history,
            commands::clear_history,
            commands::delete_history_entry,
            commands::restore_history_entry,
            commands::reprocess_history_entry,
            commands::recent_apps,
            commands::copy_text,
            commands::set_autostart,
            commands::get_memory,
            commands::check_update,
            commands::install_update,
            commands::overlay_click,
            commands::overlay_screen,
            commands::overlay_cancel,
            commands::overlay_hit_rect,
            commands::file_probe,
            commands::file_pick,
            commands::file_start,
            commands::file_cancel,
            commands::file_status,
            commands::file_clear,
            commands::file_save_beside,
            commands::dictionary_export,
            commands::dictionary_import_preview,
            commands::usage_summary,
            commands::usage_clear,
            commands::correct_history_entry,
            commands::summarize_text,
            commands::file_save_summary,
            commands::notes_state,
            commands::notes_accept_consent,
            commands::notes_start,
            commands::notes_stop,
            commands::notes_list,
            commands::notes_get,
            commands::notes_delete,
            commands::notes_rename,
            commands::notes_text,
            commands::notes_save_summary,
            commands::notes_export,
            commands::notes_output_devices,
            commands::notes_test_start,
            commands::notes_rename_speaker,
            commands::assistant_state,
            commands::assistant_send,
            commands::assistant_stop,
            commands::assistant_retry,
            commands::assistant_new,
            commands::assistant_open,
            commands::assistant_list,
            commands::assistant_delete,
            commands::assistant_clear,
            commands::assistant_copy,
            commands::assistant_open_source,
            commands::assistant_apply,
            commands::assistant_close,
            commands::assistant_capture,
            commands::screen_view,
            commands::screen_image,
            commands::screen_ready,
            commands::screen_submit,
            commands::screen_cancel,
            commands::screen_report,
            commands::overlay_status,
            commands::overlay_open,
            commands::overlay_menu,
            commands::main_take_open,
            commands::open_xai_console,
            commands::list_input_devices,
            commands::mic_monitor,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => {
            eprintln!("Ditok não arrancou: {e}");
            std::process::exit(1);
        }
    };

    app.run(
        |#[cfg_attr(not(target_os = "macos"), allow(unused_variables))] app, event| match event {
            // macOS: clicar no ícone da Dock (ou abrir a app no Finder) com ela já a
            // correr. A barra flutuante conta como janela visível, por isso ignoramos
            // `has_visible_windows` e abrimos sempre a janela principal.
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => settings_window::open(app),
            // Sem janelas, o Tauri pede para sair quando a última janela fecha. Só
            // deixamos sair quando há código de saída, i.e. via `app.exit(0)` no "Sair".
            RunEvent::ExitRequested {
                api, code: None, ..
            } => api.prevent_exit(),
            RunEvent::Exit => {
                tracing::info!("a sair");
                logging::shutdown();
            }
            _ => {}
        },
    );
}
