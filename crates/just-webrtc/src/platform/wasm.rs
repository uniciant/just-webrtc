//! Wasm WebRTC implementation using `web_sys`

use std::rc::Rc;

use async_cell::unsync::AsyncCell;
use bytes::Bytes;
use js_sys::{Function, Reflect};
use local_channel::mpsc::{Receiver, Sender};
use log::{debug, error, trace};
use wasm_bindgen::{closure::Closure, convert::FromWasmAbi, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use web_sys::{Event, MessageEvent, RtcBundlePolicy, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelInit, RtcDataChannelType, RtcIceCandidateInit, RtcIceConnectionState, RtcIceTransportPolicy, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcPeerConnectionState, RtcSdpType, RtcSessionDescriptionInit};

use crate::{types::{BundlePolicy, ICECandidate, ICETransportPolicy, PeerConnectionState, SDPType, SessionDescription}, DataChannelExt, PeerConnectionExt, PeerConnectionBuilder};
use super::Platform;

/// WASM platform marker
#[derive(Debug)]
pub struct Wasm {}
impl Platform for Wasm {}

#[derive(thiserror::Error, Debug)]
/// WASM JustWebRTC Error type
pub enum Error {
    /// Local unbounded mpsc channel receive error
    #[error("data channel senders dropped!")]
    MpscRecvError,
    /// Error originating from web_sys
    #[error("webrtc error: {0}")]
    WebRtcError(String),
    /// Data channel closed error
    #[error("data channel closed!")]
    DataChannelClosed,
}

impl From<serde_wasm_bindgen::Error> for Error {
    fn from(value: serde_wasm_bindgen::Error) -> Self {
        Error::WebRtcError(value.to_string())
    }
}

fn js_value_to_error(value: JsValue) -> Error {
    let value = serde_wasm_bindgen::Error::from(value);
    Error::from(value)
}

/// WASM JustWebRTC channel wrapper
#[derive(Debug)]
pub struct Channel {
    inner: RtcDataChannel,
    ready_state: Rc<AsyncCell<bool>>,
    rx: Receiver<Bytes>,
}

impl DataChannelExt for Channel {
    async fn wait_ready(&mut self) {
        while !(self.ready_state.take().await) {}
    }

    async fn receive(&mut self) -> Result<Bytes, Error> {
        self.rx.recv().await.ok_or(Error::MpscRecvError)
    }

    async fn send(&self, data: &Bytes) -> Result<usize, Error> {
        self.inner.send_with_u8_array(data).map_err(js_value_to_error)?;
        Ok(data.len())
    }

    fn id(&self) -> u16 {
        self.inner.id().unwrap_or(0)
    }

    fn label(&self) -> String {
        self.inner.label()
    }
}

/// WASM JustWebRTC PeerConnection wrapper
#[derive(Debug)]
pub struct PeerConnection {
    is_offerer: bool,
    inner: Rc<RtcPeerConnection>,
    peer_connection_state: Rc<AsyncCell<PeerConnectionState>>,
    channels_rx: Receiver<Channel>,
    candidate_rx: Receiver<Option<ICECandidate>>,
}

impl PeerConnectionExt for PeerConnection {
    async fn wait_peer_connected(&mut self) {
        while self.peer_connection_state.take().await != PeerConnectionState::Connected {}
    }

    async fn receive_channel(&mut self) -> Result<Channel, Error> {
        self.channels_rx.recv().await.ok_or(Error::MpscRecvError)
    }

    async fn collect_ice_candidates(&mut self) -> Result<Vec<ICECandidate>, Error> {
        let mut candidate_inits = vec![];
        while let Some(candidate_init) = self.candidate_rx.recv().await.ok_or(Error::MpscRecvError)? {
            candidate_inits.push(candidate_init);
        }
        Ok(candidate_inits)
    }

    async fn get_local_description(&self) -> Option<SessionDescription> {
        if let Some(js_desc) = self.inner.local_description() {
            let desc = SessionDescription {
                sdp: js_desc.sdp(),
                sdp_type: js_desc.type_().into(),
            };
            Some(desc)
        } else {
            None
        }
    }

    async fn add_ice_candidates(&self, remote_candidates: Vec<ICECandidate>) -> Result<(), Error> {
        // add remote ICE candidates
        for candidate in remote_candidates {
            let mut candidate_init = RtcIceCandidateInit::new(&candidate.candidate);
            candidate_init.sdp_m_line_index(candidate.sdp_mline_index);
            candidate_init.sdp_mid(candidate.sdp_mid.as_deref());
            let js_promise = self.inner.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&candidate_init));
            JsFuture::from(js_promise).await.map_err(js_value_to_error)?;
        }
        Ok(())
    }

    async fn set_remote_description(&self, remote_answer: SessionDescription) -> Result<(), Error> {
        // add remote description (answer)
        let mut desc_dict = RtcSessionDescriptionInit::new(remote_answer.sdp_type.into());
        desc_dict.sdp(&remote_answer.sdp);
        let js_promise = self.inner.set_remote_description(&desc_dict);
        JsFuture::from(js_promise).await.map_err(js_value_to_error)?;
        Ok(())
    }

    fn is_offerer(&self) -> bool {
        self.is_offerer
    }
}

