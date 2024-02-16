use std::{future::Future, pin::Pin, sync::{Arc, Mutex}};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use log::{debug, error, trace};
use tokio::sync::mpsc::UnboundedSender;

use webrtc::{
    api::APIBuilder, data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, RTCDataChannel}, ice_transport::{ice_candidate::{RTCIceCandidate, RTCIceCandidateInit}, ice_connection_state::RTCIceConnectionState, ice_gatherer_state::RTCIceGathererState}, peer_connection::{peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}, Error
};

use crate::{Channel, DataChannelConfig, PeerConnection, PeerConnectionBuilder};
use super::Platform;

pub struct Native {}
impl Platform for Native {}

fn handle_peer_connection_state_change(
    state: RTCPeerConnectionState,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    if state == RTCPeerConnectionState::Failed {
        println!("Peer connection failed");
    } else {
        println!("Peer connection state has changed: {state}");
    }
    Box::pin(async {})
}

fn handle_data_channel(
    channel: Arc<RTCDataChannel>,
    local_channels: Arc<Mutex<Vec<Arc<Channel<Arc<RTCDataChannel>>>>>>,
) {
    let label = channel.label().to_string();
    let id = channel.id();
    let (incoming_tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    println!("New data channel ({label}:{id})");
    // register data channel handlers
    let (label_close, label_open, label_message) = (label.clone(), label.clone(), label.clone());
    channel.on_close(Box::new(move || handle_data_channel_close(&label_close, &id)));
    channel.on_open(Box::new(move || handle_data_channel_open(&label_open, &id)));
    channel.on_message(Box::new(move |message| handle_data_channel_message(&label_message, &id, &incoming_tx, message)));
    channel.on_error(Box::new(move |error| handle_data_channel_error(&label, &id, error)));
    // push channel & receiver to list
    let mut channels = local_channels.lock().unwrap();
    let channel = Channel { inner: channel, rx };
    channels.push(Arc::new(channel));
}

fn handle_data_channel_close(
    label: &str,
    id: &u16
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    debug!("Data channel closed ({label}:{id}");
    Box::pin(async {})
}

fn handle_data_channel_open(
    label: &str,
    id: &u16,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    debug!("Data channel open ({label}:{id})");
    Box::pin(async move {})
}

fn handle_data_channel_message(
    label: &str,
    id: &u16,
    incoming_tx: &UnboundedSender<Bytes>,
    message: DataChannelMessage
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    trace!("Data channel received message ({label}:{id})");
    if let Err(e) = incoming_tx.send(message.data) {
        error!("incoming mpsc error {e}");
    }
    Box::pin(async move {})
}

fn handle_data_channel_error(
    label: &str,
    id: &u16,
    error: Error,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    println!("Internal error on data channel ({label}:{id}). Error ({error})");
    Box::pin(async {})
}

fn handle_ice_connection_state_change(
    state: RTCIceConnectionState,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    println!("ICE connection state has changed: {state}");
    if state == RTCIceConnectionState::Failed {
        println!("ICE connection failed.");
    }
    Box::pin(async {})
}

fn handle_ice_gathering_state_change(
    state: RTCIceGathererState,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    println!("ICE gathering state has changed: {state}");
    Box::pin(async {})
}

fn handle_ice_candidate(
    candidate: Option<RTCIceCandidate>,
    candidate_tx: &UnboundedSender<RTCIceCandidateInit>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    if let Some(candidate) = candidate {
        match candidate.to_json() {
            Ok(candidate_init) => {
                if let Err(e) = candidate_tx.send(candidate_init) {
                    println!("candidate channel error ({e})");
                }
            },
            Err(e) => {
                println!("failed to serialise ice candidate ({e})");
            }
        }
    } else {
        println!("ICE gathering finished.")
    }
    Box::pin(async {})
}

impl Channel<Arc<RTCDataChannel>> {
    pub async fn send(&self, data: &Bytes) -> Result<usize> {
        Ok(self.inner.send(data).await?)
    }
}

impl PeerConnection<RTCPeerConnection, Arc<RTCDataChannel>> {
    pub async fn get_local_description(&self) -> Option<RTCSessionDescription> {
        self.connection.local_description().await
    }

    pub async fn add_ice_candidate(&self, remote_candidate_init: RTCIceCandidateInit) -> Result<()> {
        // add remote ICE candidate
        self.connection.add_ice_candidate(remote_candidate_init).await?;
        Ok(())
    }
}

impl PeerConnectionBuilder<Native> {
    pub async fn build(&self) -> Result<PeerConnection<RTCPeerConnection, Arc<RTCDataChannel>>> {
        // validate builder
        if self.remote_offer.is_some() && !self.channel_options.is_empty() {
            return Err(anyhow!("remote offer is mutually exclusive with channel settings"));
        }

        // create new connection from the api and config
        let api = APIBuilder::default().build();
        let connection: Arc<RTCPeerConnection> = Arc::new(api.new_peer_connection(self.config.clone()).await?);

        // create mpsc channels for passing info to/from handlers
        let (candidate_tx, candidate_rx) = tokio::sync::mpsc::unbounded_channel::<RTCIceCandidateInit>();
        // register state change handler
        connection.on_peer_connection_state_change(Box::new(move |state| handle_peer_connection_state_change(state)));
        // register ice related handlers
        connection.on_ice_connection_state_change(Box::new(move |state| handle_ice_connection_state_change(state)));
        connection.on_ice_gathering_state_change(Box::new(move |state| handle_ice_gathering_state_change(state)));
        connection.on_ice_candidate(Box::new(move |candidate| handle_ice_candidate(candidate, &candidate_tx)));

        let channels: Arc<Mutex<Vec<Arc<Channel<Arc<RTCDataChannel>>>>>> = Arc::new(Mutex::new(vec![]));
        // if an offer is provided, we are an answerer and are receiving the data channel
        // otherwise, we are an offerer and must configure and create the data channel
        let (desc, is_offerer) = if let Some(offer) = self.remote_offer.clone() {
            let channels_ref = channels.clone();
            connection.on_data_channel(Box::new(move |incoming_channel| {
                handle_data_channel(incoming_channel, channels_ref.clone());
                Box::pin(async move {})
            }));
            // set the remote SessionDescription (provided by remote peer via external signalling)
            connection.set_remote_description(offer).await?;
            // create answer
            let answer = connection.create_answer(None).await?;
            (answer, false)
        } else {
            for (index, (label_prefix, channel_options)) in self.channel_options.iter().enumerate() {
                let options = Some(RTCDataChannelInit::from(channel_options.clone()));
                // create channels from options (we are offering)
                let data_channel = connection.create_data_channel(&format!("{label_prefix}{index}"), options).await?;
                handle_data_channel(data_channel, channels.clone());
            }
            // create offer
            let offer = connection.create_offer(None).await?;
            (offer, true)
        };

        // sets the local SessionDescription (offer/answer) and start the UDP listeners
        connection.set_local_description(desc).await?;

        Ok(PeerConnection { is_offerer, connection, channels, candidate_rx })
    }
}

impl From<DataChannelConfig> for RTCDataChannelInit {
    fn from(value: DataChannelConfig) -> Self {
        Self {
            protocol: value.protocol,
            ordered: value.ordered,
            max_packet_life_time: value.max_packet_life_time,
            max_retransmits: value.max_retransmits,
            negotiated: value.negotiated,
        }
    }
}
