use std::marker::PhantomData;

use bytes::Bytes;
use platform::Platform;

pub mod platform;
pub mod types;

use types::{DataChannelOptions, ICECandidate, ICEServer, PeerConfiguration, SessionDescription};

#[cfg_attr(not(target_arch = "wasm32"), trait_variant::make(Send))]
pub trait DataChannelExt<E> {
    async fn wait_ready(&mut self) -> Result<(), E>;

    async fn receive(&mut self) -> Option<Bytes>;

    async fn send(&self, data: &Bytes) -> Result<usize, E>;

    fn try_receive(&mut self) -> Result<Bytes, E>;

    fn try_send(&self, data: &Bytes) -> Result<usize, E>;

    fn id(&self) -> u16;

    fn label(&self) -> String;
}

#[cfg_attr(not(target_arch = "wasm32"), trait_variant::make(Send))]
pub trait PeerConnectionExt<C, E> {
    async fn wait_peer_connected(&mut self) -> Result<(), E>;

    async fn receive_channel(&mut self) -> Option<C>;

    async fn collect_ice_candidates(&mut self) -> Vec<ICECandidate>;

    async fn get_local_description(&self) -> Option<SessionDescription>;

    async fn add_ice_candidates(&self, remote_candidates: Vec<ICECandidate>) -> Result<(), E>;

    async fn set_remote_description(&self, remote_answer: SessionDescription) -> Result<(), E>;

    fn is_offerer(&self) -> bool;

    fn try_receive_channel(&mut self) -> Result<C, E>;
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