fn handle_data_channel(
    channel: RtcDataChannel,
    channels_tx: &Sender<Channel>,
) {
    let label = channel.label();
    let id = channel.id().unwrap_or(0);  // zero values are None?
    // override default 'blob' binary type format that is unsupported on most browsers
    channel.set_binary_type(RtcDataChannelType::Arraybuffer);

    let (incoming_tx, rx) = local_channel::mpsc::channel();
    let ready_state = Rc::new(AsyncCell::new());

    debug!("New data channel ({label}:{id})");
    // register data channel handlers
    let label_close = label.clone();
    let ready_state_close = ready_state.clone();
    register_leaky_event_handler(
        |f| channel.set_onclose(f),
        move |event| handle_data_channel_close(&label_close, &id, event, &ready_state_close)
    );
    let label_open = label.clone();
    let ready_state_open = ready_state.clone();
    register_leaky_event_handler(
        |f| channel.set_onopen(f),
        move |event| handle_data_channel_open(&label_open, &id, event, &ready_state_open)
    );
    let label_message = label.clone();
    register_leaky_event_handler(
        |f| channel.set_onmessage(f),
        move |event| handle_data_channel_message(&label_message, &id, &incoming_tx, event)
    );
    register_leaky_event_handler(
        |f| channel.set_onerror(f),
        move |event| handle_data_channel_error(&label, &id, event)
    );
    // push channel & receiver to list
    let channel = Channel { inner: channel, rx, ready_state };
    if let Err(e) = channels_tx.send(channel) {
        error!("could not send data channel! ({e})")
    }
}

fn handle_data_channel_close(
    label: &str,
    id: &u16,
    event: Event,
    ready_state: &AsyncCell<bool>
) {
    ready_state.set(false);
    debug!("Data channel closed ({label}:{id}");
    trace!("event: {event:?}");
}

fn handle_data_channel_open(
    label: &str,
    id: &u16,
    event: Event,
    ready_state: &AsyncCell<bool>,
) {
    ready_state.set(true);
    debug!("Data channel open ({label}:{id})");
    trace!("event: {event:?}");
}

fn handle_data_channel_message(
    label: &str,
    id: &u16,
    incoming_tx: &Sender<Bytes>,
    event: MessageEvent,
) {
    trace!("Data channel received message ({label}:{id})");
    // receive data over unbounded channel
    if let Ok(array_buf) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
        let u_array = js_sys::Uint8Array::new(&array_buf);
        let body = u_array.to_vec();
        if let Err(e) = incoming_tx.send(body.into()) {
            error!("incoming mpsc error {e}");
        }
    } else {
        error!("Could not receive message data as ArrayBuffer (Blob not supported.)")
    }
}

fn handle_data_channel_error(
    label: &str,
    id: &u16,
    event: Event,
) {
    error!("Internal error on data channel ({label}:{id}). Error ({event:?})");
}

fn handle_ice_connection_state_change(
    connection: &Rc<RtcPeerConnection>
) {
    let state = connection.ice_connection_state();
    debug!("ICE connection state has changed: {state:?}");
    if state == RtcIceConnectionState::Failed {
        error!("ICE connection failed!");
    }
}

fn handle_ice_gathering_state_change(
    connection: &Rc<RtcPeerConnection>
) {
    let state = connection.ice_gathering_state();
    debug!("ICE gathering state has changed: {state:?}");
}

