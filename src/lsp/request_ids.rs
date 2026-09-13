//! Map transport IDs to per-request identities so a late result cannot complete
//! a newer request that reused the same wire ID after cancellation.
use super::*;
#[derive(Default)]
pub(super) struct RequestIds {
    next: u64,
    cancellation: HashMap<RequestId, Arc<std::sync::atomic::AtomicBool>>,
    requests: tex_ls_protocol::lifecycle::Requests<RequestId>,
    original: HashMap<RequestId, RequestId>,
    current: HashMap<RequestId, RequestId>,
}
impl RequestIds {
    pub fn receive(&mut self, original: RequestId) -> Option<RequestId> {
        if self.current.contains_key(&original) {
            return None;
        }
        self.next += 1;
        let internal = RequestId::from(format!("tex-ls-request-{}", self.next));
        self.requests.receive(internal.clone());
        self.cancellation.insert(
            internal.clone(),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        self.original.insert(internal.clone(), original.clone());
        self.current.insert(original, internal.clone());
        Some(internal)
    }
    pub fn cancellation(&self, internal: &RequestId) -> Arc<std::sync::atomic::AtomicBool> {
        self.cancellation
            .get(internal)
            .cloned()
            .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }
    pub fn contains(&self, internal: &RequestId) -> bool {
        self.requests.contains(internal)
    }
    pub fn complete(&mut self, internal: &RequestId) -> Option<RequestId> {
        if !self.requests.complete(internal) {
            return None;
        }
        if let Some(flag) = self.cancellation.remove(internal) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let original = self.original.remove(internal)?;
        self.current.remove(&original);
        Some(original)
    }
    pub fn cancel(&mut self, original: &RequestId) -> Option<RequestId> {
        let internal = self.current.get(original)?.clone();
        self.complete(&internal)?;
        Some(internal)
    }
    pub fn drain(&mut self) -> Vec<RequestId> {
        for flag in self.cancellation.drain().map(|(_, flag)| flag) {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let originals = self
            .original
            .drain()
            .map(|(_, original)| original)
            .collect();
        self.current.clear();
        self.requests.drain().for_each(drop);
        originals
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reused_wire_id_rejects_the_old_cancelled_completion() {
        let mut requests = RequestIds::default();
        let wire = RequestId::from(1);
        let old = requests.receive(wire.clone()).unwrap();
        requests.cancel(&wire).unwrap();
        let new = requests.receive(wire.clone()).unwrap();
        assert!(requests.complete(&old).is_none());
        assert_eq!(requests.complete(&new), Some(wire));
    }
}
