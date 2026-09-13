//! Negotiated refresh requests, coalesced while queued and while awaiting a reply.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Feature {
    Folding,
    SemanticTokens,
}

impl Feature {
    fn method(self) -> &'static str {
        match self {
            Self::Folding => "workspace/foldingRange/refresh",
            Self::SemanticTokens => "workspace/semanticTokens/refresh",
        }
    }
}

#[derive(Default)]
struct PendingRefresh {
    supported: bool,
    dirty: bool,
    inflight: Option<RequestId>,
}

#[derive(Default)]
pub(super) struct RefreshRequests {
    folding: PendingRefresh,
    semantic_tokens: PendingRefresh,
}

impl RefreshRequests {
    pub(super) fn new(policy: &tex_ls_protocol::ResponsePolicy) -> Self {
        Self {
            folding: PendingRefresh {
                supported: policy.supports("/workspace/foldingRange/refreshSupport"),
                ..Default::default()
            },
            semantic_tokens: PendingRefresh {
                supported: policy.supports("/workspace/semanticTokens/refreshSupport"),
                ..Default::default()
            },
        }
    }

    pub(super) fn invalidate(&mut self, feature: Feature) {
        let pending = match feature {
            Feature::Folding => &mut self.folding,
            Feature::SemanticTokens => &mut self.semantic_tokens,
        };
        pending.dirty |= pending.supported;
    }

    /// Called on the host's existing 100 ms tick, after worker publication.
    pub(super) fn flush(&mut self, connection: &Connection, next_id: &mut u64) {
        for (feature, pending) in [
            (Feature::Folding, &mut self.folding),
            (Feature::SemanticTokens, &mut self.semantic_tokens),
        ] {
            if !pending.dirty || pending.inflight.is_some() {
                continue;
            }
            let id = RequestId::from(format!("tex-ls-server-{next_id}"));
            *next_id += 1;
            pending.dirty = false;
            pending.inflight = Some(id.clone());
            let _ = connection.sender.send(Message::Request(Request {
                id,
                method: feature.method().into(),
                params: serde_json::Value::Null,
            }));
        }
    }

    pub(super) fn complete(&mut self, id: &RequestId) {
        for pending in [&mut self.folding, &mut self.semantic_tokens] {
            if pending.inflight.as_ref() == Some(id) {
                pending.inflight = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_are_negotiated_and_changes_during_a_request_are_not_lost() {
        let (server, client) = Connection::memory();
        let mut next_id = 1;
        let mut refreshes = RefreshRequests::default();
        refreshes.invalidate(Feature::Folding);
        refreshes.flush(&server, &mut next_id);
        assert!(client.receiver.try_recv().is_err());

        refreshes = RefreshRequests::new(&tex_ls_protocol::ResponsePolicy::new(
            &serde_json::json!({"capabilities":{"workspace":{
                "foldingRange":{"refreshSupport":true}
            }}}),
        ));
        for _ in 0..3 {
            refreshes.invalidate(Feature::Folding);
            refreshes.invalidate(Feature::SemanticTokens);
        }
        refreshes.flush(&server, &mut next_id);
        let Message::Request(first) = client.receiver.try_recv().unwrap() else {
            panic!("refresh request");
        };
        assert_eq!(first.method, Feature::Folding.method());
        assert!(client.receiver.try_recv().is_err());
        refreshes.invalidate(Feature::Folding);
        refreshes.flush(&server, &mut next_id);
        assert!(client.receiver.try_recv().is_err());
        refreshes.complete(&first.id);
        refreshes.flush(&server, &mut next_id);
        assert!(matches!(
            client.receiver.try_recv(),
            Ok(Message::Request(_))
        ));
        assert!(client.receiver.try_recv().is_err());
    }
}
