use std::{sync::Arc, time::Duration};

use anyhow::Result;

use neshi_server::{Nexus, self};
use protocol_neshi::{NESHI_NATIVE_SERVER_ADDR, NESHI_WEB_SERVER_ADDR};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::try_init()?;
    // create shared nexus
    let nexus: Arc<Nexus> = Arc::from(Nexus::default());
    // run native and web servers concurrently
    tokio::select! {
        result = neshi_server::serve_web(&nexus, NESHI_WEB_SERVER_ADDR, Some(KEEPALIVE_INTERVAL), None) => result?,
        result = neshi_server::serve(&nexus, NESHI_NATIVE_SERVER_ADDR, Some(KEEPALIVE_INTERVAL), None) => result?,
    };
    Ok(())
}
