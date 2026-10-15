//! As conversas do assistente (a janela do modo comando), uma por ficheiro em
//! `app_data_dir/conversas/<id>.json`, só neste computador.
//!
//! Uma conversa por ficheiro, e não uma lista única como o histórico dos ditados: uma
//! conversa cresce a cada pergunta, e assim mexer numa não reescreve as outras. Só se
//! guardam com o histórico ligado (`Config::history_enabled`); desligado, a conversa
//! vive na memória até fechares a app (ver `Assistant`).

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    history,
    xai::{
        chat::Role,
        responses::{Search, Source},
    },
};

/// Quantas conversas se guardam, no máximo (as mais recentes). Passando disto, a
/// mais antiga é apagada ao guardar uma nova.
pub const KEEP: usize = 200;
/// Tamanho do título, que é o princípio do primeiro pedido.
const TITLE_CHARS: usize = 56;
/// Tamanho da pré-visualização na lista.
const PREVIEW_CHARS: usize = 110;

/// Como acabou uma resposta. `Done` não se escreve no ficheiro.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Done,
    /// Pediste para parar: o texto que já tinha chegado fica.
    Stopped,
    /// Falhou (rede, chave, modelo…): `Message::error` diz porquê.
    Failed,
}

impl Status {
    fn is_done(&self) -> bool {
        *self == Self::Done
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Único dentro da conversa (e entre conversas): serve à janela para saber a que
    /// mensagem pertence cada bocado de uma resposta em streaming.
    pub id: String,
    pub role: Role,
    pub text: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Só nas respostas: o modelo que a escreveu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Só nas respostas: o que custou, em USD, tal como a xAI o cobrou.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Status::is_done")]
    pub status: Status,
    /// Com `Status::Failed`: a frase que explica a falha.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// O pedido foi dito em voz alta (e não escrito).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spoken: bool,
    /// Só nas respostas: o modelo raciocina antes de responder (a janela mostra "A pensar…").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub thinks: bool,
    /// O raciocínio que a xAI mostrou (muitas vezes não mostra nenhum).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Quanto tempo pensou antes de começar a escrever, em milissegundos.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_ms: Option<u64>,
    /// Quantos tokens gastou a pensar, se a xAI o disse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// As pesquisas que o modelo fez na web e no X.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub searches: Vec<Search>,
    /// As fontes que citou.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
}

/// A captura do ecrã com que a conversa começou (ver `screen/`). A imagem NÃO se guarda em
/// disco, porque pode mostrar qualquer coisa que tenhas aberta: vive na memória enquanto a
/// conversa está aberta (`Assistant`). Aqui fica só o que a descreve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    /// Tamanho, em píxeis, da imagem que seguiu para o modelo.
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    /// Só letras e algarismos: é o nome do ficheiro (ver `history::is_valid_id`).
    pub id: String,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// O texto que estava selecionado quando a conversa começou: é sobre ele que
    /// o primeiro pedido fala. Sem seleção, a conversa começou em branco.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    /// A app onde estavas ("whatsapp.exe"), se se soube qual era.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// A conversa é sobre uma captura do ecrã.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<Capture>,
    pub messages: Vec<Message>,
}

impl Conversation {
    pub fn new(now: OffsetDateTime, selection: Option<String>, app: Option<String>) -> Self {
        Self {
            id: history::new_id(now),
            title: String::new(),
            created_at: now,
            updated_at: now,
            selection,
            app,
            capture: None,
            messages: Vec::new(),
        }
    }

    /// A conversa passa a ser sobre uma captura do ecrã.
    pub fn with_capture(mut self, capture: Capture) -> Self {
        self.capture = Some(capture);
        self
    }

