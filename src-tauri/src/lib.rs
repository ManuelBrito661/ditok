mod audio;
mod config;
mod dictation;
mod hotkey;
mod logging;
mod metrics;
mod paste;
pub mod secrets;
mod stt;
mod tray;

use std::{sync::Arc, time::Instant};

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

            let data_dir = paths.app_data_dir()?;
            let pipeline = Arc::new(dictation::pipeline::Pipeline {
                app: app.handle().clone(),
                config,
                http: stt::http_client()?,
                metrics: metrics::MetricsStore::new(data_dir.clone()),
                debug_dir: data_dir.join("debug"),
            });
            let recorder = audio::Recorder::spawn(tx.clone())?;
            dictation::spawn(dictation::Deps { tray, recorder, pipeline }, tx, rx);

            tracing::info!(elapsed_ms = started.elapsed().as_millis(), "pronto");
            Ok(())
        })
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
