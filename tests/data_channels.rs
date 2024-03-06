use anyhow::{anyhow, Result};
use bytes::Bytes;

use just_webrtc::{DataChannelConfig, PeerConnectionBuilder};

#[tokio::test]
async fn test_data_channel() -> Result<()> {
    pretty_env_logger::try_init()?;
    // create local peer connection from channel options
    let channel_options = vec![
        (format!("test_channel_"), DataChannelConfig::default())
    ];
    let mut local_peer_connection = PeerConnectionBuilder::new()
        .with_channel_options(channel_options)
        .build()
        .await?;
    // output offer and candidates for remote peer
    let offer = local_peer_connection.get_local_description().await
        .ok_or(anyhow!("could not get local description! (offer)"))?;
    let candidates = local_peer_connection.collect_ice_candidates().await;

    // (send the offer and the candidates from local to the remote, via user's signalling implementation)

    // create remote peer connection from received offer and candidates
    let mut remote_peer_connection = PeerConnectionBuilder::new()
        .with_remote_offer(Some(offer))
        .build()
        .await?;
    remote_peer_connection.add_ice_candidates(candidates).await?;
    // output answer and candidates for local peer
    let answer = remote_peer_connection.get_local_description().await
        .ok_or(anyhow!("could not get remote description! (answer)"))?;
    let candidates = remote_peer_connection.collect_ice_candidates().await;

    // (send the answer and the candidates from the remote to the local, via user's signalling implementation)

    // update local peer from received answer and candidates
    local_peer_connection.set_remote_description(answer).await?;
    local_peer_connection.add_ice_candidates(candidates).await?;
    // wait for local and remote peers to connect
    local_peer_connection.wait_peer_connected().await?;
    remote_peer_connection.wait_peer_connected().await?;
    // receive data channel from local and remote peers
    let mut local_channel = local_peer_connection.receive_channel().await.unwrap();
    let mut remote_channel = remote_peer_connection.receive_channel().await.unwrap();
    // wait for data channels to be ready
    local_channel.wait_ready().await?;
    remote_channel.wait_ready().await?;
    // send/recv data from local to remote
    local_channel.send(&Bytes::from("hello remote!")).await?;
    let recv = remote_channel.receive().await.unwrap();
    assert_eq!(&recv, "hello remote!");
    // send/recv data from remote to local
    remote_channel.send(&Bytes::from("hello local!")).await?;
    let recv = local_channel.receive().await.unwrap();
    assert_eq!(&recv, "hello local!");
    Ok(())
}
