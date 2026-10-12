//! Definições em JSON em `app_config_dir` (Windows:
//! %APPDATA%\com.manuelbrito.ditok\config.json). Editadas pela janela de
//! definições; também podem ser editadas à mão (aplicam-se no arranque seguinte).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    hotkey::{self, DEFAULT_COMMAND_SHORTCUT, DEFAULT_SHORTCUT},
    local_models::LocalModelId,
    stt::openrouter::Transport,
    text::{Replacement, Snippet},
};

pub const DEFAULT_STT_MODEL: &str = "openai/whisper-large-v3-turbo";
pub const DEFAULT_CLEANUP_MODEL: &str = "openai/gpt-4.1-nano";

/// `#[serde(default)]` faz com que campos em falta no ficheiro usem o valor de
/// `Default`: um config.json antigo continua válido quando acrescentamos campos.
/// Campos desconhecidos (ex.: o antigo "provider") são ignorados, e `alias` aceita
/// os nomes antigos dos campos renomeados.
///
/// O mesmo struct é enviado à UI (`Serialize`) e recebido dela (`Deserialize`): o
/// TypeScript tem uma interface com os mesmos campos em `src/api.ts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub shortcut: String,
    /// Modo comando (ver `command.rs`): um segundo atalho que aplica uma
    /// instrução falada ao texto selecionado. Desligado, o atalho fica livre
    /// para outras apps, mas a escolha em `command_shortcut` é guardada.
    pub command_enabled: bool,
    pub command_shortcut: String,
    /// Slug completo do modelo no OpenRouter.
    #[serde(alias = "openrouter_model")]
    pub stt_model: String,
    #[serde(alias = "openrouter_transport")]
    pub transport: Transport,
    /// ISO-639-1 (ex.: "pt"); `None` para deteção automática.
    pub language: Option<String>,
    /// Quanto o LLM mexe no texto (ver `cleanup.rs`). Substitui o antigo
    /// `cleanup_enabled`, convertido em `migrate`.
    pub cleanup_level: CleanupLevel,
    pub cleanup_model: String,
    /// Depois de colar, deixa o ditado no clipboard (pronto para colar outra vez)
    /// em vez de repor o que lá estava.
    pub keep_in_clipboard: bool,
    /// Guarda os últimos ditados (texto incluído) em disco, só neste computador.
    pub history_enabled: bool,
    pub history_limit: usize,
    /// Guarda o áudio (WAV 16 kHz) dos últimos `history::AUDIO_KEEP` ditados em
    /// `audio/`, para os poder reprocessar. Desligado por omissão: é a tua voz,
    /// e ocupa espaço (~32 KB por segundo).
    pub keep_audio: bool,
    /// Guarda a última gravação em `debug/last.wav`.
    pub save_last_recording: bool,
    /// Barra flutuante com a mascote. Desligá-la liberta a RAM do WebView dela.
    pub show_overlay: bool,
    /// "nova linha" e "novo parágrafo" viram mudanças de linha (ver `text.rs`).
    pub voice_commands: bool,
    /// "vírgula", "ponto de interrogação", "abre aspas"… viram a pontuação (ver
    /// `text.rs`). Um config.json antigo, sem o campo, recebe o valor por omissão.
    pub spoken_punctuation: bool,
    /// Histórico rápido: com este atalho (uma combinação com tecla) abre-se um menu
    /// junto ao cursor com os últimos ditados, e o escolhido é colado onde estavas
    /// (ver `quick_history.rs`). Vazio = desligado: um atalho global "rouba" a
    /// combinação às outras apps, por isso não há nenhum por omissão.
    pub quick_history_shortcut: String,
    /// Quantas palavras por minuto escreves à mão; só serve para estimar o tempo
    /// que o ditado te poupa (página Métricas).
    pub typing_wpm: u32,
    /// Modelo de texto dos resumos; vazio = o da limpeza (`cleanup_model`).
    pub summary_model: String,
    /// Quem transcreve: o OpenRouter (nuvem) ou o motor local, neste computador
    /// (ver `stt/local.rs`). Com o local, o áudio não sai daqui e não custa nada; a
    /// limpeza com IA, os comandos e os resumos continuam a precisar de rede e de chave.
    pub stt_engine: SttEngine,
    /// O modelo do motor local (tem de estar descarregado: `local_models.rs`).
    pub local_model: LocalModelId,
    /// Minutos sem ditar até o modelo local ser largado da memória (ocupa 0,7 a 2,4 GB).
    pub local_idle_minutes: u32,
    /// O aviso sobre gravar conversas foi aceite (ver `notes/`). Sem isto o notetaker não grava.
    pub notes_consent: bool,
    /// Nas Notas, separar quem fala dentro de "Outros" (Orador 1, 2…) no fim da reunião,
    /// neste computador (ver `notes/speakers.rs`). Precisa do motor local e de dois modelos.
    pub notes_speakers: bool,
    /// Palavras escritas à tua maneira depois da transcrição ("tauri" → "Tauri").
    pub dictionary: Vec<Replacement>,
    /// Frases que dão lugar a um texto ("o meu email" → o endereço).
    pub snippets: Vec<Snippet>,
    /// Sons curtos ao começar e ao acabar de gravar (ver `sound.rs`).
    pub sounds: bool,
    /// Orçamento mensal em USD; `None` = sem orçamento. Há um aviso aos 80 % e
    /// outro aos 100 % (ver `metrics::BudgetAlert`).
    pub monthly_budget_usd: Option<f64>,
    /// Microfone escolhido, pelo nome que o sistema lhe dá; `None` = o
    /// predefinido do sistema. Se deixar de existir, uso o predefinido (ver
    /// `audio/capture.rs`).
    pub input_device: Option<String>,
    /// Saída cujo áudio se capta nas notas de reunião (o que os outros dizem), pelo
    /// nome que o sistema lhe dá; `None` = a saída predefinida do sistema, que
    /// acompanha as mudanças. Se deixar de existir, uso a predefinida.
    pub notes_output_device: Option<String>,
    /// O guia de primeira execução já foi concluído (ou saltado). Um config.json
    /// sem este campo é de uma versão anterior ao guia: ver `predates_onboarding`.
    pub onboarding_done: bool,
    /// Estilo por app: a app onde estavas quando começaste a ditar decide o tom
    /// da limpeza (ver `Config::style_for`). Um config.json sem o campo recebe as
    /// regras iniciais (`default_styles`), por isso quem atualiza também as tem.
    pub styles: Vec<StyleRule>,
}

