//! Native language-server transport, settings, scheduling, and external integrations.

use capabilities::{client_diagnostic_support, native_capabilities};
use event_loop::main_loop;
use lsp_server::Connection;
use tex_ls_protocol::capabilities::negotiate_position_encoding;

mod forward_search;
mod services;
mod task_pool;

type DynError = Box<dyn std::error::Error + Sync + Send>;

pub fn run() -> Result<(), DynError> {
    let (connection, io_threads) = Connection::stdio();
    serve(connection)?;
    io_threads.join()?;
    Ok(())
}

pub fn serve(connection: Connection) -> Result<(), DynError> {
    let (initialize_id, init_params) = connection.initialize_start()?;
    let encoding = negotiate_position_encoding(&init_params);
    let (supports_pull_diagnostics, _) = client_diagnostic_support(&init_params);
    let capabilities = native_capabilities(encoding, supports_pull_diagnostics);
    connection.initialize_finish(
        initialize_id,
        tex_ls_protocol::ResponsePolicy::new(&init_params).initialize_result(capabilities),
    )?;
    main_loop(connection, init_params, encoding)
}

mod capabilities;
mod event_loop;
mod inverse_search;
mod messages;
mod requests;
mod responses;
mod settings;
mod state;
mod synchronization;
#[cfg(test)]
mod tests;
mod watching;
mod worker;

mod client_requests;
mod request_ids;

mod loading;
mod refresh;

mod compiler_acquisition;
mod file_acquisition;
