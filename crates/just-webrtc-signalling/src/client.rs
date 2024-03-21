use std::{collections::HashSet, pin::Pin, time::Duration, future::Future};

use futures::{lock::Mutex, pin_mut, select, stream::FuturesUnordered, FutureExt, StreamExt};
use log::{debug, info, trace, warn};
use tonic::{metadata::MetadataMap, Extensions, Request, Streaming};

use crate::pb::{AdvertiseReq, AnswerListenerReq, AnswerListenerRsp, Change, OfferListenerReq, OfferListenerRsp, PeerChange, PeerDiscoverReq, PeerId, PeerListenerReq, PeerListenerRsp, SignalAnswer, SignalAnswerReq, SignalOffer, SignalOfferReq};

pub const DEFAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(10);

#[derive(thiserror::Error, Debug)]
pub enum ClientError {
    #[error(transparent)]
    Bincode(#[from] bincode::Error),
    #[error("Invalid response from server!")]
    InvalidResponse,
    #[error("Pre-existing connection with peer!")]
    PreExistingPeerConnection,
    #[error("Listener closed unexpectedly!")]
    ListenerClosed,
    #[error("Listener unavailable!")]
    ListenerUnavailable,
    #[error("Invalid URL")]
    InvalidUrl,
    #[error(transparent)]
    TonicStatus(#[from] tonic::Status),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    TonicTransport(#[from] tonic::transport::Error),
    #[error(transparent)]
    ExternalFn(#[from] anyhow::Error),
}

/// Just WebRTC client result type
pub type ClientResult<T> = Result<T, ClientError>;

/// Set of description, candidates and a remote peer ID.
pub struct SignalSet<D, C> {
    pub desc: D,
    pub candidates: C,
    pub remote_id: u64,
}

/// Private peer listener helper method
async fn peer_listener_task(mut listener: Streaming<PeerListenerRsp>) -> ClientResult<(Streaming<PeerListenerRsp>, Vec<PeerChange>)> {
    if let Some(message) = listener.message().await? {
        Ok((listener, message.peer_changes))
    } else {
        Err(ClientError::ListenerClosed)
    }
}
/// Private offer listener helper method
async fn offer_listener_task<O, C>(
    mut listener: Streaming<OfferListenerRsp>
) -> ClientResult<(Streaming<OfferListenerRsp>, SignalSet<O, C>)>
where
    O: serde::de::DeserializeOwned,
    C: serde::de::DeserializeOwned,
{
    if let Some(message) = listener.message().await? {
        let signal = message.offer_signal.ok_or(ClientError::InvalidResponse)?;
        let offer = bincode::deserialize(&signal.offer)?;
        let candidates = bincode::deserialize(&signal.candidates)?;
        let offer_set = SignalSet { desc: offer, candidates, remote_id: signal.offerer_id };
        Ok((listener, offer_set))
    } else {
        Err(ClientError::ListenerClosed)
    }
}
/// Private answer listener helper method
async fn answer_listener_task<A, C>(
    mut listener: Streaming<AnswerListenerRsp>
) -> ClientResult<(Streaming<AnswerListenerRsp>, SignalSet<A, C>)>
where
    A: serde::de::DeserializeOwned,
    C: serde::de::DeserializeOwned,
{
    if let Some(message) = listener.message().await? {
        let signal = message.answer_signal.ok_or(ClientError::InvalidResponse)?;
        let answer = bincode::deserialize(&signal.answer)?;
        let candidates = bincode::deserialize(&signal.candidates)?;
        let answer_set = SignalSet { desc: answer, candidates, remote_id: signal.answerer_id };
        Ok((listener, answer_set))
    } else  {
        Err(ClientError::ListenerClosed)
    }
}

/// Just WebRTC `tonic`-based signalling client
///
/// Compatible with both WASM and native.
pub struct RtcSignallingClient {
    grpc_metadata: MetadataMap,
    #[cfg(not(target_arch = "wasm32"))]
    inner: Mutex<crate::pb::rtc_signalling_client::RtcSignallingClient<tonic::transport::Channel>>,
    #[cfg(target_arch = "wasm32")]
    inner: Mutex<crate::pb::rtc_signalling_client::RtcSignallingClient<tonic_web_wasm_client::Client>>,
}

/// Private helper methods
impl RtcSignallingClient {
    /// Private advertise helper method
    async fn advertise(&self) -> Result<u64, tonic::Status> {
        let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(), AdvertiseReq {});
        let response = {
            let mut client = self.inner.lock().await;
            client.advertise(request).await?
        };
        let local_id = response.into_inner().local_peer.unwrap().id;
        Ok(local_id)
    }
    /// Private peer discover helper method
    async fn _peer_discover(&self, id: u64) -> Result<Vec<u64>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            PeerDiscoverReq { local_peer: Some(PeerId { id }) }
        );
        let response = {
            let mut client = self.inner.lock().await;
            client.peer_discover(request).await?
        };
        Ok(response.into_inner().remote_peers.into_iter().map(|peer| peer.id).collect())
    }
    /// Private open peer listener helper method
    async fn open_peer_listener(&self, id: u64) -> Result<Streaming<PeerListenerRsp>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            PeerListenerReq { local_peer: Some(PeerId { id }) }
        );
        let response = {
            let mut client = self.inner.lock().await;
            client.open_peer_listener(request).await?
        };
        let peer_listener = response.into_inner();
        Ok(peer_listener)
    }
    /// Private open offer listener helper method
    async fn open_offer_listener(&self, id: u64) -> Result<Streaming<OfferListenerRsp>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            OfferListenerReq { local_peer: Some(PeerId { id }) }
        );
        let response = {
            let mut client = self.inner.lock().await;
            client.open_offer_listener(request).await?
        };
        let offer_listener = response.into_inner();
        Ok(offer_listener)
    }
    /// Private open answer listener helper method
    async fn open_answer_listener(&self, id: u64) -> Result<Streaming<AnswerListenerRsp>, tonic::Status> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            AnswerListenerReq { local_peer: Some(PeerId { id }) }
        );
        let response = {
            let mut client = self.inner.lock().await;
            client.open_answer_listener(request).await?
        };
        let answer_listener = response.into_inner();
        Ok(answer_listener)
    }
    /// Private signal answer helper method
    async fn signal_answer<A, C>(
        &self,
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
        let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(),
            SignalAnswerReq {
                offerer_peer: Some(PeerId { id: remote_id }),
                answer_signal: Some(answer_signal),
            }
        );
        let _response = {
            let mut client = self.inner.lock().await;
            client.signal_answer(request).await?
        };
        Ok(remote_id)
    }
    /// Private signal offer helper method
    async fn signal_offer<O, C>(
        &self,
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
        let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(),
            SignalOfferReq {
                answerer_peer: Some(PeerId { id: remote_id }),
                offer_signal: Some(offer_signal),
            }
        );
        // queue signalling of offer
        let _response = {
            let mut client = self.inner.lock().await;
            client.signal_offer(request).await?
        };
        Ok(remote_id)
    }
}

