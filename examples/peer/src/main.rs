use std::{collections::HashMap, pin::Pin, sync::Arc};

use anyhow::{anyhow, Result};
use futures::{stream::FuturesUnordered, Future, StreamExt};
use log::{debug, info};
use neshi::{client::{read::NodeReadConnection, write::NodeWriteConnection, NodeClient}, metadata::pb::NodeMeta, state::pb::NodeState};
use protocol::{metadata::pb::SignallingMetadata, state::pb::{RtcSignal, SignallingState}, FILE_DESCRIPTOR_SET, NESHI_NATIVE_SERVER_ADDR};

use just_webrtc::{platform::PeerConnection, DataChannelConfig, RTCIceCandidateInit, RTCSessionDescription};

const ECHO_REQUEST: &str = "I'm literally Ryan Gosling.";
const ECHO_RESPONSE: &str = "I know right! He's literally me.";

/// This task monitors a neshi node for requested signalling states
/// Upon receiving a state, signalling is handled to create a remote peer connection.
/// All remote peer connections and the write connection are then returned to create echo tasks and requeue
async fn remote_peer_connections_task(
    mut write_connection: NodeWriteConnection,
) -> Result<TaskReturnType> {
    // handle offers from peers
    write_connection.receive_req_state().await?;
    let node_state = write_connection.fetch_clear_req_state();
    let mut state: SignallingState = node_state.to_state()?;

    let mut peer_connections = vec![];
    for (_remote_id, signal) in state.signals.iter_mut() {
        if signal.answer.is_some() {
            continue;
        }
        // decode remote offer and candidates
        let remote_offer: RTCSessionDescription = bincode::deserialize(&signal.offer)?;
        let remote_candidates: Vec<RTCIceCandidateInit> = bincode::deserialize(&signal.candidates)?;
        // create remote peer connection
        let mut peer_connection = just_webrtc::PeerConnectionBuilder::new()
            .with_remote_offer(Some(remote_offer))
            .build().await?;
        peer_connection.add_ice_candidates(remote_candidates).await?;
        // encode answer and updated candidates
        let answer = peer_connection.get_local_description().await
            .ok_or(anyhow!("could not get local description!"))?;
        let answer = bincode::serialize(&answer)?;
        let candidates = peer_connection.collect_ice_candidates().await;
        let candidates = bincode::serialize(&candidates)?;
        // set answer and updated candidates
        signal.answer = Some(answer);
        signal.candidates = candidates;
        peer_connections.push(peer_connection);
    }

    write_connection.commit_set_state(NodeState::new(&state))?;
    write_connection.send_clear_set_state().await?;

    Ok(TaskReturnType::RemotePeerConnections((write_connection, peer_connections)))
}

/// This task awaits a neshi node for set signalling states
/// Upon receiving a state, signalling is  handled to create a local peer connection.
/// The local peer connection is returned for creating an echo task.
async fn local_peer_connection_task(
    remote_node_id: u64,
    local_node_id: u64,
    mut read_connection: NodeReadConnection,
) -> Result<TaskReturnType> {
    let channel_options = vec![
        (format!("rtc_channel_{}_to_{}_", local_node_id, remote_node_id), DataChannelConfig::default())
    ];

    let mut local_peer_connection = just_webrtc::PeerConnectionBuilder::new()
        .with_channel_options(channel_options)
        .build().await?;

    let offer = local_peer_connection.get_local_description().await
        .ok_or(anyhow!("could not get local description!"))?;
    let offer = bincode::serialize(&offer)?;
    let candidates = local_peer_connection.collect_ice_candidates().await;
    let candidates = bincode::serialize(&candidates)?;

    let signal = RtcSignal {
        offer,
        candidates,
        answer: None
    };

    // send signal containing offer and candidates
    let sig_state = SignallingState {
        signals: HashMap::from([(local_node_id, signal)])
    };
    read_connection.commit_req_state(NodeState::new(&sig_state))?;
    read_connection.send_clear_req_state().await?;

    // receive signal containing answer and candidates
    let (answer, candidates) = loop {
        read_connection.receive_set_state().await?;
        let sig_state: SignallingState = read_connection.fetch_clear_set_state().to_state()?;
        if let Some(signal) = sig_state.signals.get(&local_node_id) {
            if let Some(answer) = &signal.answer {
                let answer: RTCSessionDescription = bincode::deserialize(&answer)?;
                let candidates: Vec<RTCIceCandidateInit> = bincode::deserialize(&signal.candidates)?;
                break (answer, candidates)
            }
        }
    };
    local_peer_connection.set_remote_description(answer).await?;
    local_peer_connection.add_ice_candidates(candidates).await?;

    Ok(TaskReturnType::LocalPeerConnection(local_peer_connection))
}

