#![doc = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))]
#![warn(missing_docs)]
#![warn(missing_debug_implementations)]
#![warn(dead_code)]
#![cfg_attr(test, allow(unused_crate_dependencies))]

use std::marker::PhantomData;

use bytes::Bytes;

pub mod platform;
pub mod types;

use platform::{Channel, Error, PeerConnection, Platform};
use types::{DataChannelOptions, ICECandidate, ICEServer, PeerConfiguration, SessionDescription};

#[cfg_attr(not(target_arch = "wasm32"), trait_variant::make(Send))]
#[cfg_attr(target_arch = "wasm32", allow(async_fn_in_trait))]
/// **Platform agnostic WebRTC DataChannel API**
pub trait DataChannelExt {
    /// Wait for the data channel to become open and ready to transfer data
    async fn wait_ready(&mut self);
    /// Receive data from the channel
    async fn receive(&mut self) -> Result<Bytes, Error>;
    /// Send data to the channel
    async fn send(&self, data: &Bytes) -> Result<usize, Error>;
    /// Get the unique ID of the channel
    fn id(&self) -> u16;
    /// Get the assigned label of the channel
    fn label(&self) -> String;
}

#[cfg_attr(not(target_arch = "wasm32"), trait_variant::make(Send))]
#[cfg_attr(target_arch = "wasm32", allow(async_fn_in_trait))]
/// **Platform agnostic WebRTC PeerConnection API**
pub trait PeerConnectionExt {
    /// Wait for the peer connection to become connected
    async fn wait_peer_connected(&mut self);
    /// Receive a data channel from the peer connection
    async fn receive_channel(&mut self) -> Result<Channel, Error>;
    /// Collect all ICE candidates from the peer connection
    async fn collect_ice_candidates(&mut self) -> Result<Vec<ICECandidate>, Error>;
    /// Get the local `SessionDescription` of the peer connection
    async fn get_local_description(&self) -> Option<SessionDescription>;
    /// Add remote ICE candidates to the peer connection
    async fn add_ice_candidates(&self, remote_candidates: Vec<ICECandidate>) -> Result<(), Error>;
    /// Pass a remote session description (answer) to the peer connection
    async fn set_remote_description(&self, remote_answer: SessionDescription) -> Result<(), Error>;
    /// Returns true if the `PeerConnection` is the 'local' PeerConnection.
    /// aka. the `PeerConnection` that created the offer (offerer)
    fn is_offerer(&self) -> bool;
}

#[derive(thiserror::Error, Debug)]
/// Error type for `PeerConnectionBuilder`
pub enum PeerConnectionBuilderError {
    /// Error raised due to conflicting build options
    #[error("remote offer is mutually exclusive with channel settings!")]
    ConflictingBuildOptions,
}

/// Builder for WebRTC Peer Connections
#[derive(Debug)]
pub struct PeerConnectionBuilder<P: Platform> {
    _platform: PhantomData<P>,
    config: PeerConfiguration,
    outgoing_buffer: usize,
    remote_offer: Option<SessionDescription>,
    channel_options: Vec<(String, DataChannelOptions)>,
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
    pub fn with_remote_offer(
        mut self,
        set: Option<SessionDescription>,
    ) -> Result<Self, PeerConnectionBuilderError> {
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
    pub fn with_channel_options(
        mut self,
        set: Vec<(String, DataChannelOptions)>,
    ) -> Result<Self, PeerConnectionBuilderError> {
        if self.remote_offer.is_none() {
            self.channel_options = set;
            Ok(self)
        } else {
            Err(PeerConnectionBuilderError::ConflictingBuildOptions)
        }
    }
}

/// Helper for creating a simple 'local' (offerer) peer connection without having to configure the [`PeerConnectionBuilder`]
///
/// A "simple peer connection" uses default google STUN/TURN servers, and default policies. It includes a single data channel.
#[derive(Debug)]
pub struct SimpleLocalPeerConnection {}

impl SimpleLocalPeerConnection {
    /// Create new simple local peer connection with a single data channel
    ///
    /// Ordered indicates whether the data transmitted across the data channel is allowed to be delivered out of order.
    pub async fn build(ordered: bool) -> Result<PeerConnection, Error> {
        // create  simple local peer connection from channel options
        let channel_options = vec![
            // single data channel with the label prefix "simple_channel_"
            (
                "simple_channel_".to_string(),
                DataChannelOptions {
                    ordered: Some(ordered),
                    ..Default::default()
                },
            ),
        ];
        let local_peer_connection = PeerConnectionBuilder::new()
            .with_channel_options(channel_options)
            .unwrap()
            .build()
            .await?;
        Ok(local_peer_connection)
    }
}

/// Helper for creating a simple 'remote' (answerer) peer connection without having to configure the [`PeerConnectionBuilder`]
///
/// A "simple peer connection" uses default google STUN/TURN servers, and default policies. It includes a single data channel.
#[derive(Debug)]
pub struct SimpleRemotePeerConnection {}

impl SimpleRemotePeerConnection {
    /// Create new simple remote peer connection from a received offer
    ///
    /// Ordered indicates whether the data transmitted across the data channel is allowed to be delivered out of order.
    pub async fn build(offer: SessionDescription) -> Result<PeerConnection, Error> {
        // create remote peer connection from received offer and candidates
        let remote_peer_connection = PeerConnectionBuilder::new()
            .with_remote_offer(Some(offer))
            .unwrap()
            .build()
            .await?;
        Ok(remote_peer_connection)
    }
}