/// Quem transcreve os ditados. No JSON: "cloud" ou "local".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SttEngine {
    /// O OpenRouter, com `stt_model`.
    #[default]
    Cloud,
    /// O motor neste computador, com `local_model`.
    Local,
}

/// Quanto a limpeza com IA mexe no texto. `rename_all = "lowercase"`: no JSON
/// fica "off", "light", "medium" ou "high".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupLevel {
    /// Sem LLM: nem custo nem latência extra.
    #[default]
    Off,
    /// Só pontuação, maiúsculas e hesitações; não reescreve.
    Light,
    /// O comportamento de sempre (também tira muletas e repetições).
    Medium,
    /// Reescreve para ficar claro e bem estruturado.
    High,
}

/// O tom de um ditado, escolhido pela app onde estás a escrever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// "Predefinido": usa só o nível global.
    #[default]
    Default,
    /// Mensagens: frases curtas, sem formalidades.
    Casual,
    /// Email e documentos: frases completas, pontuação cuidada.
    Formal,
    /// Terminal e IDE: nunca passa pelo LLM, seja qual for o nível.
    Literal,
}

impl Style {
    /// O nome que aparece na UI (pílula, histórico).
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Predefinido",
            Self::Casual => "Casual",
            Self::Formal => "Formal",
            Self::Literal => "Literal",
        }
    }
}

/// "Nesta app, este estilo." `app` é o nome do executável ("whatsapp.exe");
/// a comparação ignora maiúsculas e o ".exe" (ver `app_key`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleRule {
    pub app: String,
    pub style: Style,
}

