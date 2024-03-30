//! Just WebRTC Signalling full-mesh client for both `native` and `wasm`

use std::{collections::HashSet, future::Future, pin::Pin, time::Duration};

use futures_util::{pin_mut, select, stream::FuturesUnordered, FutureExt, StreamExt};
use log::{debug, info, trace, warn};
use tonic::{metadata::MetadataMap, Extensions, Request, Streaming};

use crate::pb::{
    AdvertiseReq, AnswerListenerReq, AnswerListenerRsp, Change, OfferListenerReq, OfferListenerRsp,
    PeerChange, PeerDiscoverReq, PeerId, PeerListenerReq, PeerListenerRsp, SignalAnswer,
    SignalAnswerReq, SignalOffer, SignalOfferReq,
};

/// Default deadline for gRPC method response
pub const DEFAULT_RESPONSE_DEADLINE: Duration = Duration::from_secs(10);

/// Signalling client error
#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    /// Bincode encoding/decoding error
    #[error(transparent)]
    Bincode(#[from] bincode::Error),
    /// Pre-existing peer connection error
    #[error("Pre-existing connection with peer!")]
    PreExistingPeerConnection,
    /// Listener closed error
    #[error("Listener closed unexpectedly!")]
    ListenerClosed,
    /// Listener unavailable error
    #[error("Listener unavailable!")]
    ListenerUnavailable,
    /// Invalid URL error
    #[error("Invalid URL")]
    InvalidUrl,
    /// Tonic gRPC method error
    #[error(transparent)]
    TonicStatus(#[from] tonic::Status),
    /// Tonic transport layer error
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    TonicTransport(#[from] tonic::transport::Error),
    /// Externally provided callback error
    #[error(transparent)]
    ExternalFn(#[from] anyhow::Error),
}

/// Just WebRTC client result type
pub type ClientResult<T> = Result<T, ClientError>;

/// Set of description, candidates and a remote peer ID.
#[derive(Debug)]
pub struct SignalSet<D, C> {
    /// Serializable session description
    pub desc: D,
    /// Serializable ICE candidates
    pub candidates: C,
    /// Remote peer ID
    pub remote_id: u64,
}

/// Private peer listener helper method
async fn peer_listener_task(
    mut listener: Streaming<PeerListenerRsp>,
) -> ClientResult<(Streaming<PeerListenerRsp>, Vec<PeerChange>)> {
    if let Some(message) = listener.message().await? {
        Ok((listener, message.peer_changes))
    } else {
        Err(ClientError::ListenerClosed)
    }
}
/// Private offer listener helper method
async fn offer_listener_task<O, C>(
    mut listener: Streaming<OfferListenerRsp>,
) -> ClientResult<(Streaming<OfferListenerRsp>, SignalSet<O, C>)>
where
    O: serde::de::DeserializeOwned,
    C: serde::de::DeserializeOwned,
{
    if let Some(message) = listener.message().await? {
        if let Some(signal) = message.offer_signal {
            let offer = bincode::deserialize(&signal.offer)?;
            let candidates = bincode::deserialize(&signal.candidates)?;
            let offer_set = SignalSet {
                desc: offer,
                candidates,
                remote_id: signal.offerer_id,
            };
            Ok((listener, offer_set))
        } else {
            trace!("received empty offer message.");
            offer_listener_task(listener).boxed_local().await
        }
    } else {
        Err(ClientError::ListenerClosed)
    }
}
/// Private answer listener helper method
async fn answer_listener_task<A, C>(
    mut listener: Streaming<AnswerListenerRsp>,
) -> ClientResult<(Streaming<AnswerListenerRsp>, SignalSet<A, C>)>
where
    A: serde::de::DeserializeOwned,
    C: serde::de::DeserializeOwned,
{
    if let Some(message) = listener.message().await? {
        if let Some(signal) = message.answer_signal {
            let answer = bincode::deserialize(&signal.answer)?;
            let candidates = bincode::deserialize(&signal.candidates)?;
            let answer_set = SignalSet {
                desc: answer,
                candidates,
                remote_id: signal.answerer_id,
            };
            Ok((listener, answer_set))
        } else {
            trace!("received empty answer message.");
            answer_listener_task(listener).boxed_local().await
        }
    } else {
        Err(ClientError::ListenerClosed)
    }
}

/// Just WebRTC `tonic`-based signalling client
///
/// Compatible with both WASM and native.
#[derive(Debug)]
pub struct RtcSignallingClient {
    grpc_metadata: MetadataMap,
    #[cfg(not(target_arch = "wasm32"))]
    inner: crate::pb::rtc_signalling_client::RtcSignallingClient<tonic::transport::Channel>,
    #[cfg(target_arch = "wasm32")]
    inner: crate::pb::rtc_signalling_client::RtcSignallingClient<tonic_web_wasm_client::Client>,
}

/// Private helper methods
impl RtcSignallingClient {
    /// Private advertise helper method
    async fn advertise(&mut self) -> Result<u64, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            AdvertiseReq {},
        );
        debug!("sending advertise request");
        let response = self.inner.advertise(request).await?;
        let local_id = response.into_inner().local_peer.unwrap().id;
        debug!("received advertise response ({local_id:#016x})");
        Ok(local_id)
    }
    /// Private peer discover helper method
    async fn _peer_discover(&mut self, id: u64) -> Result<Vec<u64>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            PeerDiscoverReq {
                local_peer: Some(PeerId { id }),
            },
        );
        debug!("sending peer discover request ({id:#016x})");
        let response = self.inner.peer_discover(request).await?;
        debug!("received peer discover response ({id:#016x})");
        Ok(response
            .into_inner()
            .remote_peers
            .into_iter()
            .map(|peer| peer.id)
            .collect())
    }
    /// Private open peer listener helper method
    async fn open_peer_listener(
        &mut self,
        id: u64,
    ) -> Result<Streaming<PeerListenerRsp>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            PeerListenerReq {
                local_peer: Some(PeerId { id }),
            },
        );
        debug!("sending open peer listener request ({id:#016x})");
        let response = self.inner.open_peer_listener(request).await?;
        debug!("received open peer listener response ({id:#016x})");
        let peer_listener = response.into_inner();
        Ok(peer_listener)
    }
    /// Private open offer listener helper method
    async fn open_offer_listener(
        &mut self,
        id: u64,
    ) -> Result<Streaming<OfferListenerRsp>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            OfferListenerReq {
                local_peer: Some(PeerId { id }),
            },
        );
        debug!("sending open offer listener request ({id:#016x})");
        let response = self.inner.open_offer_listener(request).await?;
        debug!("received open offer listener response ({id:#016x})");
        let offer_listener = response.into_inner();
        Ok(offer_listener)
    }
    /// Private open answer listener helper method
    async fn open_answer_listener(
        &mut self,
        id: u64,
    ) -> Result<Streaming<AnswerListenerRsp>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            AnswerListenerReq {
                local_peer: Some(PeerId { id }),
            },
        );
        debug!("sending open answer listener request ({id:#016x})");
        let response = self.inner.open_answer_listener(request).await?;
        debug!("received open answer listener response ({id:#016x})");
        let answer_listener = response.into_inner();
        Ok(answer_listener)
    }
    /// Private signal answer helper method
    async fn signal_answer<A, C>(
        &mut self,
        id: u64,
        remote_id: u64,
        answer: A,
        candidates: C,
    ) -> ClientResult<u64>
    where
        A: serde::Serialize,
        C: serde::Serialize,
    {
        let answer_signal = SignalAnswer {
            answerer_id: id,
            candidates: bincode::serialize(&candidates)?,
            answer: bincode::serialize(&answer)?,
        };
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            SignalAnswerReq {
                offerer_peer: Some(PeerId { id: remote_id }),
                answer_signal: Some(answer_signal),
            },
        );
        debug!("sending signal answer request ({id:#016x})");
        let _response = self.inner.signal_answer(request).await?;
        debug!("received signal answer response ({id:#016x})");
        Ok(remote_id)
    }
    /// Private signal offer helper method
    async fn signal_offer<O, C>(
        &mut self,
        id: u64,
        remote_id: u64,
        offer: O,
        candidates: C,
    ) -> ClientResult<u64>
    where
        O: serde::Serialize,
        C: serde::Serialize,
    {
        let offer_signal = SignalOffer {
            offerer_id: id,
            candidates: bincode::serialize(&candidates)?,
            offer: bincode::serialize(&offer)?,
        };
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            SignalOfferReq {
                answerer_peer: Some(PeerId { id: remote_id }),
                offer_signal: Some(offer_signal),
            },
        );
        // queue signalling of offer
        debug!("sending signal offer request ({id:#016x})");
        let _response = self.inner.signal_offer(request).await?;
        debug!("received signal offer response ({id:#016x})");
        Ok(remote_id)
    }
}

