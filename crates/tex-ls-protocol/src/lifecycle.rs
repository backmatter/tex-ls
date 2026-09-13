//! Transport-independent session states and exactly-once request completion.
use std::{collections::HashSet, hash::Hash};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Lifecycle {
    #[default]
    Uninitialized,
    Running,
    ShuttingDown,
    Exited,
}
impl Lifecycle {
    pub fn initialize(&mut self) -> Result<(), &'static str> {
        if *self != Self::Uninitialized {
            return Err("Session is already initialized");
        }
        *self = Self::Running;
        Ok(())
    }
    pub fn require_running(self) -> Result<(), &'static str> {
        if self == Self::Running {
            Ok(())
        } else {
            Err("Session is not running")
        }
    }
    pub fn shutdown(&mut self) -> Result<(), &'static str> {
        self.require_running()?;
        *self = Self::ShuttingDown;
        Ok(())
    }
    pub fn exit(&mut self) {
        *self = Self::Exited;
    }
}

/// Only unfinished requests are retained. Completion and cancellation consume
/// the same entry; a late worker result cannot produce a second response.
pub struct Requests<I> {
    pending: HashSet<I>,
}
impl<I> Default for Requests<I> {
    fn default() -> Self {
        Self {
            pending: HashSet::new(),
        }
    }
}
impl<I: Eq + Hash> Requests<I> {
    pub fn contains(&self, id: &I) -> bool {
        self.pending.contains(id)
    }
    pub fn receive(&mut self, id: I) -> bool {
        self.pending.insert(id)
    }
    pub fn complete(&mut self, id: &I) -> bool {
        self.pending.remove(id)
    }
    pub fn drain(&mut self) -> impl Iterator<Item = I> + '_ {
        self.pending.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_completion_and_shutdown_each_consume_requests_once() {
        let mut requests = Requests::default();
        assert!(requests.receive(1));
        assert!(!requests.receive(1));
        assert!(requests.complete(&1));
        assert!(!requests.complete(&1));
        requests.receive(2);
        assert_eq!(requests.drain().collect::<Vec<_>>(), vec![2]);
        assert!(!requests.complete(&2));
    }
    #[test]
    fn lifecycle_requires_initialize_and_does_not_restart_an_exited_session() {
        let mut state = Lifecycle::default();
        assert!(state.require_running().is_err());
        state.initialize().unwrap();
        assert!(state.initialize().is_err());
        state.shutdown().unwrap();
        assert!(state.require_running().is_err());
        state.exit();
        assert!(state.initialize().is_err());
    }
}