/// As regras com que se começa, com os nomes que `focus::app_name` devolve em
/// cada sistema: executáveis no Windows, nomes do Dock no macOS.
fn default_styles() -> Vec<StyleRule> {
    #[cfg(target_os = "macos")]
    const RULES: &[(&str, Style)] = &[
        ("whatsapp", Style::Casual),
        ("discord", Style::Casual),
        ("slack", Style::Casual),
        ("telegram", Style::Casual),
        ("microsoft outlook", Style::Formal),
        ("microsoft word", Style::Formal),
        ("mail", Style::Formal),
        ("thunderbird", Style::Formal),
        ("terminal", Style::Literal),
        ("iterm2", Style::Literal),
        ("visual studio code", Style::Literal),
        ("wezterm", Style::Literal),
    ];
    #[cfg(not(target_os = "macos"))]
    const RULES: &[(&str, Style)] = &[
        ("whatsapp.exe", Style::Casual),
        ("discord.exe", Style::Casual),
        ("slack.exe", Style::Casual),
        ("telegram.exe", Style::Casual),
        ("outlook.exe", Style::Formal),
        ("olk.exe", Style::Formal),
        ("winword.exe", Style::Formal),
        ("thunderbird.exe", Style::Formal),
        ("windowsterminal.exe", Style::Literal),
        ("cmd.exe", Style::Literal),
        ("powershell.exe", Style::Literal),
        ("pwsh.exe", Style::Literal),
        ("code.exe", Style::Literal),
        ("wezterm-gui.exe", Style::Literal),
    ];
    // `&(app, style)` desestrutura cada tuplo já na assinatura do closure.
    RULES
        .iter()
        .map(|&(app, style)| StyleRule {
            app: app.to_owned(),
            style,
        })
        .collect()
}

/// A forma de comparar nomes de apps: sem espaços nas pontas, em minúsculas e
/// sem ".exe" ("WhatsApp.exe", "whatsapp" e " whatsapp.EXE " dão o mesmo).
pub fn app_key(app: &str) -> String {
    let app = app.trim().to_lowercase();
    match app.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => app,
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcut: DEFAULT_SHORTCUT.to_owned(),
            command_enabled: true,
            command_shortcut: DEFAULT_COMMAND_SHORTCUT.to_owned(),
            stt_model: DEFAULT_STT_MODEL.to_owned(),
            transport: Transport::Multipart,
            language: Some("pt".to_owned()),
            cleanup_level: CleanupLevel::Off,
            cleanup_model: DEFAULT_CLEANUP_MODEL.to_owned(),
            keep_in_clipboard: true,
            history_enabled: true,
            history_limit: 50,
            keep_audio: false,
            save_last_recording: false,
            show_overlay: true,
            voice_commands: true,
            spoken_punctuation: true,
            quick_history_shortcut: String::new(),
            typing_wpm: 40,
            summary_model: String::new(),
            stt_engine: SttEngine::Cloud,
            local_model: LocalModelId::default(),
            local_idle_minutes: 5,
            notes_consent: false,
            notes_speakers: false,
            dictionary: Vec::new(),
            snippets: Vec::new(),
            sounds: true,
            monthly_budget_usd: None,
            input_device: None,
            notes_output_device: None,
            onboarding_done: false,
            styles: default_styles(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("o modelo tem de ser o slug completo, com \"/\" (ex.: {DEFAULT_STT_MODEL})")]
    Model,
    #[error(
        "idioma inválido: usa um código de 2 letras (ex.: pt) ou deixa vazio para deteção automática"
    )]
    Language,
    #[error("o histórico tem de guardar entre 1 e 1000 ditados")]
    HistoryLimit,
    #[error("cada palavra do dicionário precisa de saber como fica escrita")]
    DictionaryEntry,
    #[error("cada expansão precisa de uma frase e de um texto")]
    SnippetEntry,
    #[error("o dicionário e as expansões têm um limite de {MAX_ENTRIES} entradas cada")]
    TooManyEntries,
    #[error("uma entrada do dicionário ou das expansões é demasiado longa")]
    EntryTooLong,
    #[error("o orçamento tem de ser um valor em USD maior do que 0 (ou vazio para não ter)")]
    Budget,
    #[error("o nome do microfone é demasiado longo")]
    InputDevice,
    #[error("o nome da saída de áudio é demasiado longo")]
    NotesOutputDevice,
    #[error("cada estilo precisa do nome da app")]
    StyleEntry,
    #[error("já há um estilo para {0}")]
    DuplicateStyle(String),
    #[error("os estilos têm um limite de {MAX_STYLES} apps")]
    TooManyStyles,
    #[error("o atalho de comando tem de ser diferente do atalho de ditado")]
    SameShortcut,
    #[error("o atalho do histórico rápido tem de ser diferente dos de ditado e de comando")]
    QuickShortcutClash,
    #[error("a velocidade a escrever tem de estar entre 5 e 300 palavras por minuto")]
    TypingSpeed,
    #[error("o tempo até largar o modelo local tem de estar entre 1 e 240 minutos")]
    LocalIdle,
    #[error(
        "o modelo dos resumos tem de ser o slug completo, com \"/\" (ex.: {DEFAULT_CLEANUP_MODEL}), ou ficar vazio"
    )]
    SummaryModel,
}