/// Return type of externally implemented function creating an offer signal
///
/// Future returning the resulting offer signal
pub type CreateOfferFut<O, C, E> = Pin<Box<dyn Future<Output = Result<SignalSet<O, C>, E>>>>;

/// Return type of externally implemented function receiving an answer signal
///
/// Future returning a result with the remote peer id
pub type ReceiveAnswerFut<E> = Pin<Box<dyn Future<Output = Result<u64, E>>>>;

/// Return type of externally implemented function handling completion of local signalling
///
/// Future returning a result with the remote peer id
pub type LocalSigCpltFut<E> = Pin<Box<dyn Future<Output = Result<u64, E>>>>;

/// Return type of externally implemented function receiving an offer signal and returning an answer
///
/// Future returning the resulting answer signal
pub type ReceiveOfferFut<A, C, E> = Pin<Box<dyn Future<Output = Result<SignalSet<A, C>, E>>>>;

/// Return type of externally implemented function handling completion of remote signalling
///
/// Returns a result with the remote peer id
pub type RemoteSigCpltFut<E> = Pin<Box<dyn Future<Output = Result<u64, E>>>>;

impl RtcSignallingClient {
    /// Create the client and open connections to the signalling service.
    ///
    /// Returns resulting signalling client
    pub async fn connect(
        addr: String,
        timeout: Option<Duration>,
        tls_enabled: bool,
        domain: Option<String>,
        tls_ca_pem: Option<String>,
    ) -> ClientResult<Self> {
        // create an empty temp request to build timeout metadata
        let mut tmp_req = Request::new(());
        tmp_req.set_timeout(timeout.unwrap_or(DEFAULT_RESPONSE_DEADLINE));
        // decompose into parts to get complete grpc metadata
        let (grpc_metadata, _, _) = tmp_req.into_parts();
        // create the client
        #[cfg(not(target_arch = "wasm32"))]
        let client = {
            let endpoint = if domain.is_some() && tls_ca_pem.is_some() && tls_enabled {
                let ca_certificate = tonic::transport::Certificate::from_pem(tls_ca_pem.unwrap());
                let tls_config = tonic::transport::ClientTlsConfig::new()
                    .domain_name(domain.unwrap())
                    .ca_certificate(ca_certificate);
                let addr = format!("https://{addr}");
                tonic::transport::Channel::from_shared(addr)
                    .map_err(|_e| ClientError::InvalidUrl)?
                    .tls_config(tls_config)?
            } else {
                let addr = format!("http://{addr}");
                tonic::transport::Channel::from_shared(addr)
                    .map_err(|_e| ClientError::InvalidUrl)?
            };
            let channel = endpoint.connect().await?;
            crate::pb::rtc_signalling_client::RtcSignallingClient::new(channel)
        };
        #[cfg(target_arch = "wasm32")]
        let client = {
            if domain.is_some() || tls_ca_pem.is_some() {
                warn!("Signalling client domain/tls settings are ignored! Client TLS is handled by the browser.");
            }
            let addr = if tls_enabled {
                format!("https://{addr}")
            } else {
                format!("http://{addr}")
            };
            let client = tonic_web_wasm_client::Client::new(addr);
            crate::pb::rtc_signalling_client::RtcSignallingClient::new(client)
        };
        // advertise local peer
        // return connected client
        Ok(Self {
            grpc_metadata,
            inner: client,
        })
    }

