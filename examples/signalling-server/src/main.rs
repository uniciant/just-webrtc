use std::{sync::Arc, time::Duration};

use anyhow::Result;
use just_webrtc_signalling::{
    server::Signalling, DEFAULT_NATIVE_SERVER_ADDR, DEFAULT_WEB_SERVER_ADDR,
};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::try_init()?;

    // create shared mapped signalling channels
    let signalling = Arc::new(Signalling::new());
    // create server futures
    let serve_fut = just_webrtc_signalling::server::serve(
        signalling.clone(),
        DEFAULT_NATIVE_SERVER_ADDR.parse()?,
        Some(KEEPALIVE_INTERVAL),
        None,
        None,
    );
    let serve_web_fut = just_webrtc_signalling::server::serve_web(
        signalling,
        DEFAULT_WEB_SERVER_ADDR.parse()?,
        Some(KEEPALIVE_INTERVAL),
        None,
        None,
    );
    // run servers concurrently
    tokio::try_join!(serve_fut, serve_web_fut)?;

    Ok(())
}