/// Limites do dicionário e das expansões: muito acima do que se usa, só para um
/// config.json editado à mão não pôr o regex a compilar durante segundos.
pub(crate) const MAX_ENTRIES: usize = 1000;
pub(crate) const MAX_PHRASE_CHARS: usize = 200;
pub(crate) const MAX_SNIPPET_CHARS: usize = 5000;
const MAX_STYLES: usize = 200;
/// Um nome de ficheiro no Windows não passa de 260 caracteres (MAX_PATH).
const MAX_APP_CHARS: usize = 260;

impl Config {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("config.json")
    }

    /// Verifica o que vem da UI antes de o guardar.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let slug_ok = |s: &str| s.contains('/') && !s.contains(char::is_whitespace);
        if !slug_ok(&self.stt_model) || !slug_ok(&self.cleanup_model) {
            return Err(ConfigError::Model);
        }
        if let Some(lang) = &self.language
            && !(lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase()))
        {
            return Err(ConfigError::Language);
        }
        if !(1..=1000).contains(&self.history_limit) {
            return Err(ConfigError::HistoryLimit);
        }
        if let Some(budget) = self.monthly_budget_usd
            && !(budget.is_finite() && budget > 0.0 && budget <= 100_000.0)
        {
            return Err(ConfigError::Budget);
        }
        if self
            .input_device
            .as_deref()
            .is_some_and(|d| d.chars().count() > MAX_PHRASE_CHARS)
        {
            return Err(ConfigError::InputDevice);
        }
        if self
            .notes_output_device
            .as_deref()
            .is_some_and(|d| d.chars().count() > MAX_PHRASE_CHARS)
        {
            return Err(ConfigError::NotesOutputDevice);
        }
        if self.dictionary.len() > MAX_ENTRIES || self.snippets.len() > MAX_ENTRIES {
            return Err(ConfigError::TooManyEntries);
        }
        let too_long = |s: &str, max: usize| s.chars().count() > max;
        for r in &self.dictionary {
            if r.to.trim().is_empty() {
                return Err(ConfigError::DictionaryEntry);
            }
            if too_long(&r.from, MAX_PHRASE_CHARS) || too_long(&r.to, MAX_PHRASE_CHARS) {
                return Err(ConfigError::EntryTooLong);
            }
        }
        for s in &self.snippets {
            if s.trigger.trim().is_empty() || s.text.trim().is_empty() {
                return Err(ConfigError::SnippetEntry);
            }
            if too_long(&s.trigger, MAX_PHRASE_CHARS) || too_long(&s.text, MAX_SNIPPET_CHARS) {
                return Err(ConfigError::EntryTooLong);
            }
        }
        // Com o modo desligado, o atalho guardado não é registado: pode repetir-se.
        if self.command_enabled && hotkey::same_shortcut(&self.shortcut, &self.command_shortcut) {
            return Err(ConfigError::SameShortcut);
        }
        let summary = self.summary_model.trim();
        if !summary.is_empty() && !slug_ok(summary) {
            return Err(ConfigError::SummaryModel);
        }
        if !(1..=240).contains(&self.local_idle_minutes) {
            return Err(ConfigError::LocalIdle);
        }
        if !(5..=300).contains(&self.typing_wpm) {
            return Err(ConfigError::TypingSpeed);
        }
        let quick = self.quick_history_shortcut.trim();
        if !quick.is_empty()
            && (hotkey::same_shortcut(quick, &self.shortcut)
                || (self.command_enabled && hotkey::same_shortcut(quick, &self.command_shortcut)))
        {
            return Err(ConfigError::QuickShortcutClash);
        }
        if self.styles.len() > MAX_STYLES {
            return Err(ConfigError::TooManyStyles);
        }
        let mut seen = std::collections::HashSet::new();
        for rule in &self.styles {
            let key = app_key(&rule.app);
            if key.is_empty() {
                return Err(ConfigError::StyleEntry);
            }
            if too_long(&rule.app, MAX_APP_CHARS) {
                return Err(ConfigError::EntryTooLong);
            }
            // `insert` devolve `false` se a chave já lá estava.
            if !seen.insert(key) {
                return Err(ConfigError::DuplicateStyle(rule.app.trim().to_owned()));
            }
        }
        Ok(())
    }

    /// O "modelo" que vai no pedido de transcrição e fica nas métricas e no histórico:
    /// o do OpenRouter, ou `local/<modelo>` com o motor local.
    pub fn active_stt_model(&self) -> String {
        match self.stt_engine {
            SttEngine::Cloud => self.stt_model.clone(),
            SttEngine::Local => crate::stt::local::slug(self.local_model),
        }
    }

    /// O modelo dos resumos: o escolhido, ou o da limpeza se estiver vazio.
    pub fn effective_summary_model(&self) -> &str {
        let model = self.summary_model.trim();
        if model.is_empty() {
            &self.cleanup_model
        } else {
            model
        }
    }

    /// O estilo para a app onde o ditado começou. Sem app conhecida (macOS, ou
    /// não deu para a saber) ou sem regra para ela, é o Predefinido.
    pub fn style_for(&self, app: Option<&str>) -> Style {
        let Some(key) = app.map(app_key).filter(|k| !k.is_empty()) else {
            return Style::Default;
        };
        self.styles
            .iter()
            .find(|r| app_key(&r.app) == key)
            .map_or(Style::Default, |r| r.style)
    }

    /// Lê um config.json, convertendo primeiro os campos de versões anteriores.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        let mut value: serde_json::Value = serde_json::from_slice(bytes)?;
        migrate(&mut value);
        serde_json::from_value(value)
    }

    /// Lê o ficheiro; se não existir, cria-o com os valores por omissão. Se estiver
    /// inválido, copia-o para config.json.bak e usa os valores por omissão (o
    /// próximo save reescreve o config.json, por isso a cópia guarda o que escreveste)
    /// e regista o erro.
    pub fn load_or_create(config_dir: &Path) -> Self {
        let path = Self::path(config_dir);
        match std::fs::read(&path) {
            Ok(bytes) => match Self::from_json(&bytes) {
                Ok(config) => config,
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "config.json inválido; a usar valores por omissão");
                    Self::backup_invalid(config_dir);
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                if let Err(e) = config.save(config_dir) {
                    tracing::warn!(error = %e, "não foi possível criar config.json");
                }
                config
            }
            Err(e) => {
                tracing::error!(path = %path.display(), error = %e, "não foi possível ler config.json");
                Self::backup_invalid(config_dir);
                Self::default()
            }
        }
    }

    /// Copia o config.json ilegível para config.json.bak antes de os valores por
    /// omissão o substituírem. Se a cópia falhar, só regista o aviso.
    fn backup_invalid(config_dir: &Path) {
        let path = Self::path(config_dir);
        let backup = config_dir.join("config.json.bak");
        if let Err(e) = std::fs::copy(&path, &backup) {
            tracing::warn!(error = %e, "não foi possível copiar o config.json inválido para config.json.bak");
        } else {
            tracing::warn!(backup = %backup.display(), "config.json inválido copiado para config.json.bak");
        }
    }

    /// `true` se o config.json existe mas foi escrito por uma versão anterior ao
    /// guia de primeira execução (não tem o campo `onboarding_done`). Quem já
    /// usava o Ditok com a chave guardada não precisa do guia (ver lib.rs).
    ///
    /// Porquê ler o ficheiro outra vez: depois de o `serde` preencher os campos em
    /// falta com o `Default`, um `false` "em falta" e um `false` escrito são iguais.
    pub fn predates_onboarding(config_dir: &Path) -> bool {
        let Ok(bytes) = std::fs::read(Self::path(config_dir)) else {
            return false;
        };
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .is_ok_and(|v| v.is_object() && v.get("onboarding_done").is_none())
    }

    /// Escreve num ficheiro temporário e renomeia: se a app morrer a meio, o
    /// config.json antigo fica intacto em vez de meio escrito.
    pub fn save(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = config_dir.join("config.json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, Self::path(config_dir))
    }
}

