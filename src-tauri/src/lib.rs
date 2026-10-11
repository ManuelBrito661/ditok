mod audio;
mod dictation;
mod hotkey;
mod logging;
mod tray;

use std::time::Instant;

use tauri::{Manager, RunEvent};

pub fn run() {
    let started = Instant::now();

    // Canal do orquestrador. O `Sender` vai para o atalho (e mais tarde para o
    // áudio/rede); o `Receiver` tem um único dono: a tarefa do orquestrador.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

    let app = tauri::Builder::default()
        // Tem de ser o primeiro plugin: uma segunda instância lutaria pelo atalho.
        .plugin(tauri_plugin_single_instance::init(|_app, argv, _cwd| {
            tracing::info!(?argv, "segunda instância ignorada");
        }))
        .plugin(hotkey::plugin(tx.clone()))
        .setup(move |app| {
            logging::init(&app.path().app_log_dir()?)?;
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "Ditok a arrancar");

            // macOS: app de barra de menus, sem ícone na Dock.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let tray = tray::Tray::new(app.handle())?;
            if let Err(e) = hotkey::register_dictation(app.handle(), hotkey::DEFAULT_SHORTCUT) {
                tracing::error!(error = %e, "sem atalho de ditado");
                tray.set_tooltip("Ditok: atalho indisponível (ver log)");
            }
            let recorder = audio::Recorder::spawn(tx.clone())?;
            let deps = dictation::Deps {
                app: app.handle().clone(),
                tray,
                recorder,
                debug_dir: app.path().app_data_dir()?.join("debug"),
            };
            dictation::spawn(deps, tx, rx);

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
