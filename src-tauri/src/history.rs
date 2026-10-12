//! Histórico opcional dos últimos ditados (com o texto), só em disco local
//! (`app_data_dir/history.json`). Desligado por omissão.
//!
//! Opcionalmente (`Config::keep_audio`), também o áudio dos últimos
//! [`AUDIO_KEEP`] ditados, em `app_data_dir/audio/<id>.wav`, para os poder
//! "Reprocessar" com outro modelo ou outro nível de limpeza.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::Style;

/// Quantos áudios se guardam, no máximo (os dos ditados mais recentes).
pub const AUDIO_KEEP: usize = 20;

/// Tamanho máximo de um identificador. Os nossos têm ~20 caracteres.
const MAX_ID_LEN: usize = 40;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Identifica a entrada (apagar, reprocessar) e dá o nome ao ficheiro de
    /// áudio. Entradas antigas não o têm: recebem um, derivado de `at`, ao ler.
    #[serde(default)]
    pub id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// O texto colado.
    pub text: String,
    /// O texto antes da limpeza com LLM, se a limpeza o alterou.
    pub raw_text: Option<String>,
    pub model: String,
    pub audio_ms: u64,
    pub total_ms: Option<u64>,
    /// `false` se o texto não foi colado (a janela em foco mudou ou a colagem
    /// falhou) e ficou só no clipboard. Entradas antigas, sem o campo, foram coladas.
    #[serde(default = "pasted_by_default")]
    pub pasted: bool,
    /// A app onde o ditado começou ("whatsapp.exe"), se se soube qual era.
    #[serde(default)]
    pub app: Option<String>,
    /// O estilo aplicado, só quando mudou alguma coisa (não é o Predefinido e a
    /// limpeza está ligada). Entradas antigas não o têm.
    #[serde(default)]
    pub style: Option<Style>,
    /// Modo comando: a instrução falada ("torna isto mais formal"). Nesse caso
    /// `raw_text` é o texto selecionado que foi substituído (se havia). `None`
    /// num ditado e nas entradas antigas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// O estilo que o ditado usou, sempre que se soube (também o Predefinido).
    /// "Reprocessar" usa-o; sem ele (entradas antigas), o da app de hoje.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_used: Option<Style>,
}

fn pasted_by_default() -> bool {
    true
}

/// O que a UI recebe: a entrada e se ainda há áudio para a reprocessar.
/// `flatten` põe os campos da entrada ao mesmo nível do `has_audio` no JSON, por
/// isso para o TypeScript é um objeto só. O `has_audio` não vai para o disco: é
/// calculado ao listar (o ficheiro pode ter sido apagado entretanto).
#[derive(Debug, Clone, Serialize)]
pub struct HistoryItem {
    #[serde(flatten)]
    pub entry: HistoryEntry,
    pub has_audio: bool,
}

/// O identificador de uma entrada criada em `at`: os nanossegundos desde 1970.
/// Só dígitos (e um "-" se fosse antes de 1970), por isso serve de nome de ficheiro.
pub fn new_id(at: OffsetDateTime) -> String {
    at.unix_timestamp_nanos().to_string()
}

/// Um identificador vindo da UI só pode ter letras e algarismos ASCII, "-" e "_".
/// Sem isto, um id como "../config" faria o `audio/<id>.wav` apontar para fora da
/// pasta (path traversal). Não há ".", "/", "\\" nem ":" possíveis.
pub fn is_valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        && !is_windows_device(id)
}

/// Nomes que o Windows reserva para dispositivos ("CON.wav" não é um ficheiro).
/// Não distingue maiúsculas.
fn is_windows_device(id: &str) -> bool {
    let upper = id.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let b = upper.as_bytes();
    b.len() == 4 && (&b[..3] == b"COM" || &b[..3] == b"LPT") && (b'1'..=b'9').contains(&b[3])
}

/// Corta as entradas mais antigas até caber em `limit`, mas nunca a `keep` (a que
/// acabou de ser reposta, se já era a mais antiga).
fn cut_oldest(entries: &mut Vec<HistoryEntry>, limit: usize, keep: &str) {
    let mut excess = entries.len().saturating_sub(limit);
    let mut i = 0;
    while excess > 0 && i < entries.len() {
        if entries[i].id == keep {
            i += 1;
        } else {
            entries.remove(i);
            excess -= 1;
        }
    }
}

