use std::sync::Arc;

use anyhow::Result;

use neshi_server::{Nexus, self};
use protocol::{NESHI_NATIVE_SERVER_ADDR, NESHI_WEB_SERVER_ADDR};

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::try_init()?;
    // create shared nexus
    let nexus: Arc<Nexus> = Arc::from(Nexus::default());
    // run native and web servers concurrently
    tokio::select! {
        result = neshi_server::serve_web(&nexus, NESHI_WEB_SERVER_ADDR) => result?,
        result = neshi_server::serve(&nexus, NESHI_NATIVE_SERVER_ADDR) => result?,
    };
    Ok(())
}
