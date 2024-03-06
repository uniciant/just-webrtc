use std::sync::Arc;

use bytes::Bytes;
use log::{debug, error, trace};
use tokio::sync::{mpsc::UnboundedSender, watch};
use webrtc::{
    api::APIBuilder, data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, data_channel_state::RTCDataChannelState, RTCDataChannel}, ice_transport::{ice_candidate::{RTCIceCandidate, RTCIceCandidateInit}, ice_connection_state::RTCIceConnectionState, ice_gatherer_state::RTCIceGathererState, ice_gathering_state::RTCIceGatheringState}, peer_connection::{peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}, Error
};

use crate::{Channel, DataChannelConfig, GenericPeerConnection, PeerConnectionBuilder, PeerConnectionBuilderError};
use super::Platform;

pub struct Native {}
impl Platform for Native {}

fn handle_peer_connection_state_change(
    state: RTCPeerConnectionState,
    peer_state_tx: &watch::Sender<RTCPeerConnectionState>
) {
    if state == RTCPeerConnectionState::Failed {
        error!("Peer connection failed");
    } else {
        debug!("Peer connection state has changed: {state}");
    }
    if let Err(e) = peer_state_tx.send(state) {
        error!("could not send peer connection state! ({e})");
    }
}

fn handle_data_channel(
    channel: Arc<RTCDataChannel>,
    channels_tx: &UnboundedSender<Channel<Arc<RTCDataChannel>>>,
) {
    let label = channel.label().to_string();
    let id = channel.id();

    // create mpsc channels for passing info to/from handlers
    let (incoming_tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    let (ready_state_tx, ready_state_rx) = tokio::sync::watch::channel(false);

    debug!("New data channel ({label}:{id})");
    // register data channel handlers
    let (label_close, label_open, label_message) = (label.clone(), label.clone(), label.clone());
    channel.on_close(Box::new(move || {
        handle_data_channel_close(&label_close, &id);
        Box::pin(async {})
    }));
    channel.on_open(Box::new(move || {
        handle_data_channel_open(&label_open, &id, &ready_state_tx);
        Box::pin(async {})
    }));
    channel.on_message(Box::new(move |message| {
        handle_data_channel_message(&label_message, &id, &incoming_tx, message);
        Box::pin(async {})
    }));
    channel.on_error(Box::new(move |error| {
        handle_data_channel_error(&label, &id, error);
        Box::pin(async {})
    }));

    // push channel & receiver to list
    let channel = Channel { inner: channel, rx, ready_state_rx };
    if let Err(e) = channels_tx.send(channel) {
        error!("could not send data channel! ({e})")
    }
}

fn handle_data_channel_close(label: &str, id: &u16) {
    debug!("Data channel closed ({label}:{id}");
}

fn handle_data_channel_open(label: &str, id: &u16, ready_state_tx: &watch::Sender<bool>) {
    debug!("Data channel open ({label}:{id})");
    if let Err(e) = ready_state_tx.send(true) {
        error!("could not send data channel ready state! ({e})");
    }
}

fn handle_data_channel_message(
    label: &str,
    id: &u16,
    incoming_tx: &UnboundedSender<Bytes>,
    message: DataChannelMessage
) {
    trace!("Data channel received message ({label}:{id})");
    if let Err(e) = incoming_tx.send(message.data) {
        error!("incoming mpsc error {e}");
    }
}

fn handle_data_channel_error(label: &str, id: &u16, error: Error) {
    error!("Internal error on data channel ({label}:{id}). Error ({error})");
}

fn handle_ice_connection_state_change(state: RTCIceConnectionState) {
    if state == RTCIceConnectionState::Failed {
        error!("ICE connection failed.");
    }
}

fn handle_ice_gathering_state_change(state: RTCIceGathererState) {
    debug!("ICE gathering state has changed: {state}");
}

fn handle_ice_candidate(
    candidate: Option<RTCIceCandidate>,
    candidate_tx: &UnboundedSender<Option<RTCIceCandidateInit>>,
) {
    let message = if let Some(candidate) = candidate {
        match candidate.to_json() {
            Ok(candidate_init) => {
                Some(candidate_init)
            },
            Err(e) => {
                error!("failed to serialise ice candidate ({e})");
                return;
            }
        }
    } else {
        debug!("ICE gathering finished.");
        None
    };
    if let Err(e) = candidate_tx.send(message) {
        error!("candidate channel error ({e})");
    }
}

impl Channel<Arc<RTCDataChannel>> {
    pub fn ready_state(&self) -> RTCDataChannelState {
        self.inner.ready_state()
    }

    pub async fn send(&self, data: &Bytes) -> Result<usize, webrtc::Error> {
        self.inner.send(data).await
    }
}

impl GenericPeerConnection<RTCPeerConnection, Arc<RTCDataChannel>> {
    pub fn connection_state(&self) -> RTCPeerConnectionState {
        self.connection.connection_state()
    }

    pub fn ice_gathering_state(&self) -> RTCIceGatheringState {

        self.connection.ice_gathering_state()
    }

    pub async fn get_local_description(&self) -> Option<RTCSessionDescription> {
        self.connection.local_description().await
    }

    pub async fn add_ice_candidates(&self, remote_candidates: Vec<RTCIceCandidateInit>) -> Result<(), webrtc::Error> {
        // add remote ICE candidate
        for candidate in remote_candidates {
            self.connection.add_ice_candidate(candidate).await?;
        }
        Ok(())
    }

    pub async fn set_remote_description(&self, remote_answer: RTCSessionDescription) -> Result<(), webrtc::Error> {
        Ok(self.connection.set_remote_description(remote_answer).await?)
    }
}

pub type PeerConnection = GenericPeerConnection<RTCPeerConnection, Arc<RTCDataChannel>>;

impl PeerConnectionBuilder<Native> {
    pub async fn build(&self) -> Result<PeerConnection, PeerConnectionBuilderError<webrtc::Error>> {
        // validate builder
        if self.remote_offer.is_some() && !self.channel_options.is_empty() {
            return Err(PeerConnectionBuilderError::ConflictingBuildOptions);
        }

        // create new connection from the api and config
        let api = APIBuilder::default().build();
        let connection: Arc<RTCPeerConnection> = Arc::new(api.new_peer_connection(self.config.clone()).await?);

        // create mpsc channels for passing info to/from handlers
        let (candidate_tx, candidate_rx) = tokio::sync::mpsc::unbounded_channel::<Option<RTCIceCandidateInit>>();
        let (channels_tx, channels_rx) = tokio::sync::mpsc::unbounded_channel::<Channel<Arc<RTCDataChannel>>>();
        let (peer_connection_state_tx, peer_connection_state_rx) = tokio::sync::watch::channel(RTCPeerConnectionState::default());

        // register state change handler
        connection.on_peer_connection_state_change(Box::new(move |state| {
            handle_peer_connection_state_change(state, &peer_connection_state_tx);
            Box::pin(async {})
        }));
        // register ice related handlers
        connection.on_ice_connection_state_change(Box::new(move |state| {
            handle_ice_connection_state_change(state);
            Box::pin(async {})
        }));
        connection.on_ice_gathering_state_change(Box::new(move |state| {
            handle_ice_gathering_state_change(state);
            Box::pin(async {})
        }));
        connection.on_ice_candidate(Box::new(move |candidate| {
            handle_ice_candidate(candidate, &candidate_tx);
            Box::pin(async {})
        }));

        // if an offer is provided, we are an answerer and are receiving the data channel
        // otherwise, we are an offerer and must configure and create the data channel
        let (desc, is_offerer) = if let Some(offer) = self.remote_offer.clone() {
            connection.on_data_channel(Box::new(move |incoming_channel| {
                handle_data_channel(incoming_channel, &channels_tx);
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
                handle_data_channel(data_channel, &channels_tx);
            }
            // create offer
            let offer = connection.create_offer(None).await?;
            (offer, true)
        };

        // sets the local SessionDescription (offer/answer) and start the UDP listeners
        connection.set_local_description(desc).await?;

        Ok(PeerConnection { is_offerer, connection, peer_connection_state_rx, channels_rx, candidate_rx })
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
