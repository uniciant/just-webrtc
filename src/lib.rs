use std::{marker::PhantomData, sync::{Arc, Mutex}};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use platform::Platform;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{error::TryRecvError, UnboundedReceiver};
use webrtc::{ice_transport::{ice_candidate::RTCIceCandidateInit, ice_server::RTCIceServer}, peer_connection::{configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription}};

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
    rx: UnboundedReceiver<Bytes>,
}

impl<T> Channel<T> {
    pub async fn receive(&mut self) -> Result<Bytes> {
        if let Some(data) = self.rx.recv().await {
            Ok(data)
        } else {
            Err(anyhow!("rx channel closed!"))
        }
    }

    pub fn try_receive(&mut self) -> Result<Bytes> {
        Ok(self.rx.try_recv()?)
    }

}

pub struct PeerConnection<T, U> {
    is_offerer: bool,
    connection: Arc<T>,
    channels: Arc<Mutex<Vec<Arc<Channel<U>>>>>,
    candidate_rx: UnboundedReceiver<RTCIceCandidateInit>,
}

impl<T, U> PeerConnection<T, U> {
    pub fn is_offerer(&self) -> bool {
        self.is_offerer
    }

    pub fn get_channel(&self, index: usize) -> Result<Arc<Channel<U>>> {
        let channels = self.channels.lock().unwrap();
        let channel = channels.get(index).unwrap();
        Ok(channel.clone())
    }

    pub fn try_get_ice_candidates(&mut self) -> Result<Vec<RTCIceCandidateInit>> {
        let mut candidate_inits = vec![];
        loop {
            match self.candidate_rx.try_recv() {
                Ok(candidate_init) => candidate_inits.push(candidate_init),
                Err(TryRecvError::Empty) => break,
                Err(e) => return Err(e.into())
            }
        }
        Ok(candidate_inits)
    }
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
                    "stun:stun.l.google.com:19302".to_owned(),
                    "stun:stun1.l.google.com:19302".to_owned(),
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
    /// Create new DataChannelBuilder (equivalent to DataChannelBuilder::default())
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
