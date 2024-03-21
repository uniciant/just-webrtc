#[cfg(all(target_arch = "wasm32", feature = "server"))]
compile_error!("feature \"server\" is not compatible with target \"wasm32\"");

pub(crate) mod pb { tonic::include_proto!("main_pb"); }

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

pub const DEFAULT_NATIVE_SERVER_ADDR: &str = "[::1]:10000";
pub const DEFAULT_WEB_SERVER_ADDR: &str = "[::1]:10001";