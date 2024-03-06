use std::{marker::PhantomData, sync::Arc};

use bytes::Bytes;
use platform::Platform;
use serde::{Deserialize, Serialize};

use tokio::sync::{mpsc::{error::TryRecvError, UnboundedReceiver}, watch};
pub use webrtc::{
    ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer, ice_gatherer_state::RTCIceGathererState},
    peer_connection::{configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription, peer_connection_state::RTCPeerConnectionState}
};

pub mod platform;

/// DataChannelConfig can be used to configure properties of the underlying
/// channel such as data reliability.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct DataChannelConfig {
    /// ordered indicates if data is allowed to be delivered out of order. The
    /// default value of true, guarantees that data will be delivered in order.
    pub ordered: Option<bool>,

    /// max_packet_life_time limits the time (in milliseconds) during which the
    /// channel will transmit or retransmit data if not acknowledged. This value
    /// may be clamped if it exceeds the maximum value supported.
    pub max_packet_life_time: Option<u16>,

    /// max_retransmits limits the number of times a channel will retransmit data
    /// if not successfully delivered. This value may be clamped if it exceeds
    /// the maximum value supported.
    pub max_retransmits: Option<u16>,

    /// protocol describes the subprotocol name used for this channel.
    pub protocol: Option<String>,

    /// negotiated describes if the data channel is created by the local peer or
    /// the remote peer. The default value of None tells the user agent to
    /// announce the channel in-band and instruct the other peer to dispatch a
    /// corresponding DataChannel. If set to Some(id), it is up to the application
    /// to negotiate the channel and create an DataChannel with the same id
    /// at the other peer.
    pub negotiated: Option<u16>,
}

pub struct Channel<T> {
    inner: T,
    ready_state_rx: watch::Receiver<bool>,
    rx: UnboundedReceiver<Bytes>,
}

impl<T> Channel<T> {
    pub async fn wait_ready(&mut self) -> Result<(), watch::error::RecvError> {
        let _ = self.ready_state_rx.wait_for(|s| s == &true).await?;
        Ok(())
    }

    pub async fn receive(&mut self) -> Option<Bytes> {
        self.rx.recv().await
    }

    pub fn try_receive(&mut self) -> Result<Bytes, TryRecvError> {
        self.rx.try_recv()
    }
}

pub struct GenericPeerConnection<T, U> {
    is_offerer: bool,
    connection: Arc<T>,
    peer_connection_state_rx: watch::Receiver<RTCPeerConnectionState>,
    channels_rx: UnboundedReceiver<Channel<U>>,
    candidate_rx: UnboundedReceiver<Option<RTCIceCandidateInit>>,
}

impl<T, U> GenericPeerConnection<T, U> {
    pub fn is_offerer(&self) -> bool {
        self.is_offerer
    }

    pub async fn wait_peer_connected(&mut self) -> Result<(), watch::error::RecvError> {
        let _ = self.peer_connection_state_rx.wait_for(|s| s == &RTCPeerConnectionState::Connected).await?;
        Ok(())
    }

    pub async fn receive_channel(&mut self) -> Option<Channel<U>> {
        self.channels_rx.recv().await
    }

    pub fn try_receive_channel(&mut self) -> Result<Channel<U>, TryRecvError> {
        self.channels_rx.try_recv()
    }

    /// Collect all ICE candidates for the current negotiation
    pub async fn collect_ice_candidates(&mut self) -> Vec<RTCIceCandidateInit> {
        let mut candidate_inits = vec![];
        loop {
            match self.candidate_rx.recv().await {
                Some(Some(candidate_init)) => candidate_inits.push(candidate_init),
                _ => return candidate_inits,
            }
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum PeerConnectionBuilderError<E> {
    #[error("remote offer is mutually exclusive with channel settings!")]
    ConflictingBuildOptions,
    #[error("webrtc error ({0})")]
    WebRTCError(#[from] E)
}

pub struct PeerConnectionBuilder<P: Platform> {
    _platform: PhantomData<P>,
    config: RTCConfiguration,
    outgoing_buffer: usize,
    remote_offer: Option<RTCSessionDescription>,
    channel_options: Vec<(String, DataChannelConfig)>
}

impl<P: Platform> Default for PeerConnectionBuilder<P> {
    fn default() -> Self {
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls: vec![
                    "stun:stun.l.google.com:19302".to_string(),
                    "stun:stun1.l.google.com:19302".to_string(),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let outgoing_buffer = 16;

        Self {
            _platform: PhantomData,
            config,
            outgoing_buffer,
            remote_offer: None,
            channel_options: vec![],
        }
    }
}

impl<P: Platform> PeerConnectionBuilder<P> {
    /// Create new PeerConnectionBuilder (equivalent to PeerConnectionBuilder::default())
    pub fn new() -> Self {
        Self::default()
    }

    /// Specify internal WebRTC peer configuration settings
    pub fn set_config(&mut self, set: RTCConfiguration) -> &Self {
        self.config = set;
        self
    }

    /// Specify outgoing data channel buffer size
    pub fn set_outgoing_buffer(&mut self, set: usize) -> &Self {
        self.outgoing_buffer = set;
        self
    }

    /// Provide an Offer as created by a remote peer
    ///
    /// This option is mutually exclusive with the `with_channel_` settings.
    pub fn with_remote_offer(&mut self, set: Option<RTCSessionDescription>) -> &Self {
        self.remote_offer = set;
        self
    }

    /// Provide options for initial data channel creation (as an offerer)
    ///
    /// This option is mutually exclusive with `with_remote_offer`.
    pub fn with_channel_options(&mut self, set: Vec<(String, DataChannelConfig)>) -> &Self {
        self.channel_options = set;
        self
    }
}