async fn peer_echo_task(
    mut peer_connection: PeerConnection,
) -> Result<TaskReturnType> {
    peer_connection.wait_peer_connected().await?;
    let mut channel = peer_connection.receive_channel().await.unwrap();
    channel.wait_ready().await?;
    // prepare echo request/response
    let echo_request = bincode::serialize(ECHO_REQUEST)?.into();
    let echo_response = bincode::serialize(ECHO_RESPONSE)?.into();
    // task loop
    loop {
        // echo request
        channel.send(&echo_request).await?;
        // echo response
        if let Some(bytes) = channel.receive().await {
            let s: &str = bincode::deserialize(&bytes)?;
            if s == ECHO_REQUEST {
                info!("Received request! Sending response.");
                channel.send(&echo_response).await?;
            }
        } else {
            break Err(anyhow!("remote channel closed!"));
        }
    }?;
    Ok(TaskReturnType::Echo)
}

enum TaskReturnType {
    Echo,
    LocalPeerConnection(PeerConnection),
    RemotePeerConnections((NodeWriteConnection, Vec<PeerConnection>))
}

async fn run_peer() -> Result<()> {
    let neshi_client = Arc::new(NodeClient::new(format!("http://{NESHI_NATIVE_SERVER_ADDR}")));
    // get existing nodes from neshi server
    let nodes = neshi_client.get_nodes().await?;
    // advertise self as node on neshi server
    let metadata = SignallingMetadata {};
    let neshi_node_metadata = NodeMeta::new::<SignallingMetadata, SignallingState>(FILE_DESCRIPTOR_SET, &metadata)?;
    let neshi_local_node_id = neshi_client.register_node(neshi_node_metadata.clone()).await?;

    // create unordered tasks queue
    let mut tasks = FuturesUnordered::<Pin<Box<dyn Future<Output = Result<TaskReturnType>>>>>::new();

    // queue local peer connection tasks, to make offers and receive answers from older nodes...
    for node in nodes {
        if let Some(node_meta) = node.node_meta {
            let neshi_client = neshi_client.clone();
            let task = Box::pin(async move {
                let read_connection = NodeReadConnection::connect_node(node.node_id, &node_meta, &neshi_client).await?;
                local_peer_connection_task(node.node_id, neshi_local_node_id, read_connection).await
            });
            tasks.push(task);
        }
    }
    // queue remote peer connections task, to receive offers and send answers to younger nodes...
    let write_connection = NodeWriteConnection::connect_node(
        neshi_local_node_id,
        &neshi_node_metadata,
        &neshi_client
    ).await?;
    tasks.push(Box::pin(remote_peer_connections_task(write_connection)));

    // run tasks concurrently
    while let Some(result) = tasks.next().await {
        match result? {
            TaskReturnType::Echo => { debug!("echo task complete!?") }
            TaskReturnType::LocalPeerConnection(peer_connection) => {
                // queue echo task for local peer connection
                tasks.push(Box::pin(peer_echo_task(peer_connection)));
            },
            TaskReturnType::RemotePeerConnections((write_connection, peer_connections)) => {
                // requeue the remote peer connections task
                tasks.push(Box::pin(remote_peer_connections_task(write_connection)));
                // queue echo tasks for remote peer connections
                for peer_connection in peer_connections {
                    tasks.push(Box::pin(peer_echo_task(peer_connection)));
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    pretty_env_logger::try_init()?;
    run_peer().await
}