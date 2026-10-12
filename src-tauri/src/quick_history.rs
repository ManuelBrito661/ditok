//! Histórico rápido: um atalho abre um menu do sistema junto ao cursor com os
//! últimos ditados, e o escolhido é colado na janela onde estavas.
//!
//! É um menu nativo (o mesmo tipo do menu do tray) e não uma janela nossa: custa
//! 0 MB de RAM, tem setas, Enter, Esc e as teclas 1–9 sem código nosso, e não
//! rouba o foco à app de destino. O preço é não ter caixa de pesquisa (para isso
//! há a página Início).
//!
//! O caminho: atalho → `DictationEvent::QuickHistory` → `open` (guarda a janela em
//! foco e mostra o menu) → o utilizador escolhe → o handler do menu chama
//! `chosen` → `DictationEvent::PasteText` → o orquestrador cola.

use std::sync::Arc;

use tauri::{
    AppHandle, Manager, Wry,
    menu::{Menu, MenuBuilder, MenuItemBuilder},
};

use crate::{
    dictation::DictationEvent, history::HistoryEntry, overlay, settings_window, state::AppState,
    tray,
};

/// Os itens de menu das entradas têm o id `qh:<id da entrada>`.
pub const ID_PREFIX: &str = "qh:";
/// Id do item informativo ("Ainda não há ditados"), que não faz nada.
const INFO_ID: &str = "qh-info";
/// Quantas entradas se mostram: as teclas 1 a 9 escolhem-nas.
const SHOWN: usize = 9;
/// Quantos carateres de cada ditado cabem numa linha do menu.
const LABEL_CHARS: usize = 60;

/// Uma linha do menu. `id` é `None` numa linha só informativa (desativada).
#[derive(Debug, PartialEq, Eq)]
pub struct Item {
    pub id: Option<String>,
    pub label: String,
}

/// As linhas do menu para estas entradas (a mais recente primeiro).
pub fn menu_items(entries: &[HistoryEntry], history_enabled: bool) -> Vec<Item> {
    let info = |label: &str| {
        vec![Item {
            id: None,
            label: label.to_owned(),
        }]
    };
    if !history_enabled {
        return info("O histórico está desligado (Definições › Dados e privacidade)");
    }
    let items: Vec<Item> = entries
        .iter()
        .filter(|e| !e.text.trim().is_empty())
        .take(SHOWN)
        .enumerate()
        .map(|(i, e)| Item {
            id: Some(format!("{ID_PREFIX}{}", e.id)),
            label: label(i + 1, &e.text),
        })
        .collect();
    if items.is_empty() {
        return info("Ainda não há ditados");
    }
    items
}

/// "&1  Olá Pedro, tudo bem…": o `&` marca a tecla que escolhe a linha (no Windows
/// sublinha-a); o texto vai numa só linha, cortado, e os seus `&` escrevem-se
/// `&&` para não virarem teclas.
fn label(n: usize, text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut shown: String = flat.chars().take(LABEL_CHARS).collect();
    if flat.chars().count() > LABEL_CHARS {
        shown.push('…');
    }
    format!("&{n}  {}", shown.replace('&', "&&"))
}

fn build_menu(app: &AppHandle, items: &[Item]) -> tauri::Result<Menu<Wry>> {
    let mut builder = MenuBuilder::new(app);
    for item in items {
        let entry = MenuItemBuilder::with_id(item.id.as_deref().unwrap_or(INFO_ID), &item.label)
            .enabled(item.id.is_some())
            .build(app)?;
        builder = builder.item(&entry);
    }
    builder.build()
}

/// Mostra o menu junto ao cursor. Corre numa thread de bloqueio: ler o histórico
/// é disco, e `popup_menu` só volta quando o menu fecha.
pub fn open(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(state) = app.try_state::<Arc<AppState>>() else {
            return;
        };
        // Neste instante a janela em primeiro plano ainda é a tua: é lá que se cola.
        tray::remember_menu_target();
        let config = state.config();
        let entries = if config.history_enabled {
            state.history.list()
        } else {
            Vec::new()
        };
        let items = menu_items(&entries, config.history_enabled);
        match build_menu(&app, &items) {
            Ok(menu) => popup(&app, &menu),
            Err(e) => tracing::warn!(error = %e, "não foi possível montar o histórico rápido"),
        }
    });
}

