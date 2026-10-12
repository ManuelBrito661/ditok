//! Um valor caro que se carrega quando é preciso e se larga quando ninguém o usa há
//! algum tempo: o modelo do motor local (centenas de MB de RAM).
//!
//! Não há um relógio a "acordar" de tempos a tempos: depois de cada uso, quem usou
//! agenda UMA verificação para daqui a `N` minutos (`unload_if_idle`). Se entretanto
//! houve outro uso, o número da geração mudou e a verificação não faz nada; se o
//! valor ainda está a ser usado (alguém tem um `Arc` seu), também não.
//!
//! É genérico no tipo guardado para se testar sem modelos nem sherpa-onnx.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

struct State<T> {
    value: Option<Arc<T>>,
    /// Sobe a cada uso. Uma verificação agendada só descarrega se a geração que viu
    /// ao ser agendada continua a ser a atual (ninguém usou entretanto).
    generation: u64,
}

pub struct IdleSlot<T> {
    state: Mutex<State<T>>,
}

impl<T> Default for IdleSlot<T> {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                value: None,
                generation: 0,
            }),
        }
    }
}

impl<T> IdleSlot<T> {
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// O valor carregado, ou carregado agora com `load` (a trancar o slot: quem chegar
    /// ao mesmo tempo espera, em vez de carregar duas vezes). `keep` diz se o valor
    /// que lá está serve (ex.: é o mesmo modelo); se não, é trocado.
    pub fn acquire<E>(
        &self,
        keep: impl FnOnce(&T) -> bool,
        load: impl FnOnce() -> Result<T, E>,
    ) -> Result<Arc<T>, E> {
        let mut state = self.lock();
        state.generation += 1;
        if let Some(value) = &state.value
            && keep(value)
        {
            return Ok(Arc::clone(value));
        }
        // Larga o antigo antes de carregar o novo: nunca dois modelos em memória.
        state.value = None;
        let value = Arc::new(load()?);
        state.value = Some(Arc::clone(&value));
        Ok(value)
    }

    /// Chamado quando acaba um uso: marca o instante e devolve a geração a passar a
    /// `unload_if_idle` daqui a `N` minutos.
    pub fn finish_use(&self) -> u64 {
        let mut state = self.lock();
        state.generation += 1;
        state.generation
    }

    /// Larga o valor se ninguém o usou desde `generation` e ninguém o tem agora.
    /// Devolve se o largou.
    pub fn unload_if_idle(&self, generation: u64) -> bool {
        let mut state = self.lock();
        if state.generation != generation {
            return false;
        }
        // `strong_count == 1`: só o slot tem o valor; ninguém está a descodificar.
        match &state.value {
            Some(value) if Arc::strong_count(value) == 1 => {
                state.value = None;
                true
            }
            _ => false,
        }
    }

    /// Larga já o valor (se ainda há quem o use, esse `Arc` mantém-no vivo até acabar).
    pub fn unload(&self) {
        let mut state = self.lock();
        state.generation += 1;
        state.value = None;
    }

