//! Panic-payload helpers shared by the engine guard in `poly-core` and any component host.

/// Best-effort recovery of a panic payload's message.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&'static str>() {
        (*text).to_owned()
    } else {
        "panic payload was not a string".to_owned()
    }
}
