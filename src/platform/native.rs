use std::sync::Arc;

use bytes::Bytes;
use log::{debug, error, trace};
use tokio::sync::{mpsc::UnboundedSender, watch};
use webrtc::{
    api::APIBuilder,
    data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, RTCDataChannel},
    ice_transport::{ice_candidate::{RTCIceCandidate, RTCIceCandidateInit}, ice_connection_state::RTCIceConnectionState, ice_credential_type::RTCIceCredentialType, ice_gatherer_state::RTCIceGathererState, ice_server::RTCIceServer},
    peer_connection::{configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, policy::{bundle_policy::RTCBundlePolicy, ice_transport_policy::RTCIceTransportPolicy, rtcp_mux_policy::RTCRtcpMuxPolicy}, sdp::{sdp_type::RTCSdpType, session_description::RTCSessionDescription}, RTCPeerConnection},
};

use crate::{types::{BundlePolicy, ICECandidate, ICECredentialType, ICEServer, ICETransportPolicy, PeerConfiguration, PeerConnectionState, RTCPMuxPolicy, SDPType, SessionDescription}, Channel, DataChannelOptions, GenericPeerConnection, PeerConnectionBuilder};
use super::Platform;

pub struct Native {}
impl Platform for Native {}

fn handle_peer_connection_state_change(
    state: RTCPeerConnectionState,
    peer_state_tx: &watch::Sender<PeerConnectionState>
) {
    if state == RTCPeerConnectionState::Failed {
        error!("Peer connection failed");
    } else {
        debug!("Peer connection state has changed: {state}");
    }
    if let Err(e) = peer_state_tx.send(state.into()) {
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

fn handle_data_channel_error(label: &str, id: &u16, error: webrtc::Error) {
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
    candidate_tx: &UnboundedSender<Option<ICECandidate>>,
) {
    let message = if let Some(candidate) = candidate {
        match candidate.to_json() {
            Ok(candidate_init) => {
                Some(candidate_init.into())
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
    pub fn id(&self) -> u16 {
        self.inner.id()
    }

    pub fn label(&self) -> String {
        self.inner.label().to_string()
    }

    pub async fn send(&self, data: &Bytes) -> Result<usize, webrtc::Error> {
        self.inner.send(data).await
    }
}

pub type PeerConnection = GenericPeerConnection<RTCPeerConnection, Arc<RTCDataChannel>>;

impl PeerConnection {
    pub async fn get_local_description(&self) -> Option<SessionDescription> {
        self.connection.local_description().await.map(|desc| desc.into())
    }

    pub async fn add_ice_candidates(&self, remote_candidates: Vec<ICECandidate>) -> Result<(), webrtc::Error> {
        // add remote ICE candidates
        for candidate in remote_candidates {
            self.connection.add_ice_candidate(candidate.into()).await?;
        }
        Ok(())
    }

    pub async fn set_remote_description(&self, remote_answer: SessionDescription) -> Result<(), webrtc::Error> {
        Ok(self.connection.set_remote_description(remote_answer.try_into()?).await?)
    }
}

impl PeerConnectionBuilder<Native> {
    pub async fn build(&self) -> Result<PeerConnection, webrtc::Error> {
        // create new connection from the api and config
        let api = APIBuilder::default().build();

        let connection: Arc<RTCPeerConnection> = Arc::new(api.new_peer_connection(self.config.clone().into()).await?);

        // create mpsc channels for passing info to/from handlers
        let (candidate_tx, candidate_rx) = tokio::sync::mpsc::unbounded_channel::<Option<ICECandidate>>();
        let (channels_tx, channels_rx) = tokio::sync::mpsc::unbounded_channel::<Channel<Arc<RTCDataChannel>>>();
        let (peer_connection_state_tx, peer_connection_state_rx) = tokio::sync::watch::channel(PeerConnectionState::New);

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
            connection.set_remote_description(offer.try_into()?).await?;
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

// Convert from just_webrtc types to native webrtc types

impl From<DataChannelOptions> for RTCDataChannelInit {
    fn from(value: DataChannelOptions) -> Self {
        Self {
            protocol: value.protocol,
            ordered: value.ordered,
            max_packet_life_time: value.max_packet_life_time,
            max_retransmits: value.max_retransmits,
            negotiated: value.negotiated,
        }
    }
}

impl From<SDPType> for RTCSdpType {
    fn from(value: SDPType) -> Self {
        match value {
            SDPType::Answer => Self::Answer,
            SDPType::Offer => Self::Offer,
            SDPType::Pranswer => Self::Pranswer,
            SDPType::Rollback => Self::Rollback,
        }
    }
}

impl From<RTCSdpType> for SDPType {
    fn from(value: RTCSdpType) -> Self {
        match value {
            RTCSdpType::Answer => Self::Answer,
            RTCSdpType::Offer => Self::Offer,
            RTCSdpType::Pranswer => Self::Pranswer,
            RTCSdpType::Rollback => Self::Rollback,
            RTCSdpType::Unspecified => Self::Answer,
        }
    }
}

impl TryFrom<SessionDescription> for RTCSessionDescription {
    type Error = webrtc::Error;

    fn try_from(value: SessionDescription) -> Result<Self, Self::Error> {
        match value.sdp_type {
            SDPType::Answer => RTCSessionDescription::answer(value.sdp),
            SDPType::Offer => RTCSessionDescription::offer(value.sdp),
            SDPType::Pranswer => RTCSessionDescription::pranswer(value.sdp),
            SDPType::Rollback => {
                let mut out = RTCSessionDescription::default();
                out.sdp = value.sdp;
                out.sdp_type = value.sdp_type.into();
                Ok(out)
            },
        }
    }
}

impl From<RTCSessionDescription> for SessionDescription {
    fn from(value: RTCSessionDescription) -> Self {
        Self {
            sdp: value.sdp,
            sdp_type: value.sdp_type.into()
        }
    }
}

impl From<BundlePolicy> for RTCBundlePolicy {
    fn from(value: BundlePolicy) -> Self {
        match value {
            BundlePolicy::Balanced => Self::Balanced,
            BundlePolicy::MaxBundle => Self::MaxBundle,
            BundlePolicy::MaxCompat => Self::MaxCompat,
        }
    }
}

impl From<ICETransportPolicy> for RTCIceTransportPolicy {
    fn from(value: ICETransportPolicy) -> Self {
        match value {
            ICETransportPolicy::All => Self::All,
            ICETransportPolicy::Relay => Self::Relay,
        }
    }
}

impl From<RTCPMuxPolicy> for RTCRtcpMuxPolicy {
    fn from(value: RTCPMuxPolicy) -> Self {
        match value {
            RTCPMuxPolicy::Negotiate => Self::Negotiate,
            RTCPMuxPolicy::Require => Self::Require,
        }
    }
}

impl From<ICECredentialType> for RTCIceCredentialType {
    fn from(value: ICECredentialType) -> Self {
        match value {
            ICECredentialType::Oauth => Self::Oauth,
            ICECredentialType::Password => Self::Password,
        }
    }
}

impl From<ICEServer> for RTCIceServer {
    fn from(value: ICEServer) -> Self {
        Self {
            credential: value.credential,
            credential_type: value.credential_type.into(),
            urls: value.urls,
            username: value.username,
        }
    }
}

impl From<PeerConfiguration> for RTCConfiguration {
    fn from(value: PeerConfiguration) -> Self {
        Self {
            ice_candidate_pool_size: value.ice_candidate_pool_size,
            peer_identity: value.peer_identity,
            bundle_policy: value.bundle_policy.into(),
            ice_transport_policy: value.ice_transport_policy.into(),
            rtcp_mux_policy: value.rtcp_mux_policy.into(),
            ice_servers: value.ice_servers.into_iter().map(|a| a.into()).collect(),
            ..Default::default()
        }
    }
}

impl From<RTCPeerConnectionState> for PeerConnectionState {
    fn from(value: RTCPeerConnectionState) -> Self {
        match value {
            RTCPeerConnectionState::Closed => Self::Closed,
            RTCPeerConnectionState::Connected => Self::Connected,
            RTCPeerConnectionState::Connecting => Self::Connecting,
            RTCPeerConnectionState::Disconnected => Self::Disconnected,
            RTCPeerConnectionState::Failed => Self::Failed,
            RTCPeerConnectionState::New => Self::New,
            RTCPeerConnectionState::Unspecified => Self::New,
        }
    }
}

impl From<RTCIceCandidateInit> for ICECandidate {
    fn from(value: RTCIceCandidateInit) -> Self {
        Self {
            candidate: value.candidate,
            sdp_mid: value.sdp_mid,
            sdp_mline_index: value.sdp_mline_index,
            username_fragment: value.username_fragment,
        }
    }
}

impl From<ICECandidate> for RTCIceCandidateInit {
    fn from(value: ICECandidate) -> Self {
        Self {
            candidate: value.candidate,
            sdp_mid: value.sdp_mid,
            sdp_mline_index: value.sdp_mline_index,
            username_fragment: value.username_fragment,
        }
    }
}