/// Externally implemented function creating an offer signal
///
/// Returns the resulting offer signal
pub type CreateOfferFn<O, C, E> = Box<dyn Fn(u64) -> Pin<Box<dyn Future<Output = Result<SignalSet<O, C>, E>>>>>;

/// Externally implemented function receiving an answer signal
///
/// Returns a result with the remote peer id
pub type ReceiveAnswerFn<A, C, E> = Box<dyn Fn(SignalSet<A, C>) -> Pin<Box<dyn Future<Output = Result<u64, E>>>>>;

/// Externally implemented function handling completion of local signalling
///
/// Returns a result with the remote peer id
pub type LocalSigCpltFn<E> = Box<dyn Fn(u64) -> Pin<Box<dyn Future<Output = Result<u64, E>>>>>;

/// Externally implemented function receiving an offer signal and returning an answer
///
/// Returns the resulting answer signal
pub type ReceiveOfferFn<A, O, C, E> = Box<dyn Fn(SignalSet<O, C>) -> Pin<Box<dyn Future<Output = Result<SignalSet<A, C>, E>>>>>;

/// Externally implemented function handling completion of remote signalling
///
/// Returns a result with the remote peer id
pub type RemoteSigCpltFn<E> = Box<dyn Fn(u64) -> Pin<Box<dyn Future<Output = Result<u64, E>>>>>;

