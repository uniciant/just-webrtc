use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Duration};

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

async fn peer_echo_task(remote_id: u64, mut peer_connection: PeerConnection) -> Result<u64> {
    info!("started peer echo task. ({remote_id})");
    // take peer connection from the map
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

async fn create_offer(remote_id: u64) -> Result<(SignalSet, PeerConnection)> {
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
    let signal = SignalSet {
        desc: offer,
        candidates,
        remote_id,
    };
    Ok((signal, local_peer_connection))
}

async fn receive_offer(offer_set: SignalSet) -> Result<(SignalSet, PeerConnection)> {
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
    Ok((answer_set, remote_peer_connection))
}

async fn receive_answer(
    answer_set: SignalSet,
    local_peer_connection: PeerConnection,
) -> Result<(u64, PeerConnection)> {
    let remote_id = answer_set.remote_id;
    // set answer description and add candidates
    local_peer_connection
        .set_remote_description(answer_set.desc)
        .await?;
    local_peer_connection
        .add_ice_candidates(answer_set.candidates)
        .await?;
    Ok((remote_id, local_peer_connection))
}

/// Peer connection map for passing connections between callback functions
#[derive(Debug, Default)]
struct PeerConnectionMap {
    inner: HashMap<u64, Option<PeerConnection>>,
}

impl PeerConnectionMap {
    fn insert(&mut self, id: u64, peer_connection: PeerConnection) -> Result<()> {
        if self.inner.insert(id, Some(peer_connection)).is_some() {
            return Err(anyhow!("pre-existing peer ID!"));
        }
        Ok(())
    }

    fn take(&mut self, id: &u64) -> Result<PeerConnection> {
        self.inner
            .get_mut(id)
            .ok_or(anyhow!("peer ID not found!"))?
            .take()
            .ok_or(anyhow!("peer connection not found!"))
    }

    fn place(&mut self, id: &u64, peer_connection: PeerConnection) -> Result<()> {
        if self
            .inner
            .get_mut(id)
            .ok_or(anyhow!("peer ID not found!"))?
            .replace(peer_connection)
            .is_some()
        {
            return Err(anyhow!("dropped peer connection!"));
        }
        Ok(())
    }
}

async fn run_peer(addr: &str) -> Result<()> {
    // create locally shared peer connections map
    let peer_connections: Rc<RefCell<PeerConnectionMap>> =
        Rc::new(RefCell::new(PeerConnectionMap::default()));
    // prepare create offer callback function
    let create_offer_fn = |remote_id| {
        let peer_connections = peer_connections.clone();
        async move {
            let (offer, local_peer_connection) = create_offer(remote_id).await?;
            peer_connections
                .borrow_mut()
                .insert(remote_id, local_peer_connection)?;
            Ok(offer)
        }
        .boxed_local()
    };
    // prepare receive answer callback function
    let receive_answer_fn = |answer_set: SignalSet| {
        let peer_connections = peer_connections.clone();
        async move {
            let peer_connection = peer_connections.borrow_mut().take(&answer_set.remote_id)?;
            let (remote_id, peer_connection) = receive_answer(answer_set, peer_connection).await?;
            peer_connections
                .borrow_mut()
                .place(&remote_id, peer_connection)?;
            Ok(remote_id)
        }
        .boxed_local()
    };
    // prepare local signalling complete callback function
    let local_sig_cplt_fn = |remote_id| {
        let peer_connections = peer_connections.clone();
        async move {
            let local_peer_connection = peer_connections.borrow_mut().take(&remote_id)?;
            peer_echo_task(remote_id, local_peer_connection).await
        }
        .boxed_local()
    };
    // prepare receive offer callback function
    let receive_offer_fn = |offer_set| {
        let peer_connections = peer_connections.clone();
        async move {
            let (answer, remote_peer_connection) = receive_offer(offer_set).await?;
            peer_connections
                .borrow_mut()
                .insert(answer.remote_id, remote_peer_connection)?;
            Ok(answer)
        }
        .boxed_local()
    };
    // prepare remote signalling complete callback function
    let remote_sig_cplt_fn = |remote_id| {
        let peer_connections = peer_connections.clone();
        async move {
            let remote_peer_connection = peer_connections.borrow_mut().take(&remote_id)?;
            peer_echo_task(remote_id, remote_peer_connection).await
        }
        .boxed_local()
    };
    // create signalling client
    let mut signalling_client =
        RtcSignallingClient::new(addr.to_string(), None, false, None, None)?;
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

    console_log::init_with_level(log::Level::Debug).unwrap();
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
