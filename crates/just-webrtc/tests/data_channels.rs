use anyhow::{anyhow, Result};
use bytes::Bytes;
use just_webrtc::types::{DataChannelOptions, PeerConnectionState};
use just_webrtc::{DataChannelExt, PeerConnectionBuilder, PeerConnectionExt};
use log::debug;

async fn test_data_channel() -> Result<()> {
    // create local peer connection from channel options
    debug!("create local peer connection");
    let channel_options = vec![(format!("test_channel_"), DataChannelOptions::default())];
    let local_peer_connection = PeerConnectionBuilder::new()
        .with_channel_options(channel_options)?
        .build()
        .await?;
    // output offer and candidates for remote peer
    let offer = local_peer_connection
        .get_local_description()
        .await
        .ok_or(anyhow!("could not get local description! (offer)"))?;
    let candidates = local_peer_connection.collect_ice_candidates().await?;

    // (send the offer and the candidates from local to the remote, via user's signalling implementation)

    // create remote peer connection from received offer and candidates
    debug!("create remote peer connection");
    let remote_peer_connection = PeerConnectionBuilder::new()
        .with_remote_offer(Some(offer))?
        .build()
        .await?;
    remote_peer_connection
        .add_ice_candidates(candidates)
        .await?;
    // output answer and candidates for local peer
    let answer = remote_peer_connection
        .get_local_description()
        .await
        .ok_or(anyhow!("could not get remote description! (answer)"))?;
    let candidates = remote_peer_connection.collect_ice_candidates().await?;

    // (send the answer and the candidates from the remote to the local, via user's signalling implementation)

    // update local peer from received answer and candidates
    debug!("update local peer from received answer/candidates");
    local_peer_connection.set_remote_description(answer).await?;
    local_peer_connection.add_ice_candidates(candidates).await?;
    // wait for local and remote peers to connect
    debug!("wait for local and remote peers to connect");
    while local_peer_connection.state_change().await != PeerConnectionState::Connected {}
    while remote_peer_connection.state_change().await != PeerConnectionState::Connected {}
    // receive data channel from local and remote peers
    debug!("receive data channel from local and remote peers");
    let local_channel = local_peer_connection.receive_channel().await.unwrap();
    let remote_channel = remote_peer_connection.receive_channel().await.unwrap();
    // wait for data channels to be ready
    debug!("wait for data channels to be ready");
    local_channel.wait_ready().await;
    remote_channel.wait_ready().await;

    // send/recv data from local to remote
    debug!("send/recv data from local to remote");
    local_channel.send(&Bytes::from("hello remote!")).await?;
    let recv = remote_channel.receive().await.unwrap();
    assert_eq!(&recv, "hello remote!");
    // send/recv data from remote to local
    debug!("send/recv data from remote to local");
    remote_channel.send(&Bytes::from("hello local!")).await?;
    let recv = local_channel.receive().await.unwrap();
    assert_eq!(&recv, "hello local!");
    Ok(())
}

#[tokio::test]
async fn test_data_channel_native() -> Result<()> {
    pretty_env_logger::try_init()?;
    test_data_channel().await
}

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);
#[wasm_bindgen_test::wasm_bindgen_test]
async fn test_data_channel_web() -> Result<()> {
    console_log::init_with_level(log::Level::Trace)?;
    test_data_channel().await
}
