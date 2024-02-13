use anyhow::{anyhow, Result};
use bytes::Bytes;

use tokio::sync::{mpsc::{error::TryRecvError, Receiver, Sender, UnboundedReceiver, UnboundedSender}, Mutex};

use std::{future::Future, pin::Pin, sync::Arc};

use webrtc::{
    api::{APIBuilder, API}, data_channel::{data_channel_init::RTCDataChannelInit, data_channel_message::DataChannelMessage, RTCDataChannel}, ice_transport::{ice_candidate::{RTCIceCandidate, RTCIceCandidateInit}, ice_connection_state::RTCIceConnectionState, ice_gatherer_state::RTCIceGathererState, ice_server::RTCIceServer}, peer_connection::{configuration::RTCConfiguration, peer_connection_state::RTCPeerConnectionState, sdp::session_description::RTCSessionDescription, RTCPeerConnection}, Error
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

fn handle_data_channel_close(
    label: &str,
    id: &u16
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    println!("Data channel closed ({label}:{id}");
    // do nothing
    Box::pin(async {})
}

fn handle_data_channel_open(
    label: String,
    id: u16,
    outgoing_rx: Arc<Mutex<Receiver<Bytes>>>,
    data_channel: Arc<RTCDataChannel>
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    println!("Data channel open ({label}:{id})");
    // transmit loop
    Box::pin(async move {
        let mut outgoing_rx = outgoing_rx.lock().await;
        loop {
            if let Some(s) = outgoing_rx.recv().await {
                if let Err(e) = data_channel.send(&s).await {
                    println!("error sending over data channel ({label}:{id}), Error: {e}");
                    break;
                }
            } else {
                println!("outgoing channel closed!");
                break;
            }
        }
    })
}

fn handle_data_channel_message(
    incoming_tx: &UnboundedSender<Bytes>,
    message: DataChannelMessage
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    if let Err(e) = incoming_tx.send(message.data) {
        println!("incoming mpsc error {e}");
    }
    Box::pin(async move {})
}

fn handle_data_channel_error(
    error: Error,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    println!("internal data channel error {error}");
    Box::pin(async {})
}

fn handle_ice_connection_state_change(
    state: RTCIceConnectionState,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
    if state == RTCIceConnectionState::Failed {
        println!("ICE connection failed.");
    } else {
        println!("ICE connection state has changed: {state}");
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
    pub tx: Sender<Bytes>,
    pub rx: UnboundedReceiver<Bytes>,
    is_offerer: bool,
    connection: RTCPeerConnection,
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
    api: API,
    config: RTCConfiguration,
    outgoing_buffer: usize,
    remote_offer: Option<RTCSessionDescription>,
    channel_label: String,
    channel_options: Option<RTCDataChannelInit>
}

impl Default for PeerConnectionBuilder {
    fn default() -> Self {
        let api = APIBuilder::default().build();
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
            api,
            config,
            outgoing_buffer,
            remote_offer: None,
            channel_label: String::new(),
            channel_options: None,
        }
    }
}

impl PeerConnectionBuilder {
    /// Create new DataChannelBuilder (equivalent to DataChannelBuilder::default())
    pub fn new() -> Self {
        Self::default()
    }

    /// Specify internal WebRTC API settings
    pub fn set_api(&mut self, set: API) -> &Self {
        self.api = set;
        self
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

    /// Provide label for initial data channel creation (as an offerer)
    ///
    /// When not empty, mutually exclusive with `with_remote_offer`.
    pub fn with_channel_label(&mut self, set: String) -> &Self {
        self.channel_label = set;
        self
    }

    /// Provide options for initial data channel creation (as an offerer)
    ///
    /// This option is mutually exclusive with `with_remote_offer`.
    pub fn with_channel_options(&mut self, set: Option<RTCDataChannelInit>) -> &Self {
        self.channel_options = set;
        self
    }

    pub async fn build(&self) -> Result<PeerConnection> {
        // validate builder
        if self.remote_offer.is_some() && (!self.channel_label.is_empty() || self.channel_options.is_some()) {
            return Err(anyhow!("remote offer is mutually exclusive with channel settings"));
        }

        // create new connection from the api and config
        let connection: RTCPeerConnection = self.api.new_peer_connection(self.config.clone()).await?;

        // create mpsc channels for passing info to/from handlers
        let (tx, outgoing_rx) = tokio::sync::mpsc::channel::<Bytes>(self.outgoing_buffer);
        let outgoing_rx = Arc::new(Mutex::new(outgoing_rx));
        let (incoming_tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
        let (candidate_tx, candidate_rx) = tokio::sync::mpsc::unbounded_channel::<RTCIceCandidateInit>();
        // register state change handler
        connection.on_peer_connection_state_change(Box::new(move |state| handle_peer_connection_state_change(state)));
        // register ice related handlers
        connection.on_ice_connection_state_change(Box::new(move |state| handle_ice_connection_state_change(state)));
        connection.on_ice_gathering_state_change(Box::new(move |state| handle_ice_gathering_state_change(state)));
        connection.on_ice_candidate(Box::new(move |candidate| handle_ice_candidate(candidate, &candidate_tx)));

        // if an offer is provided, we are an answerer and are receiving the data channel
        // otherwise, we are an offerer and must configure and create the data channel
        let (desc, is_offerer) = if let Some(offer) = self.remote_offer.clone() {
            // configure from remote offer (we are answering)
            // register data channel handler
            connection.on_data_channel(Box::new(move |data_channel| {
                let label = data_channel.label().to_string();
                let id = data_channel.id();
                let incoming_tx_copy = incoming_tx.clone();
                let outgoing_rx_ref = outgoing_rx.clone();
                let data_channel_ref = data_channel.clone();

                println!("New data channel ({label}:{id})");
                // handle open channel
                Box::pin(async move {
                    let label_close = label.clone();
                    data_channel.on_close(Box::new(move || handle_data_channel_close(&label_close, &id)));
                    data_channel.on_open(Box::new(move || handle_data_channel_open(label, id, outgoing_rx_ref, data_channel_ref)));
                    data_channel.on_message(Box::new(move |message| handle_data_channel_message(&incoming_tx_copy, message)));
                    data_channel.on_error(Box::new(move |error| handle_data_channel_error(error)));
                })
            }));

            // set the remote SessionDescription (provided by remote peer via external signalling)
            connection.set_remote_description(offer).await?;

            // create answer and start the UDP listeners
            let answer = connection.create_answer(None).await?;
            (answer, false)
        } else {
            // create channel from options (we are offering)
            let data_channel = connection.create_data_channel(&self.channel_label, self.channel_options.clone()).await?;
            // register data channel handlers
            let label = data_channel.label().to_string();
            let label_close = label.clone();
            let id = data_channel.id();
            let data_channel_ref = data_channel.clone();
            data_channel.on_close(Box::new(move || handle_data_channel_close(&label_close, &id)));
            data_channel.on_open(Box::new(move || handle_data_channel_open(label, id, outgoing_rx, data_channel_ref)));
            data_channel.on_message(Box::new(move |message| handle_data_channel_message(&incoming_tx, message)));
            data_channel.on_error(Box::new(move |error| handle_data_channel_error(error)));

            // create offer and start the UDP listeners
            let offer = connection.create_offer(None).await?;
            (offer, true)
        };

        // sets the local SessionDescription (offer/answer), and starts UDP listeners
        connection.set_local_description(desc).await?;

        Ok(PeerConnection { is_offerer, connection, candidate_rx, tx, rx })
    }
}