fn handle_ice_candidate(
    event: RtcPeerConnectionIceEvent,
    candidate_tx: &Sender<Option<ICECandidate>>,
) {
    let message = if let Some(candidate) = event.candidate() {
        let candidate_init = ICECandidate {
            candidate: candidate.candidate(),
            sdp_mid:  candidate.sdp_mid(),
            sdp_mline_index: candidate.sdp_m_line_index(),
            username_fragment: None,
        };
        Some(candidate_init)
    } else {
        debug!("ICE gathering finished.");
        None
    };
    if let Err(e) = candidate_tx.send(message) {
        error!("candidate channel error! ({e})")
    }
}

fn handle_peer_connection_state_change(
    connection: &Rc<RtcPeerConnection>,
    peer_state: &Rc<AsyncCell<PeerConnectionState>>
) {
    let state = connection.connection_state();
    if state == RtcPeerConnectionState::Failed {
        error!("Peer connection failed");
    } else {
        debug!("Peer connection state has changed: {state:?}");
    }
    peer_state.set(state.into());
}

impl PeerConnectionBuilder<Wasm> {
    /// Build new WASM JustWebRTC Peer Connection
    pub async fn build(&self) -> Result<PeerConnection, Error> {
        // parse config to dictionary
        let mut config = RtcConfiguration::new();
        config.bundle_policy(self.config.bundle_policy.into());
        config.ice_servers(&serde_wasm_bindgen::to_value(&self.config.ice_servers)?);
        config.ice_transport_policy(self.config.ice_transport_policy.into());
        config.peer_identity(Some(&self.config.peer_identity));

        // create new connection from config
        let connection = Rc::new(RtcPeerConnection::new_with_configuration(&config).map_err(js_value_to_error)?);

        // create mpsc channels for passing info to/from handlers
        let (candidate_tx, candidate_rx) = local_channel::mpsc::channel::<Option<ICECandidate>>();
        let (channels_tx, channels_rx) = local_channel::mpsc::channel::<Channel>();
        let peer_connection_state = Rc::new(AsyncCell::new());

        // register peer state change handler
        let connection_ptr = connection.clone();
        let peer_connection_state_ptr = peer_connection_state.clone();
        register_leaky_event_handler(
            |f| connection.set_onconnectionstatechange(f),
            move |_event: JsValue| handle_peer_connection_state_change(&connection_ptr, &peer_connection_state_ptr)
        );
        // register ice connection state change handler
        let connection_ptr = connection.clone();
        register_leaky_event_handler(
            |f| connection.set_oniceconnectionstatechange(f),
            move |_event: JsValue| handle_ice_connection_state_change(&connection_ptr)
        );
        // register ice gathering state change handler
        let connection_ptr = connection.clone();
        register_leaky_event_handler(
            |f| connection.set_onicegatheringstatechange(f),
            move |_event: JsValue| handle_ice_gathering_state_change(&connection_ptr)
        );
        // register ice candidate handler
        register_leaky_event_handler(
            |f| connection.set_onicecandidate(f),
            move |event| handle_ice_candidate(event, &candidate_tx)
        );

        // if an offer is provided, we are an answerer and are receiving the data channel
        // otherwise, we are an offerer and must configure and create the data channel
        let (desc, is_offerer) = if let Some(offer) = &self.remote_offer {
            // register data channel handler
            register_leaky_event_handler(
                |f| connection.set_ondatachannel(f),
                move |event: RtcDataChannelEvent| {
                    let channel = event.channel();
                    handle_data_channel(channel, &channels_tx);
                }
            );
            // set the remote SessionDescription (provided by remote peer via external signalling)
            let mut remote_desc_dict = RtcSessionDescriptionInit::new(offer.sdp_type.into());
            remote_desc_dict.sdp(&offer.sdp);
            let js_promise = connection.set_remote_description(&remote_desc_dict);
            JsFuture::from(js_promise).await.map_err(js_value_to_error)?;
            // create answer
            let js_promise = connection.create_answer();
            let js_value = JsFuture::from(js_promise).await.map_err(js_value_to_error)?;
            let js_value = Reflect::get(&js_value, &JsValue::from_str("sdp")).map_err(js_value_to_error)?;
            let answer_sdp = js_value.as_string().unwrap();
            let mut answer = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
            answer.sdp(&answer_sdp);
            (answer, false)
        } else {
            // create channels from options (we are offering)
            for (index, (label_prefix, channel_options)) in self.channel_options.iter().enumerate() {
                let mut data_channel_dict = RtcDataChannelInit::new();
                if let Some(o) = channel_options.ordered { data_channel_dict.ordered(o); }
                if let Some(m) = channel_options.max_packet_life_time { data_channel_dict.max_packet_life_time(m); }
                if let Some(r) = channel_options.max_retransmits { data_channel_dict.max_retransmits(r); }
                if let Some(p) = &channel_options.protocol { data_channel_dict.protocol(p); }
                if let Some(id) = channel_options.negotiated {
                    data_channel_dict.negotiated(true);
                    data_channel_dict.id(id);
                } else {
                    data_channel_dict.id(index as u16);
                }
                let channel = connection.create_data_channel_with_data_channel_dict(&format!("{label_prefix}{index}"), &data_channel_dict);
                handle_data_channel(channel, &channels_tx);
            }
            // create offer
            let js_promise = connection.create_offer();
            let js_value = JsFuture::from(js_promise).await.map_err(js_value_to_error)?;
            let js_value = Reflect::get(&js_value, &JsValue::from_str("sdp")).map_err(js_value_to_error)?;
            let offer_sdp = js_value.as_string().unwrap();
            let mut offer = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
            offer.sdp(&offer_sdp);
            (offer, true)
        };

        // sets the local SessionDescription (offer/answer), and starts UDP listeners
        let js_promise = connection.set_local_description(&desc);
        JsFuture::from(js_promise).await.map_err(js_value_to_error)?;

        Ok(PeerConnection { is_offerer, inner: connection, peer_connection_state, channels_rx, candidate_rx })
    }
}

