#[cfg(not(target_arch = "wasm32"))]
#[cfg(feature = "server")]
#[tokio::main]
async fn main() {
    use std::time::Duration;
    use just_webrtc_signalling::{DEFAULT_NATIVE_SERVER_ADDR, DEFAULT_WEB_SERVER_ADDR};

    const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

    pretty_env_logger::try_init().unwrap();

    // create server futures
    let serve_fut = just_webrtc_signalling::server::serve(
        DEFAULT_NATIVE_SERVER_ADDR.parse().unwrap(),
        Some(KEEPALIVE_INTERVAL), None,
        None,
    );
    #[cfg(feature = "server-web")]
    let serve_web_fut = just_webrtc_signalling::server::serve_web(
        DEFAULT_WEB_SERVER_ADDR.parse().unwrap(),
        Some(KEEPALIVE_INTERVAL), None,
        None,
    );
    #[cfg(not(feature = "server-web"))]
    let serve_web_fut = async { Ok(()) };
    // run servers concurrently
    tokio::try_join!(serve_fut, serve_web_fut).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
#[cfg(not(feature = "server"))]
fn main() {
    println!("Must enable 'server' feature to run `just-webrtc-signalling` server!");
}

#[cfg(target_arch = "wasm32")]
fn main() {
    println!("Cannot run `just-webrtc-signalling` server on web architecture!");
}
