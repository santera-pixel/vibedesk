//! The `proxy-provider` step: point the agent at the capability proxy.
//!
//! Three capabilities live at the proxy boundary — compaction, reading and
//! terminal — so the proxy has to be listening whenever any of them is on, and
//! the agent has to be pointed at it. `provider.rs` owns both halves; this step
//! only decides whether the machine needs it and calls the owner.

use crate::provision::Probe;

pub fn probe() -> Probe {
    let registered = crate::capabilities::provider::is_registered();
    let needed = crate::capabilities::proxy::needed();
    Probe {
        satisfied: registered == needed,
        version: None,
        detail: match (needed, registered) {
            (true, true) => "registered, and a boundary capability needs it".into(),
            (true, false) => "a boundary capability needs it but it is not registered".into(),
            (false, true) => "registered, but no boundary capability needs it".into(),
            (false, false) => "not needed, and not registered".into(),
        },
    }
}

pub async fn install() -> Result<String, String> {
    if !crate::capabilities::proxy::needed() {
        crate::capabilities::provider::disable()
            .map_err(|error| format!("could not unregister the provider: {error}"))?;
        return Ok("no boundary capability needs the proxy; unregistered".into());
    }

    let port = crate::capabilities::proxy::ensure_started()
        .await
        .map_err(|error| format!("the proxy did not start: {error}"))?;
    crate::capabilities::provider::enable(port)
        .map_err(|error| format!("could not register the provider: {error}"))?;
    Ok(format!("registered on port {port}"))
}