    pub fn is_loaded(&self) -> bool {
        self.lock().value.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Conta quantos valores existem (o `Drop` desconta): como a RAM do modelo.
    struct Model {
        id: u32,
        alive: Arc<AtomicU32>,
    }

    impl Model {
        fn new(id: u32, alive: &Arc<AtomicU32>) -> Self {
            alive.fetch_add(1, Ordering::Relaxed);
            Self {
                id,
                alive: Arc::clone(alive),
            }
        }
    }

    impl Drop for Model {
        fn drop(&mut self) {
            self.alive.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn loads_once_and_reuses() {
        let alive = Arc::new(AtomicU32::new(0));
        let slot = IdleSlot::default();
        let loads = AtomicU32::new(0);
        for _ in 0..3 {
            let m = slot
                .acquire(
                    |m: &Model| m.id == 1,
                    || {
                        loads.fetch_add(1, Ordering::Relaxed);
                        Ok::<_, ()>(Model::new(1, &alive))
                    },
                )
                .unwrap();
            assert_eq!(m.id, 1);
        }
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert_eq!(alive.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_different_model_replaces_the_old_one_without_two_in_memory() {
        let alive = Arc::new(AtomicU32::new(0));
        let slot = IdleSlot::default();
        let first = slot
            .acquire(|m: &Model| m.id == 1, || Ok::<_, ()>(Model::new(1, &alive)))
            .unwrap();
        drop(first);
        let peak = AtomicU32::new(0);
        let second = slot
            .acquire(
                |m: &Model| m.id == 2,
                || {
                    // Quando o novo carrega, o antigo já foi largado.
                    peak.store(alive.load(Ordering::Relaxed), Ordering::Relaxed);
                    Ok::<_, ()>(Model::new(2, &alive))
                },
            )
            .unwrap();
        assert_eq!(second.id, 2);
        assert_eq!(peak.load(Ordering::Relaxed), 0);
        assert_eq!(alive.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_failed_load_leaves_the_slot_empty() {
        let slot: IdleSlot<u32> = IdleSlot::default();
        let err = slot.acquire(|_| true, || Err::<u32, _>("não carregou"));
        assert_eq!(err.unwrap_err(), "não carregou");
        assert!(!slot.is_loaded());
    }

    #[test]
    fn unload_when_idle_and_no_users() {
        let alive = Arc::new(AtomicU32::new(0));
        let slot = IdleSlot::default();
        let m = slot
            .acquire(|_: &Model| true, || Ok::<_, ()>(Model::new(1, &alive)))
            .unwrap();
        drop(m);
        let generation = slot.finish_use();
        assert!(slot.unload_if_idle(generation));
        assert!(!slot.is_loaded());
        assert_eq!(
            alive.load(Ordering::Relaxed),
            0,
            "a memória foi mesmo largada"
        );
    }

    #[test]
    fn no_unload_while_in_use() {
        let alive = Arc::new(AtomicU32::new(0));
        let slot = IdleSlot::default();
        let in_use = slot
            .acquire(|_: &Model| true, || Ok::<_, ()>(Model::new(1, &alive)))
            .unwrap();
        let generation = slot.finish_use();
        // Alguém ainda tem o modelo na mão (a descodificar): não se larga.
        assert!(!slot.unload_if_idle(generation));
        assert!(slot.is_loaded());
        drop(in_use);
        assert!(
            slot.unload_if_idle(generation),
            "depois de acabar, já se larga"
        );
    }

    #[test]
    fn a_newer_use_cancels_an_older_scheduled_unload() {
        let alive = Arc::new(AtomicU32::new(0));
        let slot = IdleSlot::default();
        let m = slot
            .acquire(|_: &Model| true, || Ok::<_, ()>(Model::new(1, &alive)))
            .unwrap();
        drop(m);
        let old = slot.finish_use(); // verificação agendada para daqui a N minutos
        // Antes de ela correr, volta a haver um ditado.
        let m = slot
            .acquire(|_: &Model| true, || Ok::<_, ()>(Model::new(1, &alive)))
            .unwrap();
        drop(m);
        let new = slot.finish_use();
        assert!(
            !slot.unload_if_idle(old),
            "a verificação velha não faz nada"
        );
        assert!(slot.is_loaded());
        assert!(slot.unload_if_idle(new));
    }

    #[test]
    fn explicit_unload_keeps_a_running_decode_alive() {
        let alive = Arc::new(AtomicU32::new(0));
        let slot = IdleSlot::default();
        let running = slot
            .acquire(|_: &Model| true, || Ok::<_, ()>(Model::new(1, &alive)))
            .unwrap();
        slot.unload();
        assert!(!slot.is_loaded());
        assert_eq!(
            alive.load(Ordering::Relaxed),
            1,
            "o `Arc` de quem usa mantém-no"
        );
        drop(running);
        assert_eq!(alive.load(Ordering::Relaxed), 0);
    }
}