    /// Acrescenta uma mensagem e devolve-a. O título vem do primeiro pedido.
    pub fn push(
        &mut self,
        role: Role,
        text: String,
        spoken: bool,
        now: OffsetDateTime,
    ) -> &mut Message {
        if self.title.is_empty() && role == Role::User {
            self.title = title_from(&text);
        }
        self.updated_at = now;
        self.messages.push(Message {
            id: history::new_id(now),
            role,
            text,
            at: now,
            model: None,
            cost_usd: None,
            status: Status::Done,
            error: None,
            spoken,
            thinks: false,
            reasoning: None,
            thinking_ms: None,
            reasoning_tokens: None,
            searches: Vec::new(),
            sources: Vec::new(),
        });
        self.messages
            .last_mut()
            .expect("acabou de ser acrescentada")
    }

    pub fn message_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.messages.iter_mut().find(|m| m.id == id)
    }

    fn summary(&self) -> Summary {
        let preview = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant && !m.text.trim().is_empty())
            .or_else(|| self.messages.first())
            .map(|m| clip(&collapse(&m.text), PREVIEW_CHARS))
            .unwrap_or_default();
        Summary {
            id: self.id.clone(),
            title: if self.title.is_empty() {
                "Conversa sem título".to_owned()
            } else {
                self.title.clone()
            },
            updated_at: self.updated_at,
            messages: self.messages.len(),
            preview,
        }
    }

    /// A conversa tem `needle` (já em minúsculas) no título, no texto selecionado ou
    /// em alguma mensagem?
    fn matches(&self, needle: &str) -> bool {
        let has = |t: &str| t.to_lowercase().contains(needle);
        has(&self.title)
            || self.selection.as_deref().is_some_and(has)
            || self.messages.iter().any(|m| has(&m.text))
    }
}

/// O que a lista de conversas mostra de cada uma.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub messages: usize,
    pub preview: String,
}

/// Espaços e mudanças de linha seguidos viram um espaço só.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Corta a `max` carateres, com reticências.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// O título de uma conversa: o início do primeiro pedido.
pub fn title_from(request: &str) -> String {
    clip(&collapse(request), TITLE_CHARS)
}

/// Uma conversa apagada: nenhuma gravação a pode repor (ver `ChatStore::save`).
const DELETED: u64 = u64::MAX;

pub struct ChatStore {
    dir: PathBuf,
    /// A versão da última gravação de cada conversa. Quem guarda corre numa thread de
    /// bloqueio, e duas gravações da mesma conversa (parar uma resposta e pedir outra)
    /// podem chegar fora de ordem: cada uma leva o número do momento em que foi
    /// pedida, e uma mais antiga do que a que já lá está é ignorada. Serve também de
    /// cadeado: guardar escreve um ficheiro e pode apagar outros, e duas gravações ao
    /// mesmo tempo (o fim de uma resposta e um "Apagar") não se podem cruzar.
    versions: Mutex<HashMap<String, u64>>,
}

