//! Platform dependant WebRTC implementations

/// Platform marker
pub trait Platform {}

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::{PeerConnection, Channel, Error};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{PeerConnection, Channel, Error};