/// Um menu precisa de uma janela a que pertencer. Usa a barra flutuante ou, se
/// não existir, a janela principal; sem nenhuma (a barra está desligada e a janela
/// fechada), cria a barra só para isto e destrói-a a seguir.
fn popup(app: &AppHandle, menu: &Menu<Wry>) {
    let existing = app
        .get_webview_window(overlay::LABEL)
        .or_else(|| app.get_webview_window(settings_window::LABEL));
    let (window, temporary) = match existing {
        Some(window) => (window, false),
        None => {
            overlay::create(app);
            match app.get_webview_window(overlay::LABEL) {
                Some(window) => (window, true),
                None => {
                    tracing::warn!("sem janela onde abrir o histórico rápido");
                    return;
                }
            }
        }
    };
    if let Err(e) = window.popup_menu(menu) {
        tracing::warn!(error = %e, "não foi possível abrir o histórico rápido");
    }
    if temporary {
        overlay::destroy(app);
    }
}

/// Uma linha do menu foi escolhida: cola o texto da entrada.
pub fn chosen(app: &AppHandle, entry_id: &str) {
    let app = app.clone();
    let entry_id = entry_id.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(state) = app.try_state::<Arc<AppState>>() else {
            return;
        };
        let Some(entry) = state.history.get(&entry_id) else {
            tracing::warn!("histórico rápido: a entrada escolhida já não existe");
            return;
        };
        tracing::info!("histórico rápido: colar uma entrada");
        let _ = state.events.send(DictationEvent::PasteText(entry.text));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn entry(id: &str, text: &str) -> HistoryEntry {
        HistoryEntry {
            id: id.into(),
            at: datetime!(2026-10-12 12:00 UTC),
            text: text.into(),
            raw_text: None,
            model: "m/m".into(),
            audio_ms: 1000,
            total_ms: Some(500),
            pasted: true,
            app: None,
            style: None,
            instruction: None,
            style_used: None,
            corrected: false,
        }
    }

    #[test]
    fn recent_for_menu_truncates_and_limits() {
        let entries: Vec<HistoryEntry> = (0..15)
            .map(|i| entry(&i.to_string(), &format!("ditado {i}")))
            .collect();
        let items = menu_items(&entries, true);
        assert_eq!(items.len(), SHOWN, "só as 9 mais recentes");
        assert_eq!(items[0].id.as_deref(), Some("qh:0"));
        assert_eq!(items[0].label, "&1  ditado 0");
        assert_eq!(items[8].label, "&9  ditado 8");

        let long = entry("x", &"palavra ".repeat(40));
        let label = &menu_items(&[long], true)[0].label;
        assert!(label.ends_with('…'));
        assert_eq!(label.chars().count(), "&1  ".len() + LABEL_CHARS + 1);
    }

    #[test]
    fn labels_are_one_line_and_escape_ampersands() {
        let items = menu_items(&[entry("a", "Olá\n\nPedro   &  Ana")], true);
        assert_eq!(items[0].label, "&1  Olá Pedro && Ana");
    }

    #[test]
    fn empty_entries_are_skipped() {
        let entries = [entry("a", "   "), entry("b", "texto")];
        let items = menu_items(&entries, true);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_deref(), Some("qh:b"));
        assert_eq!(items[0].label, "&1  texto");
    }

    #[test]
    fn no_history_or_disabled_history_gives_one_inert_line() {
        let none = menu_items(&[], true);
        assert_eq!(none.len(), 1);
        assert!(none[0].id.is_none());
        let off = menu_items(&[entry("a", "x")], false);
        assert_eq!(off.len(), 1);
        assert!(off[0].id.is_none());
        assert!(off[0].label.contains("desligado"));
    }

    #[test]
    fn menu_ids_are_valid_history_ids() {
        let entries = [entry("1760000000000000000", "x")];
        let id = menu_items(&entries, true)[0].id.clone().unwrap();
        let entry_id = id.strip_prefix(ID_PREFIX).unwrap();
        assert!(crate::history::is_valid_id(entry_id));
    }
}
