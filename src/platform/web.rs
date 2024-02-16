use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use js_sys::Function;
use tokio::sync::mpsc::UnboundedSender;
use log::{debug, error, trace};
use wasm_bindgen::{closure::Closure, convert::FromWasmAbi, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use web_sys::{Event, MessageEvent, RtcBundlePolicy, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcIceCandidateInit, RtcIceConnectionState, RtcIceTransportPolicy, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcPeerConnectionState};
use webrtc::{ice_transport::ice_candidate::RTCIceCandidateInit, peer_connection::sdp::session_description::RTCSessionDescription};

use crate::{Channel, PeerConnection, PeerConnectionBuilder};
use super::Platform;

pub struct Web {}
impl Platform for Web {}

fn handle_peer_connection_state_change(
    connection: Arc<RtcPeerConnection>,
) {
    let state = connection.connection_state();
    debug!("Peer connection state has changed: {state:?}");
    if state == RtcPeerConnectionState::Failed {
        error!("Peer connection failed");
    }
}

fn handle_ice_connection_state_change(
    connection: Arc<RtcPeerConnection>
) {
    let state = connection.ice_connection_state();
    debug!("ICE connection state has changed: {state:?}");
    if state == RtcIceConnectionState::Failed {
        error!("ICE connection failed!");
    }
}

fn handle_ice_gathering_state_change(
    connection: Arc<RtcPeerConnection>
) {
    let state = connection.ice_gathering_state();
    debug!("ICE gathering state has changed: {state:?}");
}

fn handle_ice_candidate(
    event: RtcPeerConnectionIceEvent,
    candidate_tx: &UnboundedSender<RTCIceCandidateInit>,
) {
    if let Some(candidate) = event.candidate() {
        match serde_wasm_bindgen::from_value::<RTCIceCandidateInit>(candidate.to_json().into()) {
            Ok(candidate_init) => {
                if let Err(e) = candidate_tx.send(candidate_init) {
                    error!("Candidate channel error! ({e})")
                }
            },
            Err(e) => error!("Failed to serialize ICE candidate! ({e})")
        };
    } else {
        debug!("ICE gathering finished.");
    }
}

fn handle_data_channel(
    channel: RtcDataChannel,
    local_channels: Arc<Mutex<Vec<Arc<Channel<RtcDataChannel>>>>>,
) {
    let label = channel.label();
    let id = channel.id().unwrap();
    let (incoming_tx, rx) = tokio::sync::mpsc::unbounded_channel::<Bytes>();
    debug!("New data channel ({label}:{id})");
    // register data channel handlers
    let label_close = label.clone();
    register_leaky_event_handler(
        |f| channel.set_onclose(f),
        move |event| handle_data_channel_close(&label_close, &id, event)
    );
    let label_open = label.clone();
    register_leaky_event_handler(
        |f| channel.set_onopen(f),
        move |event| handle_data_channel_open(&label_open, &id, event)
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
    let mut channels = local_channels.lock().unwrap();
    let channel = Arc::new(Channel { inner: channel, rx });
    channels.push(channel);
}

fn handle_data_channel_close(
    label: &str,
    id: &u16,
    event: Event,
) {
    debug!("Data channel closed ({label}:{id}");
    trace!("event: {event:?}");
}

fn handle_data_channel_open(
    label: &str,
    id: &u16,
    event: Event,
) {
    debug!("Data channel open ({label}:{id})");
    trace!("event: {event:?}");
}

fn handle_data_channel_message(
    label: &str,
    id: &u16,
    incoming_tx: &UnboundedSender<Bytes>,
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

impl Channel<RtcDataChannel> {
    pub fn send(&self, data: &Bytes) -> Result<()> {
        Ok(self.inner.send_with_u8_array(data).unwrap())
    }
}

impl PeerConnection<RtcPeerConnection, RtcDataChannel> {
    pub fn get_local_description(&self) -> Option<RTCSessionDescription> {
        if let Some(js_desc) = self.connection.local_description() {
            match serde_wasm_bindgen::from_value(js_desc.into()) {
                Ok(desc) => Some(desc),
                Err(e) => {
                    error!("could not deserialize from js description ({e})");
                    None
                },
            }
        } else {
            None
        }
    }

    pub async fn add_ice_candidate(&self, remote_candidate_init: RTCIceCandidateInit) -> Result<()> {
        // add remote ICE candidate
        match serde_wasm_bindgen::to_value(&remote_candidate_init) {
            Ok(js) => {
                let js_candidate_init = RtcIceCandidateInit::from(js);
                let js_promise = self.connection.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&js_candidate_init));
                JsFuture::from(js_promise).await.unwrap();
                Ok(())
            },
            Err(e) => Err(anyhow!("could not serialize to js ice candidate ({e})"))
        }
    }
}

impl PeerConnectionBuilder<Web> {
    pub async fn build(&self) -> Result<PeerConnection<RtcPeerConnection, RtcDataChannel>> {
        // validate builder
        if self.remote_offer.is_some() && (!self.channel_options.is_empty()) {
            return Err(anyhow!("remote offer is mutually exclusive with channel settings"));
        }

        // parse config to dictionary
        let mut config = RtcConfiguration::new();
        config.bundle_policy(
            RtcBundlePolicy::from_js_value(
                &serde_wasm_bindgen::to_value(&self.config.bundle_policy).unwrap()
            ).unwrap()
        );
        config.ice_servers(&serde_wasm_bindgen::to_value(&self.config.ice_servers).unwrap());
        config.ice_transport_policy(
            RtcIceTransportPolicy::from_js_value(
                &serde_wasm_bindgen::to_value(&self.config.ice_transport_policy).unwrap()
            ).unwrap()
        );
        config.peer_identity(Some(&self.config.peer_identity));

        // create new connection from config
        let connection = Arc::new(RtcPeerConnection::new_with_configuration(&config).unwrap());

        // create mpsc channels for passing info to/from handlers
        let (candidate_tx, candidate_rx) = tokio::sync::mpsc::unbounded_channel::<RTCIceCandidateInit>();

        // register peer state change handler
        let connection_ptr = connection.clone();
        register_leaky_event_handler(
            |f| connection.set_onconnectionstatechange(f),
            move |_event: JsValue| handle_peer_connection_state_change(connection_ptr.clone())
        );
        // register ice connection state change handler
        let connection_ptr = connection.clone();
        register_leaky_event_handler(
            |f| connection.set_oniceconnectionstatechange(f),
            move |_event: JsValue| handle_ice_connection_state_change(connection_ptr.clone())
        );
        // register ice gathering state change handler
        let connection_ptr = connection.clone();
        register_leaky_event_handler(
            |f| connection.set_onicegatheringstatechange(f),
            move |_event: JsValue| handle_ice_gathering_state_change(connection_ptr.clone())
        );
        // register ice candidate handler
        register_leaky_event_handler(
            |f| connection.set_onicecandidate(f),
            move |event| handle_ice_candidate(event, &candidate_tx)
        );

        let channels: Arc<Mutex<Vec<Arc<Channel<RtcDataChannel>>>>> = Arc::new(Mutex::new(vec![]));
        // if an offer is provided, we are an answerer and are receiving the data channel
        // otherwise, we are an offerer and must configure and create the data channel
        let (desc, is_offerer) = if let Some(offer) = &self.remote_offer {
            // register data channel handler
            let channels_ref = channels.clone();
            register_leaky_event_handler(
                |f| connection.set_ondatachannel(f),
                move |event: RtcDataChannelEvent| {
                    let channel = event.channel();
                    handle_data_channel(channel, channels_ref.clone());
                }
            );
            // set the remote SessionDescription (provided by remote peer via external signalling)
            let js_value = serde_wasm_bindgen::to_value(offer).unwrap();
            let js_promise = connection.set_remote_description(&js_value_to_dict(js_value)?);
            let _ = JsFuture::from(js_promise).await.unwrap();
            // create answer
            let js_promise = connection.create_answer();
            let js_value = JsFuture::from(js_promise).await.unwrap();
            let answer = js_value_to_dict(js_value)?;
            (answer, false)
        } else {

            // create channels from options (we are offering)
            for (index, (label_prefix, channel_options)) in self.channel_options.iter().enumerate() {
                let js_value = serde_wasm_bindgen::to_value(channel_options).unwrap();
                let data_channel_dict = js_value_to_dict(js_value)?;
                let channel = connection.create_data_channel_with_data_channel_dict(&format!("{label_prefix}{index}"), &data_channel_dict);
                handle_data_channel(channel, channels.clone())
            }

            // create offer
            let js_promise = connection.create_offer();
            let js_value = JsFuture::from(js_promise).await.unwrap();
            let offer = js_value_to_dict(js_value)?;

            (offer, true)
        };

        // sets the local SessionDescription (offer/answer), and starts UDP listeners
        let js_promise = connection.set_local_description(&desc);
        let _ = JsFuture::from(js_promise).await.unwrap();

        Ok(PeerConnection { is_offerer, connection, channels, candidate_rx })
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

fn js_value_to_dict<T: JsCast>(
    value: JsValue,
) -> Result<T> {
    if T::is_type_of(&value) {
        let desc = T::unchecked_from_js(value);
        Ok(desc)
    } else {
        return Err(anyhow!("Could not create dict from JsValue"));
    }
}
