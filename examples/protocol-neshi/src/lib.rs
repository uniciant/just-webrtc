pub mod metadata;
pub mod state;

pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("../file_descriptor_set.bin");

pub const NESHI_NATIVE_SERVER_ADDR: &str = "[::1]:10000";
pub const NESHI_WEB_SERVER_ADDR: &str = "[::1]:10001";