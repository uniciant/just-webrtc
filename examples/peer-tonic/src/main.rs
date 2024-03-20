use std::{collections::HashMap, time::Duration};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use log::info;

use just_webrtc::{platform::PeerConnection, types::DataChannelOptions, PeerConnectionBuilder};
use protocol_tonic::client::{RtcSignallingClient, SignalSet};
use tokio::sync::Mutex;

const ECHO_REQUEST: &str = "I'm literally Ryan Gosling.";
const ECHO_RESPONSE: &str = "I know right! He's literally me.";

async fn peer_echo_task(
    remote_id: &u64,
    mut peer_connection: PeerConnection,
) -> Result<()> {
    info!("started peer echo task. ({remote_id})");
    peer_connection.wait_peer_connected().await?;
    let mut channel = peer_connection.receive_channel().await.unwrap();
    channel.wait_ready().await?;
    // prepare echo request/response
    let channel_fmt = format!("{}:{}", channel.label(), channel.id());
    // if offerer, we send echo requests, otherwise we listen for requests
    if peer_connection.is_offerer() {
        let echo_request = bincode::serialize(ECHO_REQUEST)?.into();
        let mut interval = zduny_wasm_timer::Interval::new(Duration::from_secs(1));
        loop {
            interval.next().await;
            // offerer makes echo requests
            info!("Sending echo request!");
            let instant = zduny_wasm_timer::Instant::now();
            channel.send(&echo_request).await?;
            // await response
            if let Some(bytes) = channel.receive().await {
                let s: &str = bincode::deserialize(&bytes)?;
                if s == ECHO_RESPONSE {
                    let elapsed_us = instant.elapsed().as_micros();
                    info!("Received echo response!({channel_fmt}) ({elapsed_us}us)");
                }
            } else {
                break Err(anyhow!("remote channel closed! ({channel_fmt})"));
            }
        }?;
    } else {
        let echo_response = bincode::serialize(ECHO_RESPONSE)?.into();
        loop {
            // await request
            if let Some(bytes) = channel.receive().await {
                let s: &str = bincode::deserialize(&bytes)?;
                if s == ECHO_REQUEST {
                    info!("Received echo request! Sending response. ({channel_fmt})");
                    channel.send(&echo_response).await?;
                }
            } else {
                break Err(anyhow!("remote channel closed! ({channel_fmt})"));
            }
        }?;
    }
    Ok(())
}

async fn create_offer(remote_id: u64) -> Result<(SignalSet, PeerConnection)> {
    let channel_options = vec![
        (format!("rtc_channel_to_{:#x}_", remote_id), DataChannelOptions::default())
    ];
    let mut local_peer_connection = PeerConnectionBuilder::new()
        .with_channel_options(channel_options)?
        .build().await?;
    let offer = local_peer_connection.get_local_description().await
        .ok_or(anyhow!("could not get local description!"))?;
    let candidates = local_peer_connection.collect_ice_candidates().await;
    Ok((SignalSet { desc: offer, candidates, remote_id }, local_peer_connection))
}

async fn receive_offer(offer_set: SignalSet) -> Result<(SignalSet, PeerConnection)> {
    let mut remote_peer_connection = PeerConnectionBuilder::new()
        .with_remote_offer(Some(offer_set.desc))?
        .build().await?;
    remote_peer_connection.add_ice_candidates(offer_set.candidates).await?;
    let answer = remote_peer_connection.get_local_description().await
        .ok_or(anyhow!("could not get remote description!"))?;
    let candidates = remote_peer_connection.collect_ice_candidates().await;
    let answer_set = SignalSet {
        desc: answer,
        candidates,
        remote_id: offer_set.remote_id,
    };
    Ok((answer_set, remote_peer_connection))
}

async fn receive_answer(
    answer_set: SignalSet,
    local_peer_connection: &PeerConnection,
) -> Result<()> {
    local_peer_connection.set_remote_description(answer_set.desc).await?;
    local_peer_connection.add_ice_candidates(answer_set.candidates).await?;
    Ok(())
}

async fn run_peer(addr: &str) -> Result<()> {
    // map for passing peer connections
    let peer_connections: &Mutex<HashMap<u64, PeerConnection>> = &Mutex::new(HashMap::new());

    let mut signalling_client = RtcSignallingClient::connect(addr.to_string(), None).await?;
    signalling_client.run(
        // create offer callback
        &|remote_id| async move {
            let (offer_set, local_peer_connection) = create_offer(remote_id).await?;
            let mut peer_connections = peer_connections.lock().await;
            if peer_connections.insert(remote_id, local_peer_connection).is_some() {
                return Err(anyhow!("peer connection already exists ({remote_id:#016x})"));
            }
            Ok::<_, anyhow::Error>(offer_set)
        },
        // receive answer callback
        &|answer_set| async move {
            let peer_connections = peer_connections.lock().await;
            let local_peer_connection = peer_connections.get(&answer_set.remote_id)
                .ok_or(anyhow!("could not get local peer connection!"))?;
            receive_answer(answer_set, &local_peer_connection).await?;
            Ok::<_, anyhow::Error>(())
        },
        // local signalling complete callback
        // start echo task on local peer
        &|remote_id| async move {
            let mut peer_connections = peer_connections.lock().await;
            let local_peer_connection = peer_connections.remove(&remote_id)
                .ok_or(anyhow!("could not get local peer connection!"))?;
            peer_echo_task(&remote_id, local_peer_connection).await?;
            Ok::<_, anyhow::Error>(())
        },
        // receive offer callback
        &|offer_set| async move {
            let (answer_set, remote_peer_connection) = receive_offer(offer_set).await?;
            let mut peer_connections = peer_connections.lock().await;
            if peer_connections.insert(answer_set.remote_id, remote_peer_connection).is_some() {
                return Err(anyhow!("peer connection already exists ({:#016x})", answer_set.remote_id));
            }
            Ok::<_, anyhow::Error>(answer_set)
        },
        // remote signalling complete callback
        // start echo task on remote peer
        &|remote_id| async move {
            let mut peer_connections = peer_connections.lock().await;
            let remote_peer_connection = peer_connections.remove(&remote_id)
                .ok_or(anyhow!("could not get remote peer connection!"))?;
            peer_echo_task(&remote_id, remote_peer_connection).await?;
            Ok::<_, anyhow::Error>(())
        },
    ).await?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
// Run me via `trunk serve`!
fn main() -> Result<()> {
    use log::error;
    use protocol_tonic::TONiC_WEB_SERVER_ADDR;

    console_log::init_with_level(log::Level::Debug)?;
    info!("starting web peer!");
    // run locally detached
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = run_peer(TONIC_WEB_SERVER_ADDR).await {
            error!("{e}")
        }
    });
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> Result<()> {
    use protocol_tonic::TONIC_NATIVE_SERVER_ADDR;

    pretty_env_logger::try_init()?;
    info!("starting local peer!");
    run_peer(TONIC_NATIVE_SERVER_ADDR).await
}
