use anyhow::{anyhow, Result};
use bytes::Bytes;

use log::{debug, error, trace};
use tokio::sync::mpsc::{error::TryRecvError, UnboundedReceiver, UnboundedSender};

use std::{future::Future, pin::Pin, sync::{Arc, Mutex}};

use webrtc::{
    api::APIBuilder, data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, RTCDataChannel}, ice_transport::{ice_candidate::{RTCIceCandidate, RTCIceCandidateInit}, ice_connection_state::RTCIceConnectionState, ice_gatherer_state::RTCIceGathererState, ice_server::RTCIceServer}, peer_connection::{configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}, Error
};


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
    local_channels: Arc<Mutex<Vec<(Arc<RTCDataChannel>, UnboundedReceiver<Bytes>)>>>,
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
    channels.push((channel, rx));
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

pub struct PeerConnection {
    is_offerer: bool,
    connection: RTCPeerConnection,
    channels: Arc<Mutex<Vec<(Arc<RTCDataChannel>, UnboundedReceiver<Bytes>)>>>,
    candidate_rx: UnboundedReceiver<RTCIceCandidateInit>,
}

impl PeerConnection {
    pub async fn get_local_description(&self) -> Option<RTCSessionDescription> {
        self.connection.local_description().await
    }

    pub fn is_offerer(&self) -> bool {
        self.is_offerer
    }

    pub async fn get_ice_candidates(&mut self) -> Result<Vec<RTCIceCandidateInit>> {
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

    pub async fn add_ice_candidate(&self, remote_candidate_init: RTCIceCandidateInit) -> Result<()> {
        // add remote ICE candidate
        self.connection.add_ice_candidate(remote_candidate_init).await?;
        Ok(())
    }
}


pub struct PeerConnectionBuilder {
    config: RTCConfiguration,
    outgoing_buffer: usize,
    remote_offer: Option<RTCSessionDescription>,
    channel_options: Vec<(String, RTCDataChannelInit)>,
}

impl Default for PeerConnectionBuilder {
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
            config,
            outgoing_buffer,
            remote_offer: None,
            channel_options: vec![],
        }
    }
}

impl PeerConnectionBuilder {
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
    pub fn with_channel_options(&mut self, set: Vec<(String, RTCDataChannelInit)>) -> &Self {
        self.channel_options = set;
        self
    }

    pub async fn build(&self) -> Result<PeerConnection> {
        // validate builder
        if self.remote_offer.is_some() && !self.channel_options.is_empty() {
            return Err(anyhow!("remote offer is mutually exclusive with channel settings"));
        }

        // create new connection from the api and config
        let api = APIBuilder::default().build();
        let connection: RTCPeerConnection = api.new_peer_connection(self.config.clone()).await?;

        // create mpsc channels for passing info to/from handlers
        let (candidate_tx, candidate_rx) = tokio::sync::mpsc::unbounded_channel::<RTCIceCandidateInit>();
        // register state change handler
        connection.on_peer_connection_state_change(Box::new(move |state| handle_peer_connection_state_change(state)));
        // register ice related handlers
        connection.on_ice_connection_state_change(Box::new(move |state| handle_ice_connection_state_change(state)));
        connection.on_ice_gathering_state_change(Box::new(move |state| handle_ice_gathering_state_change(state)));
        connection.on_ice_candidate(Box::new(move |candidate| handle_ice_candidate(candidate, &candidate_tx)));

        let channels: Arc<Mutex<Vec<(Arc<RTCDataChannel>, UnboundedReceiver<Bytes>)>>> = Arc::new(Mutex::new(vec![]));
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
                // create channels from options (we are offering)
                let data_channel = connection.create_data_channel(&format!("{label_prefix}{index}"), Some(channel_options.clone())).await?;
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