/// Dá um id às entradas que não o têm e garante que não há dois iguais (o
/// repetido, que é o mais recente, ganha um sufixo "-2", "-3"…).
fn fill_ids(entries: &mut [HistoryEntry]) {
    let mut seen = HashSet::new();
    for e in entries.iter_mut() {
        if !is_valid_id(&e.id) {
            e.id = new_id(e.at);
        }
        let base = e.id.clone();
        let mut n = 2;
        // `insert` devolve `false` se o id já lá estava.
        while !seen.insert(e.id.clone()) {
            e.id = format!("{base}-{n}");
            n += 1;
        }
    }
}

pub struct HistoryStore {
    path: PathBuf,
    audio_dir: PathBuf,
    /// Cada alteração lê o ficheiro, muda-o e escreve-o. Sem este `Mutex`, um
    /// ditado a acabar e um "Apagar" na janela ao mesmo tempo podiam perder-se
    /// um ao outro. `()`: não protege dados, só a ordem das operações.
    write_lock: Mutex<()>,
    /// Áudios de entradas apagadas que ainda podem voltar com "Desfazer": a
    /// rotação não lhes toca até `drop_pending_audio` (ou até serem repostas).
    /// Guarda o número de cada apagamento: um temporizador antigo não pode
    /// apagar o áudio de um "Desfazer" posterior do mesmo ditado.
    pending: Mutex<HashMap<String, u64>>,
    next_token: AtomicU64,
}