impl RtcSignallingClient {
    /// Create the client and open connections to the signalling service.
    ///
    /// Returns resulting signalling client
    pub async fn connect(
        addr: String,
        timeout: Option<Duration>,
        domain: Option<String>,
        tls_ca_pem: Option<String>,
    ) -> ClientResult<Self> {
        // create an empty temp request to build timeout metadata
        let mut tmp_req = Request::new(());
        tmp_req.set_timeout(timeout.unwrap_or(DEFAULT_REQUEST_DEADLINE));
        // decompose into parts to get complete grpc metadata
        let (grpc_metadata, _, _) = tmp_req.into_parts();
        // create the client
        #[cfg(not(target_arch = "wasm32"))]
        let client = {
            let endpoint = if domain.is_some() && tls_ca_pem.is_some() {
                let ca_certificate = tonic::transport::Certificate::from_pem(tls_ca_pem.unwrap());
                let tls_config = tonic::transport::ClientTlsConfig::new()
                    .domain_name(domain.unwrap())
                    .ca_certificate(ca_certificate);
                let addr = format!("https://{addr}");
                tonic::transport::Channel::from_shared(addr).map_err(|_e| ClientError::InvalidUrl)?
                    .tls_config(tls_config)?
            } else {
                let addr = format!("http://{addr}");
                tonic::transport::Channel::from_shared(addr).map_err(|_e| ClientError::InvalidUrl)?
            };
            let channel = endpoint.connect().await?;
            crate::pb::rtc_signalling_client::RtcSignallingClient::new(channel)
        };
        #[cfg(target_arch = "wasm32")]
        let client = {
            if domain.is_some() || tls_ca_pem.is_some() {
                warn!("Signalling client domain/tls settings are ignored! Client TLS is handled by the browser.");
            }
            let client = tonic_web_wasm_client::new(addr);
            crate::pb::rtc_signalling_client::RtcSignallingClient::new(client)
        };
        // advertise local peer
        // return connected client
        Ok(Self {
            grpc_metadata,
            inner: Mutex::new(client),
        })
    }

    /// Run signalling client
    ///
    /// Concurrent tasks are managed internally
    pub async fn run<A, O, C, E>(
        &self,
        create_offer_fn: CreateOfferFn<O, C, E>,
        receive_answer_fn: ReceiveAnswerFn<A, C, E>,
        local_sig_cplt_fn: LocalSigCpltFn<E>,
        receive_offer_fn: ReceiveOfferFn<A, O, C, E>,
        remote_sig_cplt_fn: RemoteSigCpltFn<E>,
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
        let mut signal_offer_futs = FuturesUnordered::new();
        let mut receive_answer_futs = FuturesUnordered::new();
        let mut local_sig_cplt_futs = FuturesUnordered::new();
        let mut receive_offer_futs = FuturesUnordered::new();
        let mut signal_answer_futs = FuturesUnordered::new();
        let mut remote_sig_cplt_futs = FuturesUnordered::new();
        // init empty list of discovered peers
        let mut discovered_peers: HashSet<u64> = HashSet::new();
        let mut first_discovery = true;
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
                result = create_offer_futs.select_next_some() => {
                    debug!("create offer task completed ({id:#016x})");
                    let offer_set = result?;
                    // queue signalling of offer
                    signal_offer_futs.push(self.signal_offer(id, offer_set.remote_id, offer_set.desc, offer_set.candidates))
                },
                // Offers are signalled to the remote peers...
                result = signal_offer_futs.select_next_some() => {
                    let remote_id = result?;
                    debug!("signal offer task completed ({id:#016x})");
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
                result = receive_offer_futs.select_next_some() => {
                    debug!("receive offer task completed ({id:#016x})");
                    let answer_set = result?;
                    // queue signalling of answer
                    signal_answer_futs.push(self.signal_answer(id, answer_set.remote_id, answer_set.desc, answer_set.candidates));
                },
                // The answer is sent to the remote offerer,
                // Completing a "remote" signalling chain.
                // And starting a remote signalling chain complete handler.
                result = signal_answer_futs.select_next_some() => {
                    let remote_id = result?;
                    debug!("signal answer task completed ({id:#016x})");
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