/// Converte campos antigos no JSON antes de o `serde` o ler. Trabalhar sobre o
/// `Value` (e não sobre o struct) deixa ver o que estava mesmo escrito no
/// ficheiro: depois de o `serde` preencher os campos em falta, já não se sabe.
fn migrate(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    // Até à 0.4 era `cleanup_enabled: bool`. Ligada era o que hoje é o nível Médio.
    if let Some(old) = obj.remove("cleanup_enabled")
        && !obj.contains_key("cleanup_level")
    {
        let level = if old.as_bool() == Some(true) {
            "medium"
        } else {
            "off"
        };
        obj.insert("cleanup_level".into(), level.into());
    }
    // Até à 0.5 o atalho por omissão era Ctrl+Shift+Espaço. Quem o guardou sem
    // nunca o mudar passa para o novo (a escolha do utilizador escreve sempre
    // "Control+…", por isso nunca coincide com este texto).
    if obj.get("shortcut").and_then(|v| v.as_str()) == Some(hotkey::LEGACY_SHORTCUT) {
        obj.insert("shortcut".into(), DEFAULT_SHORTCUT.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_the_old_cleanup_switch() {
        let on = Config::from_json(br#"{"cleanup_enabled": true}"#).unwrap();
        assert_eq!(on.cleanup_level, CleanupLevel::Medium);
        let off = Config::from_json(br#"{"cleanup_enabled": false}"#).unwrap();
        assert_eq!(off.cleanup_level, CleanupLevel::Off);
        // Se já houver as duas (editado à mão), ganha o campo novo.
        let both =
            Config::from_json(br#"{"cleanup_enabled": true, "cleanup_level": "high"}"#).unwrap();
        assert_eq!(both.cleanup_level, CleanupLevel::High);
        // O campo antigo não volta a ser escrito.
        let json = serde_json::to_value(&on).unwrap();
        assert!(json.get("cleanup_enabled").is_none());
        assert_eq!(json["cleanup_level"], "medium");
    }

    #[test]
    fn the_old_default_shortcut_becomes_the_new_one_but_a_chosen_one_stays() {
        let old = Config::from_json(br#"{"shortcut": "CommandOrControl+Shift+Space"}"#).unwrap();
        assert_eq!(old.shortcut, DEFAULT_SHORTCUT);
        let chosen = Config::from_json(br#"{"shortcut": "Control+Shift+Space"}"#).unwrap();
        assert_eq!(chosen.shortcut, "Control+Shift+Space");
    }

    #[test]
    fn old_configs_get_the_initial_styles_but_an_empty_list_stays_empty() {
        let old = Config::from_json(br#"{"sounds": false}"#).unwrap();
        assert_eq!(old.styles, default_styles());
        assert_eq!(old.style_for(Some("WhatsApp.exe")), Style::Casual);
        let none = Config::from_json(br#"{"styles": []}"#).unwrap();
        assert!(
            none.styles.is_empty(),
            "apagaste as regras todas: não voltam"
        );
    }

    #[test]
    fn style_lookup_ignores_case_and_the_exe_suffix() {
        let c = Config {
            styles: vec![
                StyleRule {
                    app: "Outlook.EXE".into(),
                    style: Style::Formal,
                },
                StyleRule {
                    app: " code ".into(),
                    style: Style::Literal,
                },
            ],
            ..Config::default()
        };
        assert_eq!(c.style_for(Some("outlook.exe")), Style::Formal);
        assert_eq!(c.style_for(Some("OUTLOOK")), Style::Formal);
        assert_eq!(c.style_for(Some("Code.exe")), Style::Literal);
        assert_eq!(c.style_for(Some("notepad.exe")), Style::Default);
        assert_eq!(c.style_for(Some("")), Style::Default);
        assert_eq!(c.style_for(None), Style::Default);
    }

    #[test]
    fn validation_checks_style_rules() {
        let rule = |app: &str| StyleRule {
            app: app.into(),
            style: Style::Casual,
        };
        let empty = Config {
            styles: vec![rule("  ")],
            ..Config::default()
        };
        assert!(matches!(empty.validate(), Err(ConfigError::StyleEntry)));
        let twice = Config {
            styles: vec![rule("slack.exe"), rule("Slack")],
            ..Config::default()
        };
        assert!(matches!(
            twice.validate(),
            Err(ConfigError::DuplicateStyle(_))
        ));
        let long = Config {
            styles: vec![rule(&"x".repeat(300))],
            ..Config::default()
        };
        assert!(matches!(long.validate(), Err(ConfigError::EntryTooLong)));
        let many = Config {
            styles: (0..=MAX_STYLES)
                .map(|i| rule(&format!("app{i}.exe")))
                .collect(),
            ..Config::default()
        };
        assert!(matches!(many.validate(), Err(ConfigError::TooManyStyles)));
    }

    #[test]
    fn old_names_missing_and_unknown_fields_are_tolerated() {
        let c: Config = serde_json::from_str(
            r#"{"language": null, "openrouter_model": "google/chirp-3", "provider": "openrouter"}"#,
        )
        .unwrap();
        assert_eq!(c.language, None);
        assert_eq!(c.stt_model, "google/chirp-3");
        assert_eq!(c.shortcut, DEFAULT_SHORTCUT);
        assert_eq!(
            c.cleanup_level,
            CleanupLevel::Off,
            "limpeza desligada por omissão"
        );
        assert!(
            c.history_enabled,
            "histórico ligado por omissão (o Início mostra os ditados)"
        );
        assert!(
            c.keep_in_clipboard,
            "um config.json antigo passa a deixar o ditado no clipboard"
        );
        assert!(c.voice_commands, "comandos falados ligados por omissão");
        assert!(
            c.spoken_punctuation,
            "pontuação falada ligada por omissão (também para quem atualiza)"
        );
        assert!(c.dictionary.is_empty() && c.snippets.is_empty());
        assert!(c.sounds, "sons ligados por omissão");
        assert_eq!(c.monthly_budget_usd, None);
        assert_eq!(c.input_device, None, "microfone predefinido do sistema");
        assert!(
            !c.keep_audio,
            "o áudio dos ditados não se guarda por omissão"
        );
        assert!(!c.onboarding_done);
        assert!(c.command_enabled, "modo comando ligado por omissão");
        assert_eq!(c.command_shortcut, DEFAULT_COMMAND_SHORTCUT);
    }

    #[test]
    fn command_shortcut_must_differ_from_dictation_when_enabled() {
        let same = Config {
            command_shortcut: "Control+Shift+Space".into(),
            shortcut: "CommandOrControl+Shift+Space".into(),
            ..Config::default()
        };
        // No macOS, "CommandOrControl" é o Cmd e os dois são diferentes.
        if cfg!(not(target_os = "macos")) {
            assert!(matches!(same.validate(), Err(ConfigError::SameShortcut)));
        }
        let off = Config {
            command_enabled: false,
            command_shortcut: DEFAULT_SHORTCUT.into(),
            ..Config::default()
        };
        assert!(off.validate().is_ok());
    }

    #[test]
    fn local_engine_is_off_by_default_and_names_its_model() {
        let mut c = Config::default();
        assert_eq!(
            c.stt_engine,
            SttEngine::Cloud,
            "a nuvem por omissão (também para quem atualiza)"
        );
        assert_eq!(c.local_idle_minutes, 5);
        assert_eq!(c.active_stt_model(), c.stt_model);
        c.stt_engine = SttEngine::Local;
        c.local_model = LocalModelId::WhisperSmall;
        assert_eq!(c.active_stt_model(), "local/whisper-small");
        assert!(c.validate().is_ok());
        c.local_idle_minutes = 0;
        assert!(matches!(c.validate(), Err(ConfigError::LocalIdle)));
        c.local_idle_minutes = 241;
        assert!(matches!(c.validate(), Err(ConfigError::LocalIdle)));
        // Um config.json antigo, sem os campos, lê-se com os valores por omissão.
        let old: Config = serde_json::from_str(r#"{"sounds": false}"#).unwrap();
        assert_eq!(old.stt_engine, SttEngine::Cloud);
        assert_eq!(old.local_model, LocalModelId::ParakeetV3);
        // E o JSON usa nomes em minúsculas.
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["stt_engine"], "local");
        assert_eq!(json["local_model"], "whisper-small");
    }

    #[test]
    fn summary_model_falls_back_to_the_cleanup_model() {
        let mut c = Config::default();
        assert!(c.summary_model.is_empty());
        assert_eq!(c.effective_summary_model(), c.cleanup_model);
        c.summary_model = "  anthropic/claude-haiku-4.5 ".into();
        assert_eq!(c.effective_summary_model(), "anthropic/claude-haiku-4.5");
        assert!(c.validate().is_ok());
        c.summary_model = "sem barra".into();
        assert!(matches!(c.validate(), Err(ConfigError::SummaryModel)));
        c.summary_model = "   ".into();
        assert!(c.validate().is_ok(), "só espaços conta como vazio");
    }

    #[test]
    fn quick_history_is_off_by_default_and_must_differ_from_the_other_shortcuts() {
        let c = Config::default();
        assert!(
            c.quick_history_shortcut.is_empty(),
            "sem atalho por omissão"
        );
        assert!(c.validate().is_ok());
        let clash = Config {
            quick_history_shortcut: c.command_shortcut.clone(),
            ..Config::default()
        };
        assert!(matches!(
            clash.validate(),
            Err(ConfigError::QuickShortcutClash)
        ));
        // Com o modo comando desligado, o atalho dele não está registado: pode repetir-se.
        let free = Config {
            quick_history_shortcut: c.command_shortcut.clone(),
            command_enabled: false,
            ..Config::default()
        };
        assert!(free.validate().is_ok());
    }

    #[test]
    fn detects_configs_from_before_the_onboarding() {
        let dir =
            std::env::temp_dir().join(format!("ditok-onboarding-test-{}", std::process::id()));
        assert!(
            !Config::predates_onboarding(&dir),
            "sem ficheiro: instalação nova"
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(Config::path(&dir), r#"{"sounds": false}"#).unwrap();
        assert!(
            Config::predates_onboarding(&dir),
            "versão antiga, sem o campo"
        );
        Config::default().save(&dir).unwrap();
        assert!(
            !Config::predates_onboarding(&dir),
            "versão nova, com o campo"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Um config.json inválido não se perde: fica copiado para config.json.bak.
    #[test]
    fn invalid_config_is_backed_up_before_defaults() {
        let dir = std::env::temp_dir().join(format!("ditok-bak-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = r#"{"sounds": false, "transport": "#;
        std::fs::write(Config::path(&dir), broken).unwrap();
        let loaded = Config::load_or_create(&dir);
        assert_eq!(loaded, Config::default());
        assert_eq!(
            std::fs::read_to_string(dir.join("config.json.bak")).unwrap(),
            broken
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("ditok-config-test-{}", std::process::id()));
        let c = Config {
            transport: Transport::Json,
            ..Config::default()
        };
        c.save(&dir).unwrap();
        assert_eq!(Config::load_or_create(&dir), c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validation_rejects_bad_input() {
        assert!(Config::default().validate().is_ok());
        let bad_model = Config {
            stt_model: "chirp-3".into(),
            ..Config::default()
        };
        assert!(matches!(bad_model.validate(), Err(ConfigError::Model)));
        let bad_lang = Config {
            language: Some("PT-pt".into()),
            ..Config::default()
        };
        assert!(matches!(bad_lang.validate(), Err(ConfigError::Language)));
        let auto_lang = Config {
            language: None,
            ..Config::default()
        };
        assert!(auto_lang.validate().is_ok());
        let bad_limit = Config {
            history_limit: 0,
            ..Config::default()
        };
        assert!(matches!(
            bad_limit.validate(),
            Err(ConfigError::HistoryLimit)
        ));
        let no_target = Config {
            dictionary: vec![Replacement {
                from: "tauri".into(),
                to: "  ".into(),
            }],
            ..Config::default()
        };
        assert!(matches!(
            no_target.validate(),
            Err(ConfigError::DictionaryEntry)
        ));
        let empty_snippet = Config {
            snippets: vec![Snippet {
                trigger: "o meu email".into(),
                text: String::new(),
            }],
            ..Config::default()
        };
        assert!(matches!(
            empty_snippet.validate(),
            Err(ConfigError::SnippetEntry)
        ));
        for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
            let c = Config {
                monthly_budget_usd: Some(bad),
                ..Config::default()
            };
            assert!(matches!(c.validate(), Err(ConfigError::Budget)), "{bad}");
        }
        let ok = Config {
            monthly_budget_usd: Some(10.0),
            input_device: Some("Microfone (Realtek Audio)".into()),
            ..Config::default()
        };
        assert!(ok.validate().is_ok());
        let long_device = Config {
            input_device: Some("x".repeat(500)),
            ..Config::default()
        };
        assert!(matches!(
            long_device.validate(),
            Err(ConfigError::InputDevice)
        ));
        let long_output = Config {
            notes_output_device: Some("x".repeat(500)),
            ..Config::default()
        };
        assert!(matches!(
            long_output.validate(),
            Err(ConfigError::NotesOutputDevice)
        ));
    }
}