/// Note that this function leaks some memory because the rust closure is dropped but still needs to
/// be accessed by javascript of the browser
///
/// See also: https://rustwasm.github.io/wasm-bindgen/api/wasm_bindgen/closure/struct.Closure.html#method.into_js_value
fn register_leaky_event_handler<T: FromWasmAbi + 'static>(
    mut setter: impl FnMut(Option<&Function>),
    handler: impl FnMut(T) + 'static,
) {
    let closure: Closure<dyn FnMut(T)> = Closure::wrap(Box::new(handler));
    setter(Some(closure.as_ref().unchecked_ref()));
    closure.forget();
}

// Convert from just_webrtc types to wasm webrtc types

impl From<RtcPeerConnectionState> for PeerConnectionState {
    fn from(value: RtcPeerConnectionState) -> Self {
        match value {
            RtcPeerConnectionState::Closed => Self::Closed,
            RtcPeerConnectionState::Connected => Self::Connected,
            RtcPeerConnectionState::Connecting => Self::Connecting,
            RtcPeerConnectionState::Disconnected => Self::Disconnected,
            RtcPeerConnectionState::Failed => Self::Failed,
            RtcPeerConnectionState::New => Self::New,
            _ => Self::New,
        }
    }
}

impl From<BundlePolicy> for RtcBundlePolicy {
    fn from(value: BundlePolicy) -> Self {
        match value {
            BundlePolicy::Balanced => Self::Balanced,
            BundlePolicy::MaxBundle => Self::MaxBundle,
            BundlePolicy::MaxCompat => Self::MaxCompat,
        }
    }
}

impl From<ICETransportPolicy> for RtcIceTransportPolicy {
    fn from(value: ICETransportPolicy) -> Self {
        match value {
            ICETransportPolicy::All => Self::All,
            ICETransportPolicy::Relay => Self::Relay,
        }
    }
}

impl From<SDPType> for RtcSdpType {
    fn from(value: SDPType) -> Self {
        match value {
            SDPType::Answer => Self::Answer,
            SDPType::Offer => Self::Offer,
            SDPType::Pranswer => Self::Pranswer,
            SDPType::Rollback => Self::Rollback,
        }
    }
}

impl From<RtcSdpType> for SDPType {
    fn from(value: RtcSdpType) -> Self {
        match value {
            RtcSdpType::Answer => Self::Answer,
            RtcSdpType::Offer => Self::Offer,
            RtcSdpType::Pranswer => Self::Pranswer,
            RtcSdpType::Rollback => Self::Rollback,
            _ => Self::Answer,
        }
    }
}
