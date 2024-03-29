# Just WebRTC

[![crates.io](https://img.shields.io/crates/v/just-webrtc?style=flat-square&logo=rust)](https://crates.io/crates/just-webrtc)
[![license](https://img.shields.io/badge/license-Apache--2.0_OR_MIT-blue?style=flat-square)](#license)
[![build status](https://img.shields.io/github/actions/workflow/status/uniciant/just-webrtc/rust.yml?branch=main&style=flat-square&logo=github)](https://github.com/uniciant/just-webrtc/actions)

Just simple, fast and easy WebRTC peers in Rust.

Supports WebRTC on both `native` and `wasm32` targets, with an identical API for both.

`just-webrtc` is modular, only including WebRTC types and implementations. No signalling here!

Roll your own signalling setup, or use the ready-made standard signalling client/server at [`just-webrtc-signalling`](https://crates.io/crates/just-webrtc-signalling)

... Maybe WebRTC *can* be easy?

```toml
[dependencies]
just-webrtc = "0.1"
```

## Documentation
See [docs.rs](https://docs.rs/just-webrtc) for the complete API reference.

## Examples
This basic example creates a 'local' and a 'remote' peer with a single data channel.

See the [`data_channels`](https://github.com/uniciant/just-webrtc/blob/main/crates/just-webrtc/tests/data_channels.rs) test for a compiling version of this example.

For complete examples, including signalling, see the [**Examples**](https://github.com/uniciant/just-webrtc/tree/main/examples) in the repository.

**Peer A - Local peer (creates the local peer from configuration):**

*Note: "local peer" is WebRTC-speak for the initial peer that creates the offer. Internally this peer is also referred to as the "offerer"*
```rust
use anyhow::Result;
use just_webrtc::{
    DataChannelExt,
    PeerConnectionExt,
    SimpleLocalPeerConnection,
    types::{SessionDescription, ICECandidate}
};

async fn run_local_peer() -> Result<()> {
    // create simple local peer connection with unordered data channel
    let mut local_peer_connection = SimpleLocalPeerConnection::build(false).await?;

    // output offer and candidates for remote peer
    let offer = local_peer_connection.get_local_description().await.unwrap();
    let candidates = local_peer_connection.collect_ice_candidates().await?;

    // ... send the offer and the candidates to Peer B via external signalling implementation ...
    let signalling = (offer, candidates);

    // ... receive the answer and candidates from peer via external signalling implementation ...
    let (answer, candidates) = signalling;

    // update local peer from received answer and candidates
    local_peer_connection.set_remote_description(answer).await?;
    local_peer_connection.add_ice_candidates(candidates).await?;

    // local signalling is complete! we can now wait for a complete connection
    local_peer_connection.wait_peer_connected().await;

    // receive data channel from local peer
    let mut local_channel = local_peer_connection.receive_channel().await.unwrap();
    // wait for data channels to be ready
    local_channel.wait_ready().await;

    // send data to remote (answerer)
    local_channel.send(&bytes::Bytes::from("hello remote!")).await?;
    // recv data from remote (answerer)
    let recv = local_channel.receive().await.unwrap();
    assert_eq!(&recv, "hello local!");

    Ok(())
}
```

**Peer B - Remote peer (creates the remote peer from a received offer):**

*Note: "remote peer" is WebRTC-speak for the peer that receives an offer from the "local peer". Internally this peer is also referred to as the "answerer"*
```rust
use anyhow::Result;
use just_webrtc::{
    DataChannelExt,
    PeerConnectionExt,
    SimpleRemotePeerConnection,
    types::{SessionDescription, ICECandidate}
};

async fn run_remote_peer(offer: SessionDescription, candidates: Vec<ICECandidate>) -> Result<()> {
    // ... receive the offer and the candidates from Peer A via external signalling implementation ...

    // create simple remote peer connection from received offer and candidates
    let mut remote_peer_connection = SimpleRemotePeerConnection::build(offer).await?;
    remote_peer_connection.add_ice_candidates(candidates).await?;
    // output answer and candidates for local peer
    let answer = remote_peer_connection.get_local_description().await.unwrap();
    let candidates = remote_peer_connection.collect_ice_candidates().await?;

    // ... send the answer and the candidates back to Peer B via external signalling implementation ...
    let _signalling = (answer, candidates);

    // remote signalling is complete! we can now wait for a complete connection
    remote_peer_connection.wait_peer_connected().await;

    // receive data channel from local and remote peers
    let mut remote_channel = remote_peer_connection.receive_channel().await.unwrap();
    // wait for data channels to be ready
    remote_channel.wait_ready().await;

    // send/recv data from local (offerer) to remote (answerer)
    let recv = remote_channel.receive().await.unwrap();
    assert_eq!(&recv, "hello remote!");
    // send/recv data from remote (answerer) to local (offerer)
    remote_channel.send(&bytes::Bytes::from("hello local!")).await?;

    Ok(())
}
```

## License
This project is licensed under either of
* [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0)
* [MIT License](https://opensource.org/licenses/MIT)
at your option.
