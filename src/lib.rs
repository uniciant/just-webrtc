use std::{marker::PhantomData, sync::Arc};

use bytes::Bytes;
use platform::Platform;

use tokio::sync::{mpsc::{error::TryRecvError, UnboundedReceiver}, watch};

pub mod platform;
pub mod types;

use types::{DataChannelOptions, ICECandidate, ICEServer, PeerConfiguration, PeerConnectionState, SessionDescription};

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
    peer_connection_state_rx: watch::Receiver<PeerConnectionState>,
    channels_rx: UnboundedReceiver<Channel<U>>,
    candidate_rx: UnboundedReceiver<Option<ICECandidate>>,
}

impl<T, U> GenericPeerConnection<T, U> {
    pub fn is_offerer(&self) -> bool {
        self.is_offerer
    }

    pub async fn wait_peer_connected(&mut self) -> Result<(), watch::error::RecvError> {
        let _ = self.peer_connection_state_rx.wait_for(|s| s == &PeerConnectionState::Connected).await?;
        Ok(())
    }

    pub async fn receive_channel(&mut self) -> Option<Channel<U>> {
        self.channels_rx.recv().await
    }

    pub fn try_receive_channel(&mut self) -> Result<Channel<U>, TryRecvError> {
        self.channels_rx.try_recv()
    }

    /// Collect all ICE candidates for the current negotiation
    pub async fn collect_ice_candidates(&mut self) -> Vec<ICECandidate> {
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
pub enum PeerConnectionBuilderError {
    #[error("remote offer is mutually exclusive with channel settings!")]
    ConflictingBuildOptions,
}

pub struct PeerConnectionBuilder<P: Platform> {
    _platform: PhantomData<P>,
    config: PeerConfiguration,
    outgoing_buffer: usize,
    remote_offer: Option<SessionDescription>,
    channel_options: Vec<(String, DataChannelOptions)>
}

impl<P: Platform> Default for PeerConnectionBuilder<P> {
    fn default() -> Self {
        let config = PeerConfiguration {
            ice_servers: vec![ICEServer {
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
    pub fn set_config(mut self, set: PeerConfiguration) -> Self {
        self.config = set;
        self
    }

    /// Specify outgoing data channel buffer size
    pub fn set_outgoing_buffer(mut self, set: usize) -> Self {
        self.outgoing_buffer = set;
        self
    }

    /// Provide an Offer as created by a remote peer
    ///
    /// This option is mutually exclusive with the `with_channel_` settings.
    pub fn with_remote_offer(mut self, set: Option<SessionDescription>) -> Result<Self, PeerConnectionBuilderError> {
        if self.channel_options.is_empty() {
            self.remote_offer = set;
            Ok(self)
        } else {
            Err(PeerConnectionBuilderError::ConflictingBuildOptions)
        }
    }

    /// Provide options for initial data channel creation (as an offerer)
    ///
    /// This option is mutually exclusive with `with_remote_offer`.
    pub fn with_channel_options(mut self, set: Vec<(String, DataChannelOptions)>) -> Result<Self, PeerConnectionBuilderError> {
        if self.remote_offer.is_none() {
            self.channel_options = set;
            Ok(self)
        } else {
            Err(PeerConnectionBuilderError::ConflictingBuildOptions)
        }
    }
}
