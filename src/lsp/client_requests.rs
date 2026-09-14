//! Acknowledgements for native host requests. Incoming request identities remain
//! owned by RequestIds, so client replies cannot complete reused wire IDs.
use super::*;

pub(super) fn handle_client_response(
    connection: &Connection,
    state: &mut GlobalState,
    job_tx: &Sender<WorkerJob>,
    method: &str,
    response: Response,
) {
    state.refreshes.complete(&response.id);
    if let Some(scopes) = state.pending_configuration.remove(&response.id) {
        match response.response_result {
            Ok(serde_json::Value::Array(values)) if values.len() == scopes.len() => {
                // Validate the entire response before replacing any scope.
                let parsed: Result<Vec<_>, _> = values
                    .iter()
                    .zip(&scopes)
                    .map(|(value, scope)| {
                        if value.is_null() {
                            Ok(EditorSettings::default())
                        } else {
                            EditorSettings::from_client_value(value).and_then(|settings| {
                                settings.with_workspace_roots(std::slice::from_ref(scope))
                            })
                        }
                    })
                    .collect();
                match parsed {
                    Ok(settings) => {
                        state
                            .scoped_editor_settings
                            .extend(scopes.into_iter().zip(settings));
                        state.invalidate_settings();
                        state.diagnostic_stamp += 1;
                        relint_all_open(connection, state, job_tx);
                    }
                    Err(error) => state.config_messages.push(error),
                }
            }
            _ => state.config_messages.push(
                "Invalid or failed workspace/configuration response; retaining settings".into(),
            ),
        }
    } else {
        if method == "client/registerCapability" {
            state.watcher_acknowledged = response
                .response_result
                .as_ref()
                .is_ok_and(serde_json::Value::is_null);
        }
        if let Err(error) = response.response_result {
            log::warn!("Client rejected {method}: {}", error.message);
        }
    }
}
