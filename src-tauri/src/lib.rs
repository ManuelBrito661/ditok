mod audio;
mod cleanup;
mod commands;
mod config;
mod dictation;
mod history;
mod hotkey;
mod logging;
mod metrics;
mod overlay;
mod paste;
pub mod secrets;
mod settings_window;
mod state;
mod stt;
mod sysmem;
mod tray;
mod updater;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use tauri::{Manager, RunEvent};

pub fn run() {
    let started = Instant::now();

    // Canal do orquestrador. Os `Sender` vão para o atalho, o áudio e as tarefas de
    // processamento; o `Receiver` tem um único dono: a tarefa do orquestrador.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    let app = tauri::Builder::default()
        // Tem de ser o primeiro plugin: uma segunda instância lutaria pelo atalho.
        .plugin(tauri_plugin_single_instance::init(|_app, argv, _cwd| {
            tracing::info!(?argv, "segunda instância ignorada");
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // Updates assinados vindos das Releases do GitHub; `process` serve para reiniciar.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(hotkey::plugin(tx.clone()))
        .setup(move |app| {
            let paths = app.path();
            logging::init(&paths.app_log_dir()?)?;
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "Ditok a arrancar");

            // macOS: app de barra de menus, sem ícone na Dock.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_dir = paths.app_config_dir()?;
            let config = config::Config::load_or_create(&config_dir);
            tracing::info!(path = %config::Config::path(&config_dir).display(), ?config, "configuração");

            let tray = tray::Tray::new(app.handle())?;
            if let Err(e) = hotkey::register_dictation(app.handle(), &config.shortcut) {
                tracing::error!(error = %e, "sem atalho de ditado");
                tray.set_tooltip("Ditok: atalho indisponível (ver log)");
            }

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
                stt::http_client()?,
                tx.clone(),
            ));
            // Os comandos da janela recebem o mesmo `Arc` via `State<Arc<AppState>>`.
            app.manage(Arc::clone(&state));
            if state.config().show_overlay {
                // Criar um WebView demora ~300 ms. Fazemo-lo numa thread à parte,
                // depois de o tray e o atalho estarem prontos: o arranque continua
                // instantâneo e a barra aparece um instante depois.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn_blocking(move || overlay::create(&handle));
            }
            // O nível do microfone vai diretamente da thread de áudio para a barra.
            let handle = app.handle().clone();
            let level_sink: audio::LevelSink = Box::new(move |level| overlay::emit_level(&handle, level));
            let recorder = audio::Recorder::spawn(tx.clone(), level_sink)?;
            updater::spawn_background_checks(app.handle().clone(), Arc::clone(&state));
            dictation::spawn(dictation::Deps { tray, recorder, state }, tx, rx);

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
            commands::set_autostart,
            commands::get_memory,
            commands::check_update,
            commands::install_update,
        ])
        .build(tauri::generate_context!());

    let app = match app {
        Ok(app) => app,
        Err(e) => {
            eprintln!("Ditok não arrancou: {e}");
            std::process::exit(1);
        }
    };

    app.run(|_app, event| match event {
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
    });
}
