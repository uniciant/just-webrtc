# Just WebRTC Signalling

[![crates.io](https://img.shields.io/crates/v/just-webrtc-signalling?style=flat-square&logo=rust)](https://crates.io/crates/just-webrtc-signalling)
[![documentation](https://docs.rs/just-webrtc-signalling/badge.svg)](https://docs.rs/just-webrtc-signalling)
[![license](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue?style=flat-square)](#license)
[![build status](https://img.shields.io/github/actions/workflow/status/uniciant/just-webrtc/rust.yml?branch=main&style=flat-square&logo=github)](https://github.com/uniciant/just-webrtc/actions)

Just simple, fast and easy signalling for full-mesh WebRTC connections in Rust.

Provides a TLS secure-able server and a client that is both `native` and `wasm32` compatible.

This signalling implementation is built around a [`tonic`](https://github.com/hyperium/tonic) gRPC service. gRPC is leveraged for its efficiency, security, scalability and load balancing. In the future, interoperability may also be used to signal between clients written in different languages.

`just-webrtc-signalling` is modular. Use it with [`just-webrtc`](https://crates.io/crates/just-webrtc)... or with any other WebRTC implementation you like!

```toml
[dependencies]
just-webrtc-signalling = "0.1"
```

## Documentation
See [docs.rs](https://docs.rs/just-webrtc-signalling) for the complete API reference.

## Feature flags
All features [`client`, `server` and `server_web`] are enabled by default.
* `client`: enables the signalling client (`native` and `wasm` compatible)
* `server`: enables the native signalling service (for communication with native clients)
* `server-web`: enables the web signalling service (for communication with web/wasm clients)

## Examples

**Running a client:**

The below example demonstrates how you might integrate a `just-webrtc-signalling` client with the WebRTC implementation of your choice.

NOTE: for web clients, you will need to disable the `server` and `server-web` features, as these are native only:
```toml
[dependencies]
just-webrtc-signalling = { version = "0.1", default-features = false, features = ["client"] }
```

For complete examples, using `just-webrtc` as the WebRTC implementation, see the [**Examples**](https://github.com/uniciant/just-webrtc/tree/main/examples) in the repository.

```rust
use anyhow::Result;
use futures_util::FutureExt;

use just_webrtc_signalling::client::RtcSignallingClientBuilder;

/// run all client signalling concurrently
async fn run_peer(server_address: String) -> Result<()> {
    // create signalling client
    let signalling_client = RtcSignallingClientBuilder::default().build(server_address)?;
    // start a peer
    let mut signalling_peer = signalling_client.start_peer().await?;
    // set callbacks for the new peer
    signalling_peer.set_on_create_offer(create_offer);
    signalling_peer.set_on_receive_answer(receive_answer);
    signalling_peer.set_on_local_sig_cplt(local_sig_cplt);
    signalling_peer.set_on_receive_offer(receive_offer);
    signalling_peer.set_on_remote_sig_cplt(remote_sig_cplt);
    // run the signalling peer
    loop {
        signalling_peer.step().await?;
    }
    Ok(())
}

// For the purpose of this example, the descriptions and candidates will be strings.
// In actual code, these types would be serializable types provided by the WebRTC implementation
type Description = String;
type ICECandidates = String;

/// implement create offer callback (called by the signalling client to create an offer)
async fn create_offer(remote_id: u64) -> Result<(u64, (Description, ICECandidates))> {
    // User's WebRTC implementation creates a local peer connection here.
    // Generating an offer and ICE candidates and returning them.
    // The signalling client will deliver the signal to the remote peer with the corresponding `remote_id`
    let offer = Description::new();
    let offerer_candidates = ICECandidates::new();
    Ok((remote_id, (offer, offerer_candidates)))
}

/// implement receive answer callback (called by the signalling client to deliver an answer)
async fn receive_answer(remote_id: u64, answer_set: (Description, ICECandidates)) -> Result<u64> {
    // User's WebRTC implementation receives an answer from a remote peer to a local peer connection here.
    Ok(remote_id)
}

/// implement 'local' (offerer peer) signalling complete callback
async fn local_sig_cplt(remote_id: u64) -> Result<u64> {
    // User's WebRTC implementation may now open/receive data channels
    // and perform send/receives across channels.
    Ok(remote_id)
}

/// implement receive offer callback (called by the signalling client to deliver an offer and create an answer)
async fn receive_offer(remote_id: u64, offer_set: (Description, ICECandidates)) -> Result<(u64, (Description, ICECandidates))> {
    // User's WebRTC implementation creates a remote peer connection from the remote offer and candidates here.
    // Generating an answer and ICE candidates and returning them.
    // The signalling client will deliver the signal to the remote peer with the corresponding `remote_id`
    let answer = Description::new();
    let answerer_candidates = ICECandidates::new();
    Ok((remote_id, (answer, answerer_candidates)))
}

/// implement 'remote' (answerer peer) signalling complete callback
async fn remote_sig_cplt(remote_id: u64) -> Result<u64> {
    // User's WebRTC implementation may now open/receive data channels
    // and perform send/receives across channels.
    Ok(remote_id)
}
```

**Running a server:**

Running a `just-webrtc-signalling` server is simple. Just configure & create the services you need and run concurrently!

You can also add `RtcSignallingService` alongside other services on your existing `tonic` servers!

The below example runs two services, one for native clients and one for web clients. The services share common signalling channels to enable cross-platform signalling between web and native.

```rust
use std::sync::Arc;

use anyhow::Result;
use just_webrtc_signalling::{server::Signalling, DEFAULT_NATIVE_SERVER_ADDR, DEFAULT_WEB_SERVER_ADDR};

async fn serve() -> Result<()> {
    // create shared mapped signalling channels
    let signalling = Arc::new(Signalling::new());
    // create service futures
    let serve_fut = just_webrtc_signalling::server::serve(
        signalling.clone(),
        DEFAULT_NATIVE_SERVER_ADDR.parse()?,
        None, None,  // no keepalive interval or timeout
        None,  // no TLS
    );
    let serve_web_fut = just_webrtc_signalling::server::serve_web(
        signalling,
        DEFAULT_WEB_SERVER_ADDR.parse()?,
        None, None,  // no keepalive interval or timeout
        None,  // no TLS
    );
    // run native and web facing services concurrently
    tokio::try_join!(serve_fut, serve_web_fut)?;
    Ok(())
}
```

## License
This project is licensed under either of
* [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0)
* [MIT License](https://opensource.org/licenses/MIT)
at your option.
