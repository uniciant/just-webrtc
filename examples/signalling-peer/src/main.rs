use anyhow::{anyhow, Result};
use futures_util::future::{Fuse, FusedFuture};
use futures_util::{pin_mut, select, FutureExt, StreamExt};
use just_webrtc::platform::{Channel, PeerConnection};
use just_webrtc::types::{
    DataChannelOptions, ICECandidate, PeerConnectionState, SessionDescription,
};
use just_webrtc::{DataChannelExt, PeerConnectionBuilder, PeerConnectionExt};
use just_webrtc_signalling::client::RtcSignallingClientBuilder;
use log::{error, info, warn};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

const ECHO_REQUEST: &str = "I'm literally Ryan Gosling.";
const ECHO_RESPONSE: &str = "I know right! He's literally me.";
const ECHO_PERIOD: Duration = Duration::from_millis(1000);
const CONNECTION_DROP_TIMEOUT: Duration = Duration::from_secs(15);

async fn channel_echo_task(offerer: bool, channel: Channel) -> Result<()> {
    let channel_fmt = format!("{}:{}", channel.label(), channel.id());
    // if offerer, we send echo requests, otherwise we listen for requests
    if offerer {
        let echo_request = bincode::serialize(ECHO_REQUEST)?.into();
        let mut interval = zduny_wasm_timer::Interval::new(ECHO_PERIOD);
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

async fn peer_echo_task(remote_id: &u64, peer_connection: PeerConnection) -> Result<()> {
    info!("started peer echo task. ({remote_id})");
    let state_change_fut = peer_connection.state_change().fuse();
    let channel_ready_fut = Fuse::terminated();
    let channel_task_fut = Fuse::terminated();
    let connection_drop_timeout_fut = Fuse::terminated();
    pin_mut!(
        state_change_fut,
        channel_task_fut,
        channel_ready_fut,
        connection_drop_timeout_fut
    );
    // peer connection monitoring loop
    // in this example we use the futures crate select! macro as a runtime agnostic method
    // for concurrently monitoring peer connection state and running a single channel task.
    loop {
        select! {
            state = state_change_fut => {
                state_change_fut.set(peer_connection.state_change().fuse());
                match state {
                    PeerConnectionState::New |
                    PeerConnectionState::Connected => {
                        // clear the timeout fut
                        if !connection_drop_timeout_fut.is_terminated() {
                            info!("peer connection recovered! ({remote_id})");
                            connection_drop_timeout_fut.set(Fuse::terminated());
                        } else {
                            info!("peer connected. ({remote_id})");
                            let channel = peer_connection.receive_channel().await.unwrap();
                            let offerer = peer_connection.is_offerer();
                            channel_ready_fut.set(async move {
                                channel.wait_ready().await;
                                (channel, offerer)
                            }.fuse());
                        }
                    },
                    PeerConnectionState::Disconnected => {
                        warn!("peer connection interrupted. attempting to recover... ({remote_id})");
                        let timeout = zduny_wasm_timer::Delay::new(CONNECTION_DROP_TIMEOUT);
                        connection_drop_timeout_fut.set(timeout.fuse());
                    },
                    PeerConnectionState::Failed => {
                        error!("peer connection failed! ({remote_id})");
                        return Ok(())
                    }
                    PeerConnectionState::Closed => {
                        info!("peer connection closed! ({remote_id})");
                        return Ok(())
                    }
                    _ => {},
                }
            },
            (channel, offerer) = channel_ready_fut => {
                channel_task_fut.set(channel_echo_task(offerer, channel).fuse());
            }
            result = channel_task_fut => result?,
            result = connection_drop_timeout_fut => {
                result?;
                error!("timeout waiting for connection to re-establish. ({remote_id})");
                return Ok(())
            },
        }
    }
}

async fn create_offer() -> Result<((SessionDescription, Vec<ICECandidate>), PeerConnection)> {
    let channel_options = vec![(
        "example_channel_".to_string(),
        DataChannelOptions::default(),
    )];
    let local_peer_connection = PeerConnectionBuilder::new()
        .with_channel_options(channel_options)?
        .build()
        .await?;
    let offer = local_peer_connection
        .get_local_description()
        .await
        .ok_or(anyhow!("could not get local description!"))?;
    let candidates = local_peer_connection.collect_ice_candidates().await?;
    Ok(((offer, candidates), local_peer_connection))
}

async fn receive_offer(
    offer: SessionDescription,
    candidates: Vec<ICECandidate>,
) -> Result<((SessionDescription, Vec<ICECandidate>), PeerConnection)> {
    let remote_peer_connection = PeerConnectionBuilder::new()
        .with_remote_offer(Some(offer))?
        .build()
        .await?;
    remote_peer_connection
        .add_ice_candidates(candidates)
        .await?;
    let answer = remote_peer_connection
        .get_local_description()
        .await
        .ok_or(anyhow!("could not get remote description!"))?;
    let candidates = remote_peer_connection.collect_ice_candidates().await?;
    Ok(((answer, candidates), remote_peer_connection))
}

async fn receive_answer(
    answer: SessionDescription,
    candidates: Vec<ICECandidate>,
    local_peer_connection: &PeerConnection,
) -> Result<()> {
    // set answer description and add candidates
    local_peer_connection.set_remote_description(answer).await?;
    local_peer_connection.add_ice_candidates(candidates).await?;
    Ok(())
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
    // prepare create offer callback closure
    let peer_connections_offer = peer_connections.clone();
    let create_offer_fn = move |remote_id: u64| {
        let peer_connections = peer_connections_offer.clone();
        async move {
            let (offer_set, local_peer_connection) = create_offer().await?;
            // add local peer connection to the map
            peer_connections
                .borrow_mut()
                .insert(remote_id, local_peer_connection)?;
            Ok::<_, anyhow::Error>((remote_id, offer_set))
        }
    };
    // prepare receive answer callback closure
    let peer_connections_answer = peer_connections.clone();
    let receive_answer_fn = move |remote_id, (answer, candidates)| {
        let peer_connections = peer_connections_answer.clone();
        async move {
            // take peer connection from the map
            let peer_connection = peer_connections.borrow_mut().take(&remote_id)?;
            receive_answer(answer, candidates, &peer_connection).await?;
            peer_connections
                .borrow_mut()
                .place(&remote_id, peer_connection)?;
            Ok::<_, anyhow::Error>(remote_id)
        }
    };
    // prepare local signalling complete callback closure
    let peer_connections_local = peer_connections.clone();
    let local_sig_cplt_fn = move |remote_id| {
        let peer_connections = peer_connections_local.clone();
        async move {
            // take peer connection from the map
            let local_peer_connection = peer_connections.borrow_mut().take(&remote_id)?;
            peer_echo_task(&remote_id, local_peer_connection).await?;
            Ok::<_, anyhow::Error>(remote_id)
        }
    };
    // prepare receive offer callback closure
    let peer_connections_offer = peer_connections.clone();
    let receive_offer_fn = move |remote_id: u64, (offer, candidates)| {
        let peer_connections = peer_connections_offer.clone();
        async move {
            let (answer_set, remote_peer_connection) = receive_offer(offer, candidates).await?;
            // add remote peer connection to the map
            peer_connections
                .borrow_mut()
                .insert(remote_id, remote_peer_connection)?;
            Ok::<_, anyhow::Error>((remote_id, answer_set))
        }
    };
    // prepare remote signalling complete callback closure
    let peer_connections_remote = peer_connections.clone();
    let remote_sig_cplt_fn = move |remote_id| {
        let peer_connections = peer_connections_remote.clone();
        async move {
            let remote_peer_connection = peer_connections.borrow_mut().take(&remote_id)?;
            peer_echo_task(&remote_id, remote_peer_connection).await?;
            Ok::<_, anyhow::Error>(remote_id)
        }
    };

    // build signalling client
    let signalling_client = RtcSignallingClientBuilder::default().build(addr.to_string())?;
    let mut signalling_peer = signalling_client.start_peer().await?;
    // set callbacks
    signalling_peer
        .set_on_create_offer(create_offer_fn)
        .set_on_receive_answer(receive_answer_fn)
        .set_on_local_sig_cplt(local_sig_cplt_fn)
        .set_on_receive_offer(receive_offer_fn)
        .set_on_remote_sig_cplt(remote_sig_cplt_fn);

    // run signalling peer
    loop {
        signalling_peer.step().await?;
    }
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