    /// Runs the signalling client
    ///
    /// Operates the signalling chain:
    /// advertising self, listening for offers/answers, discovering peers, and performing offer/answer signalling.
    ///
    /// The externally provided callback functions are called concurrently on the local thread.
    pub async fn run<A, O, C, E>(
        &mut self,
        create_offer_fn: impl Fn(u64) -> CreateOfferFut<O, C, E>,
        receive_answer_fn: impl Fn(SignalSet<A, C>) -> ReceiveAnswerFut<E>,
        local_sig_cplt_fn: impl Fn(u64) -> LocalSigCpltFut<E>,
        receive_offer_fn: impl Fn(SignalSet<O, C>) -> ReceiveOfferFut<A, C, E>,
        remote_sig_cplt_fn: impl Fn(u64) -> RemoteSigCpltFut<E>,
    ) -> ClientResult<()>
    where
        A: serde::Serialize + serde::de::DeserializeOwned,
        O: serde::Serialize + serde::de::DeserializeOwned,
        C: serde::Serialize + serde::de::DeserializeOwned,
        ClientError: From<E>,
    {
        let id = self.advertise().await?;
        let peer_listener = self.open_peer_listener(id).await?;
        let offer_listener = self.open_offer_listener(id).await?;
        let answer_listener = self.open_answer_listener(id).await?;
        // create futures to run initially
        let peer_listener_fut = peer_listener_task(peer_listener).fuse();
        let offer_listener_fut = offer_listener_task(offer_listener).fuse();
        let answer_listener_fut = answer_listener_task(answer_listener).fuse();
        pin_mut!(peer_listener_fut, offer_listener_fut, answer_listener_fut);
        // create empty sets of futures to run later
        let mut create_offer_futs = FuturesUnordered::new();
        let mut receive_answer_futs = FuturesUnordered::new();
        let mut local_sig_cplt_futs = FuturesUnordered::new();
        let mut receive_offer_futs = FuturesUnordered::new();
        let mut remote_sig_cplt_futs = FuturesUnordered::new();
        // init empty list of discovered peers
        let mut discovered_peers: HashSet<u64> = HashSet::new();
        let mut first_discovery = true;
        debug!("signalling loop start");
        // concurrent signalling loop
        loop {
            select! {
                // Start of "local" signalling chain
                // PeerListener receives a list of remote peers.
                // On first iteration, for each remote peer, create a local peer connection...
                result = peer_listener_fut => {
                    let (listener, peer_changes) = result?;
                    debug!("peer listener task completed ({id:#016x})");
                    // reset peer listener future
                    peer_listener_fut.set(peer_listener_task(listener).fuse());
                    // apply peer changes to discovered peers map
                    for peer_change in peer_changes.iter() {
                        match peer_change.change() {
                            Change::PeerChangeAdd => {
                                if discovered_peers.insert(peer_change.id) {
                                    info!("Discovered peer ({:#016x}).", peer_change.id);
                                } else {
                                    warn!("Rediscovered peer ({:#016x}). Local discovered peers is out of sync!", peer_change.id);
                                }
                            },
                            Change::PeerChangeRemove => {
                                if discovered_peers.remove(&peer_change.id) {
                                    warn!("Remote peer ({:#016x}) dropped by signalling. Existing connections with this peer will dangle.", peer_change.id);
                                } else {
                                    warn!("Undiscovered remote peer ({:#016x}) dropped by signalling. Local discovered peers is out of sync!", peer_change.id);
                                }
                            },
                        }
                    }
                    // if this is the first discovery, queue creation of offers for each remote peer (creates local peer connections)
                    if first_discovery {
                        first_discovery = false;
                        let create_offer_tasks = peer_changes.iter()
                            .filter_map(|peer_change|
                                if peer_change.change() == Change::PeerChangeAdd {
                                    let remote_id = peer_change.id;
                                    info!("starting local signalling chain. (remote: {remote_id:#016x}");
                                    Some(create_offer_fn(remote_id))
                                } else {
                                    None
                                }
                            );
                        create_offer_futs.extend(create_offer_tasks);
                    }
                },
                // Local peer connections generate offers...
                // Offers are signalled to the remote peers...
                result = create_offer_futs.select_next_some() => {
                    debug!("create offer task completed ({id:#016x})");
                    let offer_set = result?;
                    // perform signalling of offer
                    let remote_id = self.signal_offer(id, offer_set.remote_id, offer_set.desc, offer_set.candidates).await?;
                    debug!("signalling of offer completed ({id:#016x})");
                    info!("offer signalled to peer. (remote peer: {remote_id:#016x})");
                    // do nothing, local signalling chain continues when answer listener receives an answer
                },
                // Listen for answers from the remote peers...
                result = answer_listener_fut => {
                    let (listener, answer_set) = result?;
                    debug!("answer listener task completed ({id:#016x})");
                    // reset answer listener future
                    answer_listener_fut.set(answer_listener_task(listener).fuse());
                    // queue receive answer
                    receive_answer_futs.push(receive_answer_fn(answer_set));
                },
                // We wait to receive answer responses from the remote peers,
                // Completing a "local" signalling chain.
                // And start a local signalling chain complete handler.
                result = receive_answer_futs.select_next_some() => {
                    let remote_id = result?;
                    debug!("receive answer task completed ({id:#016x})");
                    info!("local signalling chain complete. (remote peer: {remote_id:#016x})");
                    // queue local signalling complete handler
                    local_sig_cplt_futs.push(local_sig_cplt_fn(remote_id));
                },
                // Finally, the local signalling chain complete hander exits
                result = local_sig_cplt_futs.select_next_some() => {
                    let remote_id = result?;
                    info!("local signalling handler complete. (remote peer: {remote_id:#016x})");
                },

                // Start of a "remote" signalling chain
                // OfferListener receives a remote offer.
                result = offer_listener_fut => {
                    let (listener, offer_set) = result?;
                    debug!("offer listener task completed ({id:#016x})");
                    // reset offer listener future
                    offer_listener_fut.set(offer_listener_task(listener).fuse());
                    // queue receive offer
                    receive_offer_futs.push(receive_offer_fn(offer_set));
                },
                // This offer is used to create a remote peer connection and generate an answer...
                // The answer is sent to the remote offerer,
                // Completing a "remote" signalling chain.
                // And starting a remote signalling chain complete handler.
                result = receive_offer_futs.select_next_some() => {
                    debug!("receive offer task completed ({id:#016x})");
                    let answer_set = result?;
                    // perform signalling of answer
                    let remote_id = self.signal_answer(id, answer_set.remote_id, answer_set.desc, answer_set.candidates).await?;
                    debug!("signalling of answer completed ({id:#016x})");
                    trace!("remote signalling chain complete. (remote peer: {remote_id:#016x})");
                    // queue remote signalling complete handler
                    remote_sig_cplt_futs.push(remote_sig_cplt_fn(remote_id));
                },
                // Finally, the remote signalling chain complete handler exits
                result = remote_sig_cplt_futs.select_next_some() => {
                    let remote_id = result?;
                    info!("remote signalling handler complete. (remote peer: {remote_id:#016x})");
                },
                complete => break,
            }
        }

        Ok(())
    }
}
