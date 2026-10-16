//! Bounded accepted wire-token baselines shared by native and browser hosts.
//! Hosts update this store only after cancellation and response adaptation.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

const MAX_DOCUMENTS: usize = 32;
const MAX_INTEGERS: usize = 1_048_576;
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct Baseline {
    id: String,
    sequence: u64,
    data: Vec<u32>,
}

#[derive(Debug)]
pub struct SemanticTokenStore {
    session: u64,
    sequence: u64,
    integers: usize,
    documents: BTreeMap<String, Baseline>,
}
impl Default for SemanticTokenStore {
    fn default() -> Self {
        Self {
            session: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            sequence: 0,
            integers: 0,
            documents: BTreeMap::new(),
        }
    }
}
impl SemanticTokenStore {
    pub fn forget(&mut self, uri: &str) {
        if let Some(old) = self.documents.remove(uri) {
            self.integers -= old.data.len();
        }
    }
    pub fn clear(&mut self) {
        self.documents.clear();
        self.integers = 0;
    }

    /// Compare negotiated tokens, never parser or client-independent token data.
    /// Unknown IDs and evicted/oversized baselines produce a valid full response.
    pub fn response(
        &mut self,
        method: &str,
        uri: &str,
        previous: Option<&str>,
        result: &mut Value,
    ) {
        if !matches!(
            method,
            "textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta"
        ) {
            return;
        }
        let Some(data) = result
            .get("data")
            .and_then(Value::as_array)
            .and_then(|data| {
                data.iter()
                    .map(|value| value.as_u64().and_then(|value| u32::try_from(value).ok()))
                    .collect::<Option<Vec<_>>>()
            })
        else {
            return;
        };
        self.sequence += 1;
        let id = format!("{}:{}", self.session, self.sequence);
        if method.ends_with("/delta")
            && let Some(old) = self
                .documents
                .get(uri)
                .filter(|old| previous == Some(old.id.as_str()))
        {
            let prefix = old
                .data
                .iter()
                .zip(&data)
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = old.data[prefix..]
                .iter()
                .rev()
                .zip(data[prefix..].iter().rev())
                .take_while(|(a, b)| a == b)
                .count();
            let replacement = &data[prefix..data.len() - suffix];
            // An edit costs three fields as well as its replacement. A full
            // payload is cheaper when most tokens changed.
            if replacement.len() + 8 < data.len() || old.data == data {
                let edits = if old.data == data {
                    vec![]
                } else {
                    vec![
                        json!({"start":prefix,"deleteCount":old.data.len()-prefix-suffix,"data":replacement}),
                    ]
                };
                *result = json!({"resultId":id,"edits":edits});
            }
        }
        if let Some(object) = result.as_object_mut() {
            object.insert("resultId".into(), id.clone().into());
        }
        self.forget(uri);
        if data.len() > MAX_INTEGERS {
            return;
        }
        while self.documents.len() >= MAX_DOCUMENTS || self.integers + data.len() > MAX_INTEGERS {
            let Some(uri) = self
                .documents
                .iter()
                .min_by_key(|(_, old)| old.sequence)
                .map(|(uri, _)| uri.clone())
            else {
                break;
            };
            self.forget(&uri);
        }
        self.integers += data.len();
        self.documents.insert(
            uri.into(),
            Baseline {
                id,
                sequence: self.sequence,
                data,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delta_reconstructs_negotiated_data_and_rejects_foreign_ids() {
        let mut store = SemanticTokenStore::default();
        let old: Vec<u32> = (0..100).collect();
        let mut full = json!({"data":old});
        store.response(
            "textDocument/semanticTokens/full",
            "file:a",
            None,
            &mut full,
        );
        let id = full["resultId"].as_str().unwrap().to_owned();
        let mut changed = old.clone();
        changed.splice(25..30, [77, 88]);
        let mut delta = json!({"data":changed});
        store.response(
            "textDocument/semanticTokens/full/delta",
            "file:a",
            Some(&id),
            &mut delta,
        );
        let edit = &delta["edits"][0];
        let start = edit["start"].as_u64().unwrap() as usize;
        let end = start + edit["deleteCount"].as_u64().unwrap() as usize;
        let mut reconstructed = old;
        reconstructed.splice(
            start..end,
            edit["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_u64().unwrap() as u32),
        );
        assert_eq!(reconstructed, changed);
        let id = delta["resultId"].as_str().unwrap().to_owned();
        let mut same = json!({"data":changed});
        store.response(
            "textDocument/semanticTokens/full/delta",
            "file:a",
            Some(&id),
            &mut same,
        );
        assert_eq!(same["edits"], json!([]));
        let mut foreign = json!({"data":changed});
        store.response(
            "textDocument/semanticTokens/full/delta",
            "file:b",
            Some(&id),
            &mut foreign,
        );
        assert!(foreign.get("data").is_some());
        store.forget("file:a");
        let mut closed = json!({"data":changed});
        store.response(
            "textDocument/semanticTokens/full/delta",
            "file:a",
            Some(&id),
            &mut closed,
        );
        assert!(closed.get("data").is_some());
    }
    #[test]
    fn retention_is_bounded_and_eviction_falls_back_to_full() {
        let mut store = SemanticTokenStore::default();
        for i in 0..(MAX_DOCUMENTS + 2) {
            store.response(
                "textDocument/semanticTokens/full",
                &format!("file:{i}"),
                None,
                &mut json!({"data":[1,2,3,4,5]}),
            );
        }
        assert_eq!(store.documents.len(), MAX_DOCUMENTS);
        assert!(!store.documents.contains_key("file:0"));
        assert_eq!(store.integers, MAX_DOCUMENTS * 5);
        store.clear();
        assert_eq!(store.integers, 0);
    }
}

#[cfg(test)]
mod byte_retention_tests {
    use super::*;
    #[test]
    fn data_budget_evicts_baselines_and_oversized_data_is_not_retained() {
        let mut store = SemanticTokenStore::default();
        let mut first = json!({"data": vec![0u32; MAX_INTEGERS / 2 + 1]});
        store.response(
            "textDocument/semanticTokens/full",
            "file:first",
            None,
            &mut first,
        );
        let first_id = first["resultId"].as_str().unwrap().to_owned();
        let mut next = first.clone();
        store.response(
            "textDocument/semanticTokens/full",
            "file:next",
            None,
            &mut next,
        );
        assert_eq!(store.documents.len(), 1);
        assert!(!store.documents.contains_key("file:first"));
        assert!(store.integers <= MAX_INTEGERS);
        let mut fallback = json!({"data": [0,0,1,0,0]});
        store.response(
            "textDocument/semanticTokens/full/delta",
            "file:first",
            Some(&first_id),
            &mut fallback,
        );
        assert!(fallback.get("data").is_some());
        let mut oversized = json!({"data": vec![0u32; MAX_INTEGERS + 1]});
        store.response(
            "textDocument/semanticTokens/full",
            "file:next",
            None,
            &mut oversized,
        );
        assert!(!store.documents.contains_key("file:next"));
        assert!(oversized.get("data").is_some());
    }
}