impl ChatStore {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("conversas"),
            versions: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, u64>> {
        self.versions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `None` se `id` não serve de nome de ficheiro: um id vindo da janela como
    /// "../config" apontava para fora da pasta.
    fn path(&self, id: &str) -> Option<PathBuf> {
        history::is_valid_id(id).then(|| self.dir.join(format!("{id}.json")))
    }

    /// Bloqueante. Escreve a conversa (de uma vez: primeiro um ficheiro temporário,
    /// para um corte a meio nunca deixar meia conversa) e apara as mais antigas.
    /// `version` só cresce: uma gravação com uma versão que não é maior do que a da
    /// última é ignorada, e uma conversa apagada não volta.
    pub fn save(&self, conversation: &Conversation, version: u64) -> io::Result<()> {
        let path = self
            .path(&conversation.id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "id inválido"))?;
        let mut versions = self.lock();
        if versions
            .get(&conversation.id)
            .is_some_and(|&last| last >= version)
        {
            return Ok(());
        }
        fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec(conversation).map_err(io::Error::other)?,
        )?;
        fs::rename(tmp, &path)?;
        versions.insert(conversation.id.clone(), version);
        self.prune(&conversation.id);
        Ok(())
    }

    /// Apaga as conversas mais antigas (pela data do ficheiro) até caberem `KEEP`,
    /// sem nunca apagar `keep`, a que acabou de ser guardada.
    fn prune(&self, keep: &str) {
        let mut files: Vec<(SystemTime, PathBuf)> = self
            .json_files()
            .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
            .collect();
        let excess = files.len().saturating_sub(KEEP);
        if excess == 0 {
            return;
        }
        files.sort();
        let keep_name = format!("{keep}.json");
        for (_, path) in files
            .into_iter()
            .filter(|(_, p)| p.file_name().is_some_and(|n| *n != *keep_name))
            .take(excess)
        {
            if let Err(e) = fs::remove_file(&path) {
                tracing::warn!(error = %e, path = %path.display(), "não foi possível apagar uma conversa antiga");
            }
        }
    }

    fn json_files(&self) -> impl Iterator<Item = PathBuf> {
        fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
    }

    pub fn load(&self, id: &str) -> Option<Conversation> {
        let bytes = fs::read(self.path(id)?).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(conversation) => Some(conversation),
            Err(e) => {
                tracing::warn!(error = %e, id, "conversa ilegível");
                None
            }
        }
    }

    fn all(&self) -> Vec<Conversation> {
        let mut all: Vec<Conversation> = self
            .json_files()
            .filter_map(|p| {
                let bytes = fs::read(&p).ok()?;
                serde_json::from_slice(&bytes)
                    .map_err(
                        |e| tracing::warn!(error = %e, path = %p.display(), "conversa ilegível"),
                    )
                    .ok()
            })
            .collect();
        all.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
        all
    }

    /// Bloqueante. As conversas, da mais recente para a mais antiga. Com `query`,
    /// só as que têm essas letras (sem distinguir maiúsculas) no título, no texto
    /// selecionado ou em alguma mensagem.
    pub fn list(&self, query: Option<&str>) -> Vec<Summary> {
        let needle = query
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
        self.all()
            .iter()
            .filter(|c| needle.as_deref().is_none_or(|n| c.matches(n)))
            .map(Conversation::summary)
            .collect()
    }

    /// Apaga uma conversa. `false` se já não existia.
    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let Some(path) = self.path(id) else {
            return Ok(false);
        };
        let mut versions = self.lock();
        versions.insert(id.to_owned(), DELETED);
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Apaga todas as conversas.
    pub fn clear(&self) -> io::Result<()> {
        let mut versions = self.lock();
        for last in versions.values_mut() {
            *last = DELETED;
        }
        match fs::remove_dir_all(&self.dir) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ditok-conversas-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Guarda com uma versão sempre maior do que as anteriores.
    fn save(store: &ChatStore, c: &Conversation) {
        static VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let version = VERSION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        store.save(c, version).unwrap();
    }

    fn conversation(at: OffsetDateTime, request: &str, answer: &str) -> Conversation {
        let mut c = Conversation::new(at, Some("olá pedro".into()), Some("outlook.exe".into()));
        c.push(Role::User, request.into(), true, at);
        let reply = c.push(Role::Assistant, answer.into(), false, at);
        reply.model = Some("grok".into());
        reply.cost_usd = Some(0.001);
        c
    }

    #[test]
    fn the_title_is_the_start_of_the_first_request() {
        let mut c = Conversation::new(datetime!(2026-10-14 10:00 UTC), None, None);
        c.push(
            Role::User,
            "  torna isto\n\nmais   formal ".into(),
            true,
            c.created_at,
        );
        assert_eq!(c.title, "torna isto mais formal");
        // Uma resposta ou um segundo pedido não mudam o título.
        c.push(Role::Assistant, "Caro Pedro".into(), false, c.created_at);
        c.push(Role::User, "agora em inglês".into(), false, c.created_at);
        assert_eq!(c.title, "torna isto mais formal");

        let long = title_from(&"palavra ".repeat(30));
        assert!(long.chars().count() <= TITLE_CHARS + 1, "{long}");
        assert!(long.ends_with('…'));
        assert!(!long.contains(" …"), "sem espaço antes das reticências");
    }

    #[test]
    fn message_ids_are_unique() {
        let mut c = Conversation::new(datetime!(2026-10-14 10:00 UTC), None, None);
        let t0 = datetime!(2026-10-14 10:00 UTC);
        c.push(Role::User, "a".into(), false, t0);
        c.push(
            Role::Assistant,
            "b".into(),
            false,
            t0 + time::Duration::milliseconds(5),
        );
        assert_ne!(c.messages[0].id, c.messages[1].id);
        assert!(history::is_valid_id(&c.id));
    }

    #[test]
    fn a_conversation_survives_a_roundtrip() {
        let dir = tmp("roundtrip");
        let store = ChatStore::new(&dir);
        let mut c = conversation(
            datetime!(2026-10-14 10:00 UTC),
            "traduz para inglês",
            "Hello Pedro",
        );
        c.messages[1].status = Status::Stopped;
        save(&store, &c);
        assert_eq!(store.load(&c.id), Some(c.clone()));
        // `Done` e os campos vazios não ocupam o ficheiro.
        let raw = fs::read_to_string(dir.join("conversas").join(format!("{}.json", c.id))).unwrap();
        assert!(
            raw.contains("\"stopped\"") && !raw.contains("\"done\""),
            "{raw}"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_capture_is_described_on_disk_but_the_image_is_not() {
        let dir = tmp("capture");
        let store = ChatStore::new(&dir);
        let at = datetime!(2026-10-14 10:00 UTC);
        let mut c = Conversation::new(at, None, Some("code.exe".into())).with_capture(Capture {
            width: 1280,
            height: 720,
        });
        c.push(Role::User, "o que quer dizer este erro?".into(), true, at);
        save(&store, &c);
        let raw = fs::read_to_string(dir.join("conversas").join(format!("{}.json", c.id))).unwrap();
        assert!(
            raw.contains("\"capture\":{\"width\":1280,\"height\":720}"),
            "{raw}"
        );
        assert_eq!(store.load(&c.id).unwrap().capture, c.capture);
        // Uma conversa antiga, sem o campo, continua a abrir.
        let plain = conversation(at, "traduz", "Hello");
        assert_eq!(plain.capture, None);
        assert!(
            !serde_json::to_string(&plain).unwrap().contains("capture"),
            "sem captura, nada no ficheiro"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn listing_is_newest_first_and_searchable() {
        let dir = tmp("list");
        let store = ChatStore::new(&dir);
        let a = conversation(
            datetime!(2026-10-14 10:00 UTC),
            "traduz para inglês",
            "Hello Pedro",
        );
        let b = conversation(
            datetime!(2026-10-14 11:00 UTC),
            "resume em três pontos",
            "Um. Dois. Três.",
        );
        save(&store, &a);
        save(&store, &b);

        let all = store.list(None);
        assert_eq!(
            all.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            [b.id.as_str(), a.id.as_str()]
        );
        assert_eq!(all[0].title, "resume em três pontos");
        assert_eq!(
            all[0].preview, "Um. Dois. Três.",
            "a resposta, não o pedido"
        );
        assert_eq!(all[0].messages, 2);

        // Procura no pedido, na resposta e no texto selecionado; ignora maiúsculas.
        assert_eq!(store.list(Some("HELLO")).len(), 1);
        assert_eq!(store.list(Some("inglês"))[0].id, a.id);
        assert_eq!(store.list(Some("olá pedro")).len(), 2);
        assert!(store.list(Some("nada disto")).is_empty());
        assert_eq!(store.list(Some("   ")).len(), 2, "vazio não filtra");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn delete_and_clear() {
        let dir = tmp("delete");
        let store = ChatStore::new(&dir);
        let a = conversation(datetime!(2026-10-14 10:00 UTC), "um", "1");
        let b = conversation(datetime!(2026-10-14 11:00 UTC), "dois", "2");
        save(&store, &a);
        save(&store, &b);
        assert!(store.delete(&a.id).unwrap());
        assert!(!store.delete(&a.id).unwrap(), "já não existia");
        assert_eq!(store.list(None).len(), 1);
        store.clear().unwrap();
        assert!(store.list(None).is_empty());
        store.clear().unwrap(); // sem pasta, também não falha
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn ids_cannot_escape_the_folder() {
        let dir = tmp("ids");
        let store = ChatStore::new(&dir);
        for bad in ["../config", "..\\x", "a/b", "", "con"] {
            assert!(store.path(bad).is_none(), "{bad}");
            assert!(store.load(bad).is_none(), "{bad}");
            assert!(!store.delete(bad).unwrap(), "{bad}");
        }
        let mut c = conversation(datetime!(2026-10-14 10:00 UTC), "x", "y");
        c.id = "../fora".into();
        assert!(store.save(&c, 1).is_err());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_older_snapshot_never_overwrites_a_newer_one() {
        let dir = tmp("versions");
        let store = ChatStore::new(&dir);
        let mut c = conversation(datetime!(2026-10-14 10:00 UTC), "pergunta", "resposta");
        let first = c.clone();
        c.messages[1].status = Status::Stopped;
        store.save(&c, 20).unwrap();
        // A gravação mais antiga chega atrasada.
        store.save(&first, 10).unwrap();
        assert_eq!(
            store.load(&c.id).unwrap().messages[1].status,
            Status::Stopped
        );
        // A mesma versão também não repete a escrita.
        store.save(&first, 20).unwrap();
        assert_eq!(
            store.load(&c.id).unwrap().messages[1].status,
            Status::Stopped
        );
        store.save(&first, 21).unwrap();
        assert_eq!(store.load(&c.id).unwrap().messages[1].status, Status::Done);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_deleted_conversation_stays_deleted() {
        let dir = tmp("tombstone");
        let store = ChatStore::new(&dir);
        let c = conversation(datetime!(2026-10-14 10:00 UTC), "x", "y");
        store.save(&c, 1).unwrap();
        assert!(store.delete(&c.id).unwrap());
        // Uma gravação que estava a caminho quando se apagou não a repõe.
        store.save(&c, 2).unwrap();
        assert!(store.load(&c.id).is_none());

        let d = conversation(datetime!(2026-10-14 11:00 UTC), "x", "y");
        store.save(&d, 1).unwrap();
        store.clear().unwrap();
        store.save(&d, 2).unwrap();
        assert!(store.list(None).is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_files_are_skipped() {
        let dir = tmp("garbage");
        let store = ChatStore::new(&dir);
        let c = conversation(datetime!(2026-10-14 10:00 UTC), "x", "y");
        save(&store, &c);
        fs::write(dir.join("conversas").join("123.json"), b"isto nao e json").unwrap();
        assert_eq!(store.list(None).len(), 1);
        assert!(store.load("123").is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_oldest_conversations_are_pruned() {
        let dir = tmp("prune");
        let store = ChatStore::new(&dir);
        fs::create_dir_all(dir.join("conversas")).unwrap();
        // KEEP ficheiros, com datas diferentes, e mais um a chegar.
        for i in 0..KEEP {
            let path = dir.join("conversas").join(format!("{}.json", 1000 + i));
            fs::write(&path, b"{}").unwrap();
            let age = std::time::Duration::from_secs((KEEP - i) as u64 * 10);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - age - std::time::Duration::from_secs(3600))
                .unwrap();
        }
        let c = conversation(datetime!(2026-10-14 10:00 UTC), "novo", "ok");
        save(&store, &c);
        let files: Vec<_> = store.json_files().collect();
        assert_eq!(files.len(), KEEP);
        assert!(
            files.iter().any(|p| p.ends_with(format!("{}.json", c.id))),
            "a nova fica"
        );
        assert!(
            !dir.join("conversas").join("1000.json").exists(),
            "a mais antiga sai"
        );
        assert!(dir.join("conversas").join("1001.json").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
