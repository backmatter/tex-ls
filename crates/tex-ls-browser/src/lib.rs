//! Browser bindings for the embedded language session.

#[cfg(test)]
#[macro_use]
#[path = "../../../tests/support/paths.rs"]
mod test_paths;

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
#[derive(Default)]
pub struct LanguageSession {
    inner: session::Session,
}

#[wasm_bindgen]
impl LanguageSession {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dispatch_with_context(&mut self, method: &str, params: &str) -> Result<String, JsError> {
        let params = serde_json::from_str(params)?;
        let result = self
            .inner
            .dispatch_with_context(method, params)
            .map_err(|error| JsError::new(&error))?;
        Ok(serde_json::to_string(&result)?)
    }

    pub fn dispatch(&mut self, method: &str, params: &str) -> Result<String, JsError> {
        let params = serde_json::from_str(params)?;
        let result = self
            .inner
            .dispatch(method, params)
            .map_err(|error| JsError::new(&error))?;
        Ok(serde_json::to_string(&result)?)
    }
}

pub mod session;
pub mod settings;

mod external_inputs;

#[cfg(test)]
mod tests;

#[cfg(all(feature = "allocation-metrics", target_arch = "wasm32"))]
mod metrics;

/// Allocator counters for paired profiling runs.
#[cfg(all(feature = "allocation-metrics", target_arch = "wasm32"))]
#[wasm_bindgen]
pub fn allocation_metrics() -> String {
    serde_json::to_string(&metrics::sample()).expect("counter serialization")
}

/// Current linear-memory capacity, distinct from live allocator bytes.
#[cfg(all(feature = "allocation-metrics", target_arch = "wasm32"))]
#[wasm_bindgen]
pub fn linear_memory_bytes() -> usize {
    core::arch::wasm32::memory_size(0) * 65536
}
