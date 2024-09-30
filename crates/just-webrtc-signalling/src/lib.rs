#![doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))]
#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![warn(dead_code)]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]

#[cfg(all(target_arch = "wasm32", feature = "server"))]
compile_error!("feature \"server\" is not compatible with target \"wasm32\"");

extern crate alloc;

use prost as _;

#[cfg(any(feature = "client", feature = "server"))]
pub(crate) mod pb {
    tonic::include_proto!("main_pb");
}

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

/// Default native server address
pub const DEFAULT_NATIVE_SERVER_ADDR: &str = "[::1]:10000";
/// Default web server address
pub const DEFAULT_WEB_SERVER_ADDR: &str = "[::1]:10001";
