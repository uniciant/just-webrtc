use std::{cell::OnceCell, collections::HashMap, sync::Mutex, time::Duration};

use anyhow::{anyhow, Result};
use futures_util::{FutureExt, StreamExt};
use log::info;

use just_webrtc::{
    platform::PeerConnection,
    types::{DataChannelOptions, ICECandidate, SessionDescription},
    DataChannelExt, PeerConnectionBuilder, PeerConnectionExt,
};
use just_webrtc_signalling::client::RtcSignallingClient;

const ECHO_REQUEST: &str = "I'm literally Ryan Gosling.";
const ECHO_RESPONSE: &str = "I know right! He's literally me.";

// fill signal set generics
type SignalSet = just_webrtc_signalling::client::SignalSet<SessionDescription, Vec<ICECandidate>>;
// thread local map for passing peer connections
thread_local! {
    static PEER_CONNECTIONS: OnceCell<Mutex<HashMap<u64, PeerConnection>>> = const { OnceCell::new() };
}

// start echo task on local/remote peer
async fn peer_echo_task(remote_id: u64) -> Result<u64> {
    info!("started peer echo task. ({remote_id})");
    let mut peer_connection = PEER_CONNECTIONS
        .with(|cell| {
            let peer_connections = cell.get().unwrap();
            let mut peer_connections = peer_connections.lock().unwrap();
            peer_connections.remove(&remote_id)
        })
        .ok_or(anyhow!("could not get peer connection"))?;
    peer_connection.wait_peer_connected().await;
    let mut channel = peer_connection.receive_channel().await.unwrap();
    channel.wait_ready().await;
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
            let bytes = channel.receive().await?;
            let s: &str = bincode::deserialize(&bytes)?;
            if s == ECHO_RESPONSE {
                let elapsed_us = instant.elapsed().as_micros();
                info!("Received echo response!({channel_fmt}) ({elapsed_us}us)");
            }
        }
    } else {
        let echo_response = bincode::serialize(ECHO_RESPONSE)?.into();
        loop {
            // await request
            let bytes = channel.receive().await?;
            let s: &str = bincode::deserialize(&bytes)?;
            if s == ECHO_REQUEST {
                info!("Received echo request! Sending response. ({channel_fmt})");
                channel.send(&echo_response).await?;
            }
        }
    }
}

async fn create_offer(remote_id: u64) -> Result<SignalSet> {
    let channel_options = vec![(
        format!("rtc_channel_to_{remote_id:#016x}_"),
        DataChannelOptions::default(),
    )];
    let mut local_peer_connection = PeerConnectionBuilder::new()
        .with_channel_options(channel_options)?
        .build()
        .await?;
    let offer = local_peer_connection
        .get_local_description()
        .await
        .ok_or(anyhow!("could not get local description!"))?;
    let candidates = local_peer_connection.collect_ice_candidates().await?;
    // add new local peer connection to map
    PEER_CONNECTIONS.with(|cell| {
        let peer_connections = cell.get().unwrap();
        let mut peer_connections = peer_connections.lock().unwrap();
        if peer_connections
            .insert(remote_id, local_peer_connection)
            .is_some()
        {
            Err(anyhow!("peer connection already exists"))
        } else {
            Ok(())
        }
    })?;
    Ok(SignalSet {
        desc: offer,
        candidates,
        remote_id,
    })
}

async fn receive_offer(offer_set: SignalSet) -> Result<SignalSet> {
    let mut remote_peer_connection = PeerConnectionBuilder::new()
        .with_remote_offer(Some(offer_set.desc))?
        .build()
        .await?;
    remote_peer_connection
        .add_ice_candidates(offer_set.candidates)
        .await?;
    let answer = remote_peer_connection
        .get_local_description()
        .await
        .ok_or(anyhow!("could not get remote description!"))?;
    let candidates = remote_peer_connection.collect_ice_candidates().await?;
    let remote_id = offer_set.remote_id;
    let answer_set = SignalSet {
        desc: answer,
        candidates,
        remote_id,
    };
    // add new remote peer connection to map
    PEER_CONNECTIONS.with(|cell| {
        let peer_connections = cell.get().unwrap();
        let mut peer_connections = peer_connections.lock().unwrap();
        if peer_connections
            .insert(remote_id, remote_peer_connection)
            .is_some()
        {
            Err(anyhow!("peer connection already exists"))
        } else {
            Ok(())
        }
    })?;
    Ok(answer_set)
}

async fn receive_answer(answer_set: SignalSet) -> Result<u64> {
    let remote_id = answer_set.remote_id;
    // take local peer connection from map
    let local_peer_connection = PEER_CONNECTIONS
        .with(|cell| {
            let peer_connections = cell.get().unwrap();
            let mut peer_connections = peer_connections.lock().unwrap();
            peer_connections.remove(&remote_id)
        })
        .ok_or(anyhow!("could not get local peer connection!"))?;
    // set answer description and add candidates
    local_peer_connection
        .set_remote_description(answer_set.desc)
        .await?;
    local_peer_connection
        .add_ice_candidates(answer_set.candidates)
        .await?;
    // return local peer connection map
    PEER_CONNECTIONS.with(|cell| {
        let peer_connections = cell.get().unwrap();
        let mut peer_connections = peer_connections.lock().unwrap();
        if peer_connections
            .insert(remote_id, local_peer_connection)
            .is_some()
        {
            Err(anyhow!("peer connection already exists"))
        } else {
            Ok(())
        }
    })?;
    Ok(remote_id)
}

async fn run_peer(addr: &str) -> Result<()> {
    // initialize peer connections map
    PEER_CONNECTIONS
        .with(|cell| cell.set(Mutex::new(HashMap::new())))
        .map_err(|_e| anyhow!("peer connections `OnceCell` already filled!"))?;
    // prepare callback functions
    let create_offer_fn = Box::new(|remote_id| create_offer(remote_id).boxed_local());
    let receive_answer_fn = Box::new(|answer_set| receive_answer(answer_set).boxed_local());
    let local_sig_cplt_fn = Box::new(|remote_id| peer_echo_task(remote_id).boxed_local());
    let receive_offer_fn = Box::new(|offer_set| receive_offer(offer_set).boxed_local());
    let remote_sig_cplt_fn = Box::new(|remote_id| peer_echo_task(remote_id).boxed_local());
    // create signalling client
    let mut signalling_client =
        RtcSignallingClient::connect(addr.to_string(), None, false, None, None).await?;
    // run signalling client
    signalling_client
        .run(
            create_offer_fn,
            receive_answer_fn,
            local_sig_cplt_fn,
            receive_offer_fn,
            remote_sig_cplt_fn,
        )
        .await?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
// Run me via `trunk serve`!
fn main() {
    use just_webrtc_signalling::DEFAULT_WEB_SERVER_ADDR;
    use log::error;

    console_log::init_with_level(log::Level::Trace).unwrap();
    info!("starting web peer!");
    // run locally, detached
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = run_peer(DEFAULT_WEB_SERVER_ADDR).await {
            error!("{e}")
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    use just_webrtc_signalling::DEFAULT_NATIVE_SERVER_ADDR;

    pretty_env_logger::try_init()?;
    info!("starting native peer!");
    run_peer(DEFAULT_NATIVE_SERVER_ADDR).await
}