impl HistoryStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            path: data_dir.join("history.json"),
            audio_dir: data_dir.join("audio"),
            write_lock: Mutex::new(()),
            pending: Mutex::new(HashMap::new()),
            next_token: AtomicU64::new(1),
        }
    }

    /// Um `Mutex` fica "envenenado" se uma thread entrar em pânico com ele na
    /// mão. Aqui os dados continuam válidos, por isso aproveita-se na mesma.
    fn lock(&self) -> MutexGuard<'_, ()> {
        self.write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn pending(&self) -> MutexGuard<'_, HashMap<String, u64>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mais recente primeiro.
    pub fn list(&self) -> Vec<HistoryEntry> {
        let mut entries = self.read();
        entries.reverse();
        entries
    }

    /// Para a UI: mais recente primeiro, com a indicação de áudio guardado.
    pub fn list_items(&self) -> Vec<HistoryItem> {
        let audio = self.audio_ids();
        self.list()
            .into_iter()
            .map(|entry| HistoryItem {
                has_audio: audio.contains(&entry.id),
                entry,
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<HistoryEntry> {
        self.read().into_iter().find(|e| e.id == id)
    }

    /// Bloqueante. Acrescenta e corta para os últimos `limit` ditados. Devolve
    /// o id com que a entrada ficou (é o nome do ficheiro de áudio).
    pub fn push(&self, mut entry: HistoryEntry, limit: usize) -> std::io::Result<String> {
        let _guard = self.lock();
        let mut entries = self.read();
        // O id é dado aqui (e não por quem cria a entrada) para ser único.
        entry.id.clear();
        entries.push(entry);
        fill_ids(&mut entries);
        let id = entries.last().map(|e| e.id.clone()).unwrap_or_default();
        let excess = entries.len().saturating_sub(limit);
        entries.drain(..excess);
        self.write(&entries)?;
        Ok(id)
    }

    /// Tira uma entrada e devolve-a (para o "Desfazer") e o número deste
    /// apagamento, que `drop_pending_audio` precisa. O áudio fica até lá.
    pub fn remove(&self, id: &str) -> std::io::Result<Option<(HistoryEntry, u64)>> {
        let _guard = self.lock();
        let mut entries = self.read();
        let Some(pos) = entries.iter().position(|e| e.id == id) else {
            return Ok(None);
        };
        let removed = entries.remove(pos);
        self.write(&entries)?;
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        self.pending().insert(removed.id.clone(), token);
        Ok(Some((removed, token)))
    }

    /// "Desfazer": repõe uma entrada no lugar certo (pela data). Se já lá
    /// estiver (dois cliques), não faz nada.
    pub fn restore(&self, entry: HistoryEntry, limit: usize) -> std::io::Result<()> {
        let _guard = self.lock();
        self.pending().remove(&entry.id);
        let mut entries = self.read();
        if entries.iter().any(|e| e.id == entry.id) {
            return Ok(());
        }
        // A lista no disco está da mais antiga para a mais recente.
        let pos = entries
            .iter()
            .position(|e| e.at > entry.at)
            .unwrap_or(entries.len());
        let id = entry.id.clone();
        entries.insert(pos, entry);
        cut_oldest(&mut entries, limit, &id);
        self.write(&entries)
    }

    /// Muda uma entrada no sítio (ex.: o texto depois de "Reprocessar"). Devolve
    /// a entrada já mudada, ou `None` se entretanto foi apagada.
    pub fn update(
        &self,
        id: &str,
        change: impl FnOnce(&mut HistoryEntry),
    ) -> std::io::Result<Option<HistoryEntry>> {
        let _guard = self.lock();
        let mut entries = self.read();
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
            return Ok(None);
        };
        change(entry);
        let updated = entry.clone();
        self.write(&entries)?;
        Ok(Some(updated))
    }

    /// Apaga o histórico e todo o áudio guardado.
    pub fn clear(&self) -> std::io::Result<()> {
        let _guard = self.lock();
        remove_if_exists(&self.path)?;
        self.clear_audio_locked()
    }

    fn read(&self) -> Vec<HistoryEntry> {
        let mut entries: Vec<HistoryEntry> = std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        fill_ids(&mut entries);
        entries
    }

    fn write(&self, entries: &[HistoryEntry]) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(entries).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(tmp, &self.path)
    }

    // ------------------------------------------------------------ áudio

    /// O ficheiro de áudio de uma entrada. `None` se o id não for válido: é a
    /// única forma de chegar a um caminho, por isso nada fora de `audio/`.
    fn audio_path(&self, id: &str) -> Option<PathBuf> {
        is_valid_id(id).then(|| self.audio_dir.join(format!("{id}.wav")))
    }

    /// Bloqueante. Guarda o WAV de um ditado e apaga os mais antigos, se `keep`
    /// ainda disser que o áudio se guarda. O teste faz-se com o `write_lock`
    /// trancado, o mesmo que `clear_audio`: desligar a opção não deixa um WAV
    /// para trás. Devolve se o guardou.
    pub fn save_audio_if(
        &self,
        id: &str,
        wav: &[u8],
        keep: impl FnOnce() -> bool,
    ) -> std::io::Result<bool> {
        let _guard = self.lock();
        if !keep() {
            return Ok(false);
        }
        let path = self
            .audio_path(id)
            .ok_or_else(|| std::io::Error::other("identificador inválido"))?;
        std::fs::create_dir_all(&self.audio_dir)?;
        let tmp = path.with_extension("wav.tmp");
        std::fs::write(&tmp, wav)?;
        std::fs::rename(tmp, &path)?;
        self.prune_audio_locked(AUDIO_KEEP)?;
        Ok(true)
    }

    pub fn read_audio(&self, id: &str) -> std::io::Result<Vec<u8>> {
        let path = self
            .audio_path(id)
            .ok_or_else(|| std::io::Error::other("identificador inválido"))?;
        std::fs::read(path)
    }

    /// Os ids que têm áudio guardado.
    fn audio_ids(&self) -> HashSet<String> {
        let Ok(dir) = std::fs::read_dir(&self.audio_dir) else {
            return HashSet::new();
        };
        dir.filter_map(Result::ok)
            .filter_map(|f| {
                let path = f.path();
                let is_wav = path.extension().is_some_and(|e| e == "wav");
                let stem = path.file_stem()?.to_str()?.to_owned();
                (is_wav && is_valid_id(&stem)).then_some(stem)
            })
            .collect()
    }

    /// Fica com o áudio dos `keep` ditados mais recentes (dos que têm áudio) e
    /// apaga o resto: os mais antigos e os de entradas que já não existem. Os de
    /// entradas acabadas de apagar (à espera do "Desfazer") ficam. Devolve
    /// quantos ficheiros apagou.
    pub fn prune_audio(&self, keep: usize) -> std::io::Result<usize> {
        let _guard = self.lock();
        self.prune_audio_locked(keep)
    }

    /// Como `prune_audio`, com o `write_lock` já trancado por quem chama.
    fn prune_audio_locked(&self, keep: usize) -> std::io::Result<usize> {
        let have = self.audio_ids();
        if have.is_empty() {
            return Ok(0);
        }
        let mut kept: HashSet<String> = self
            .read()
            .into_iter()
            .rev()
            .map(|e| e.id)
            .filter(|id| have.contains(id))
            .take(keep)
            .collect();
        kept.extend(self.pending().keys().cloned());
        let mut removed = 0;
        for id in have.difference(&kept) {
            if let Some(path) = self.audio_path(id) {
                remove_if_exists(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// O "Desfazer" já não vai chegar: apaga o áudio da entrada `id`, se ainda for
    /// este apagamento (`token`). Se foi reposta, saiu de `pending` e fica; se foi
    /// apagada outra vez, tem outro token e fica também.
    pub fn drop_pending_audio(&self, id: &str, token: u64) -> std::io::Result<()> {
        let _guard = self.lock();
        {
            let mut pending = self.pending();
            if pending.get(id) != Some(&token) {
                return Ok(());
            }
            pending.remove(id);
        }
        match self.audio_path(id) {
            Some(path) => remove_if_exists(&path),
            None => Ok(()),
        }
    }

    /// Apaga todo o áudio guardado (a opção foi desligada, ou o histórico apagado).
    pub fn clear_audio(&self) -> std::io::Result<()> {
        let _guard = self.lock();
        self.clear_audio_locked()
    }

    fn clear_audio_locked(&self) -> std::io::Result<()> {
        self.pending().clear();
        match std::fs::remove_dir_all(&self.audio_dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Apagar o que já não existe não é erro.
fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    fn entry(text: &str) -> HistoryEntry {
        HistoryEntry {
            id: String::new(),
            at: datetime!(2026-10-11 12:00 UTC),
            text: text.into(),
            raw_text: None,
            model: "m/m".into(),
            audio_ms: 1000,
            total_ms: Some(800),
            pasted: true,
            app: None,
            style: None,
            instruction: None,
            style_used: None,
        }
    }

    /// Uma pasta só para o teste (os testes correm em paralelo).
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ditok-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn keeps_only_the_latest_entries_newest_first() {
        let dir = temp_dir("latest");
        let store = HistoryStore::new(dir.clone());
        for t in ["um", "dois", "três"] {
            store.push(entry(t), 2).unwrap();
        }
        let texts: Vec<String> = store.list().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["três", "dois"]);
        store.clear().unwrap();
        assert!(store.list().is_empty());
        store.clear().unwrap(); // apagar duas vezes não é erro
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_entries_without_the_new_fields_still_load() {
        let json = r#"{"at":"2026-10-11T12:00:00Z","text":"olá","raw_text":null,"model":"m","audio_ms":1,"total_ms":null}"#;
        let e: HistoryEntry = serde_json::from_str(json).unwrap();
        assert!(e.pasted);
        assert_eq!((e.app, e.style), (None, None), "sem app nem estilo");
        assert_eq!(e.instruction, None, "um ditado, não um comando");
        assert_eq!(e.id, "", "sem id no ficheiro…");

        // …mas ao ler o ficheiro todas as entradas ficam com um id válido e único.
        let dir = temp_dir("old");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("history.json"), format!("[{json},{json}]")).unwrap();
        let store = HistoryStore::new(dir.clone());
        let ids: Vec<String> = store.list().into_iter().map(|e| e.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.iter().all(|id| is_valid_id(id)), "{ids:?}");
        assert_ne!(ids[0], ids[1], "a mesma hora não dá o mesmo id");
        // E continuam os mesmos de uma leitura para a outra (o Apagar precisa disso).
        let again: Vec<String> = store.list().into_iter().map(|e| e.id).collect();
        assert_eq!(ids, again);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ids_are_validated() {
        for ok in ["1760184000000000000", "-5", "abc_DEF-123", "COM10", "CONSOLE"] {
            assert!(is_valid_id(ok), "{ok}");
        }
        for bad in [
            "",
            "..",
            "../config",
            "..\\config",
            "a/b",
            "a\\b",
            "C:x",
            "x.wav",
            "olá",
            " 1",
            "CON",
            "nul",
            "Lpt9",
            "com1",
            &"1".repeat(MAX_ID_LEN + 1),
        ] {
            assert!(!is_valid_id(bad), "{bad:?}");
        }
        let store = HistoryStore::new(temp_dir("ids"));
        assert!(store.audio_path("../../etc/passwd").is_none());
        assert!(store.read_audio("../history").is_err());
        assert!(store.save_audio_if("a/b", b"x", || true).is_err());
    }

    #[test]
    fn keeps_the_audio_of_the_latest_dictations_only() {
        let dir = temp_dir("audio");
        let store = HistoryStore::new(dir.clone());
        let mut ids = Vec::new();
        for i in 0..AUDIO_KEEP + 5 {
            let mut e = entry(&format!("ditado {i}"));
            e.at += time::Duration::seconds(i as i64);
            let id = store.push(e, 100).unwrap();
            assert!(store.save_audio_if(&id, b"RIFF", || true).unwrap());
            ids.push(id);
        }
        let have = store.audio_ids();
        assert_eq!(have.len(), AUDIO_KEEP);
        // Os 5 primeiros (os mais antigos) foram-se; os últimos 20 ficaram.
        assert!(ids[..5].iter().all(|id| !have.contains(id)));
        assert!(ids[5..].iter().all(|id| have.contains(id)));
        let items = store.list_items();
        assert!(items[0].has_audio && !items.last().unwrap().has_audio);

        // Um apagado à espera do "Desfazer" fica até se desistir dele.
        let newest = ids.last().unwrap().clone();
        let (removed, token) = store.remove(&newest).unwrap().unwrap();
        store.prune_audio(AUDIO_KEEP).unwrap();
        assert!(store.audio_ids().contains(&newest));
        store.restore(removed, 100).unwrap();
        store.drop_pending_audio(&newest, token).unwrap();
        assert!(store.audio_ids().contains(&newest), "foi reposto: fica");
        assert_eq!(store.list()[0].id, newest, "reposto no mesmo sítio");

        let (_, token) = store.remove(&newest).unwrap().unwrap();
        store.drop_pending_audio(&newest, token).unwrap();
        assert!(!store.audio_ids().contains(&newest));

        // Apagar o histórico leva o áudio todo.
        store.clear().unwrap();
        assert!(store.audio_ids().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_old_undo_timer_does_not_drop_a_newer_deletion() {
        let dir = temp_dir("token");
        let store = HistoryStore::new(dir.clone());
        let id = store.push(entry("um"), 10).unwrap();
        store.save_audio_if(&id, b"RIFF", || true).unwrap();
        // Apagar, desfazer e apagar outra vez: o primeiro temporizador chega depois.
        let (first, t1) = store.remove(&id).unwrap().unwrap();
        store.restore(first, 10).unwrap();
        let (_, t2) = store.remove(&id).unwrap().unwrap();
        store.drop_pending_audio(&id, t1).unwrap();
        assert!(store.audio_ids().contains(&id), "o Desfazer ainda é válido");
        store.drop_pending_audio(&id, t2).unwrap();
        assert!(!store.audio_ids().contains(&id));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn audio_is_not_saved_when_the_option_is_off() {
        let dir = temp_dir("keep-off");
        let store = HistoryStore::new(dir.clone());
        let id = store.push(entry("um"), 10).unwrap();
        assert!(!store.save_audio_if(&id, b"RIFF", || false).unwrap());
        assert!(store.audio_ids().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn restoring_the_oldest_entry_at_the_limit_keeps_it() {
        let dir = temp_dir("restore-limit");
        let store = HistoryStore::new(dir.clone());
        let mut old = entry("antigo");
        old.at -= time::Duration::hours(1);
        let old_id = store.push(old, 10).unwrap();
        store.push(entry("novo"), 10).unwrap();
        let (removed, _) = store.remove(&old_id).unwrap().unwrap();
        store.push(entry("mais novo"), 2).unwrap();
        // Lista cheia (2): repor a mais antiga corta a "novo", não a reposta.
        store.restore(removed, 2).unwrap();
        let texts: Vec<String> = store.list().into_iter().map(|e| e.text).collect();
        assert_eq!(texts, ["mais novo", "antigo"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn update_changes_one_entry_in_place() {
        let dir = temp_dir("update");
        let store = HistoryStore::new(dir.clone());
        let id = store.push(entry("antes"), 10).unwrap();
        store.push(entry("outro"), 10).unwrap();
        let updated = store.update(&id, |e| e.text = "depois".into()).unwrap();
        assert_eq!(updated.map(|e| e.text).as_deref(), Some("depois"));
        assert_eq!(store.get(&id).unwrap().text, "depois");
        assert!(store.update("nada", |_| {}).unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
