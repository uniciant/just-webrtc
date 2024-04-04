//! Just WebRTC Signalling full-mesh client for both `native` and `wasm`

extern crate alloc;

use core::{fmt::Debug, future::Future, pin::Pin, time::Duration};
use alloc::collections::BTreeSet;

use futures_util::{stream::FuturesUnordered, FutureExt, StreamExt};
use log::{debug, info, trace, warn};
use tonic::{metadata::MetadataMap, Extensions, Request, Streaming};

use crate::pb::{
    AdvertiseReq, AnswerListenerReq, AnswerListenerRsp, Change, OfferListenerReq, OfferListenerRsp,
    PeerChange, PeerDiscoverReq, PeerId, PeerListenerReq, PeerListenerRsp, SignalAnswer,
    SignalAnswerReq, SignalOffer, SignalOfferReq,
};

/// Default deadline for gRPC method response (10 seconds)
pub const DEFAULT_RESPONSE_DEADLINE: Duration = Duration::from_secs(10);

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

/// Signalling state machine states
enum SignallingState<A, O, C> {
    PeerListener(Streaming<PeerListenerRsp>, Vec<PeerChange>),
    OfferListener(Streaming<OfferListenerRsp>, SignalSet<O, C>),
    AnswerListener(Streaming<AnswerListenerRsp>, SignalSet<A, C>),
    CreateOffer(SignalSet<O, C>),
    ReceiveAnswer(u64),
    LocalSigCplt(u64),
    ReceiveOffer(SignalSet<A, C>),
    RemoteSigCplt(u64),
}

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

/// Private peer listener helper method
async fn peer_listener_task<A, O, C>(
    mut listener: Streaming<PeerListenerRsp>,
) -> ClientResult<SignallingState<A, O, C>> {
    if let Some(message) = listener.message().await? {
        Ok(SignallingState::PeerListener(listener, message.peer_changes))
    } else {
        Err(ClientError::ListenerClosed)
    }
}

/// Private offer listener helper method
async fn offer_listener_task<A, O, C>(
    mut listener: Streaming<OfferListenerRsp>,
) -> ClientResult<SignallingState<A, O, C>>
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
            Ok(SignallingState::OfferListener(listener, offer_set))
        } else {
            trace!("received empty offer message.");
            offer_listener_task(listener).boxed_local().await
        }
    } else {
        Err(ClientError::ListenerClosed)
    }
}
/// Private answer listener helper method
async fn answer_listener_task<A, O, C>(
    mut listener: Streaming<AnswerListenerRsp>,
) -> ClientResult<SignallingState<A, O, C>>
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
            Ok(SignallingState::AnswerListener(listener, answer_set))
        } else {
            trace!("received empty answer message.");
            answer_listener_task(listener).boxed_local().await
        }
    } else {
        Err(ClientError::ListenerClosed)
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

/// Private signalling state machine future
type SignallingStateFut<A, O, C> = Pin<Box<dyn Future<Output = ClientResult<SignallingState<A, O, C>>>>>;
/// A Just WebRTC Signalling peer
pub struct RtcSignallingPeer<A, O, C> {
    local_id: u64,
    discovered_peers: BTreeSet<u64>,
    first_discovery: bool,
    // wrapped callback functions
    create_offer_task: Box<dyn Fn(u64) -> SignallingStateFut<A, O, C>>,
    receive_answer_task: Box<dyn Fn(SignalSet<A, C>) -> SignallingStateFut<A, O, C>>,
    local_sig_cplt_task: Box<dyn Fn(u64) -> SignallingStateFut<A, O, C>>,
    receive_offer_task: Box<dyn Fn(SignalSet<O, C>) -> SignallingStateFut<A, O, C>>,
    remote_sig_cplt_task: Box<dyn Fn(u64) -> SignallingStateFut<A, O, C>>,
    // queued futures
    unordered_futs: FuturesUnordered<Pin<Box<dyn Future<Output = ClientResult<SignallingState<A, O, C>>>>>>,
}

impl<A, O, C> Debug for RtcSignallingPeer<A, O, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtcSignallingPeer")
            .field("local_id", &self.local_id)
            .field("discovered_peers", &self.discovered_peers)
            .field("first_discovery", &self.first_discovery)
            .field("unordered_futs", &self.unordered_futs)
            .finish()
    }
}

impl<A, O, C> RtcSignallingPeer<A, O, C>
where
    A: serde::Serialize + serde::de::DeserializeOwned + 'static,
    O: serde::Serialize + serde::de::DeserializeOwned + 'static,
    C: serde::Serialize + serde::de::DeserializeOwned + 'static,
{
    /// Private creation of a signalling peer
    fn new(
        local_id: u64,
        peer_listener: Streaming<PeerListenerRsp>,
        offer_listener: Streaming<OfferListenerRsp>,
        answer_listener: Streaming<AnswerListenerRsp>,
    ) -> Self {
        let unordered_futs = FuturesUnordered::new();
        // queue initial listener tasks
        unordered_futs.push(peer_listener_task(peer_listener).boxed_local());
        unordered_futs.push(offer_listener_task(offer_listener).boxed_local());
        unordered_futs.push(answer_listener_task(answer_listener).boxed_local());
        Self {
            local_id,
            discovered_peers: BTreeSet::new(),
            first_discovery: true,
            unordered_futs,
            // default callback tasks
            create_offer_task: Box::new(|_id| std::future::pending().boxed_local()),
            receive_answer_task: Box::new(|answer| async move { Ok(SignallingState::ReceiveAnswer(answer.remote_id)) }.boxed_local()),
            local_sig_cplt_task: Box::new(|id| async move { Ok(SignallingState::LocalSigCplt(id)) }.boxed_local()),
            receive_offer_task: Box::new(|_offer| std::future::pending().boxed_local()),
            remote_sig_cplt_task: Box::new(|id| async move { Ok(SignallingState::RemoteSigCplt(id)) }.boxed_local()),
        }
    }

    /// Set callback for handling creation of offer signals
    ///
    ///
    /// Default: never resolves, callback must be set for functional client!
    pub fn set_on_create_offer<E>(
        &mut self,
        create_offer_fn: &'static impl Fn(u64) -> CreateOfferFut<O, C, E>,
    ) -> &mut Self
    where
        E: 'static,
        ClientError: From<E>
    {
        // wrap callback
        self.create_offer_task = Box::new(|id| {
            let f = create_offer_fn(id);
            async {
                let offer = f.await?;
                Ok(SignallingState::CreateOffer(offer))
            }.boxed_local()
        });
        self
    }
    /// Set callback for handling receiving of answer signals
    ///
    /// Default: immediately resolves
    pub fn set_on_receive_answer<E>(
        &mut self,
        receive_answer_fn: &'static impl Fn(SignalSet<A, C>) -> ReceiveAnswerFut<E>,
    ) -> &mut Self
    where
        E: 'static,
        ClientError: From<E>
    {
        // wrap callback
        self.receive_answer_task = Box::new(|answer| async {
            let remote_id = receive_answer_fn(answer).await?;
            Ok(SignallingState::ReceiveAnswer(remote_id))
        }.boxed_local());
        self
    }
    /// Set callback for handling completion of a local (offerer) signalling chain
    ///
    /// Default: immediately resolves
    pub fn set_on_local_sig_cplt<E>(
        &mut self,
        local_sig_cplt_fn: &'static impl Fn(u64) -> LocalSigCpltFut<E>,
    ) -> &mut Self
    where
        E: 'static,
        ClientError: From<E>
    {
        self.local_sig_cplt_task = Box::new(|id| {
            let f = local_sig_cplt_fn(id);
            async {
                let id = f.await?;
                Ok(SignallingState::LocalSigCplt(id))
            }.boxed_local()
        });
        self
    }
    /// Set callback for handling receiving of offer signals
    ///
    /// Default: never resolves, callback must be set for functional client!
    pub fn set_on_receive_offer<E>(
        &mut self,
        receive_offer_fn: &'static impl Fn(SignalSet<O, C>) ->  ReceiveOfferFut<A, C, E>,
    ) -> &mut Self
    where
        E: 'static,
        ClientError: From<E>
    {
        self.receive_offer_task = Box::new(|offer| {
            async {
                let answer = receive_offer_fn(offer).await?;
                Ok(SignallingState::ReceiveOffer(answer))
            }.boxed_local()
        });
        self
    }
    /// Set callback for handling completion of a remote (answerer) signalling chain
    ///
    /// Default: immediately resolves
    pub fn set_on_remote_sig_cplt<E>(
        &mut self,
        remote_sig_cplt_fn: &'static impl Fn(u64) -> RemoteSigCpltFut<E>,
    ) -> &mut Self
    where
        E: 'static,
        ClientError: From<E>
    {
        self.remote_sig_cplt_task = Box::new(|id| {
            let f = remote_sig_cplt_fn(id);
            async  {
                let id = f.await?;
                Ok(SignallingState::RemoteSigCplt(id))
            }.boxed_local()
        });
        self
    }


    /// Step the signalling concurrent state machine for a peer
    pub async fn step(&mut self, client: &mut RtcSignallingClient) -> ClientResult<()> {
        let id = self.local_id;

        // await next state completion
        let state_cplt = self.unordered_futs.next().await.unwrap()?;
        // match the state, handle, and queue new states
        match state_cplt {
            // Start of "local" signalling chain
            // PeerListener receives a list of remote peers.
            // On first iteration, for each remote peer, create a local peer connection...
            SignallingState::PeerListener(listener, peer_changes) => {
                debug!("peer listener task completed ({id:#016x})");
                // reset peer listener future
                self.unordered_futs.push(peer_listener_task(listener).boxed_local());
                // apply peer changes to discovered peers map
                for peer_change in peer_changes.iter() {
                    match peer_change.change() {
                        Change::PeerChangeAdd => {
                            if self.discovered_peers.insert(peer_change.id) {
                                info!("Discovered peer ({:#016x}).", peer_change.id);
                            } else {
                                warn!("Rediscovered peer ({:#016x}). Local discovered peers is out of sync!", peer_change.id);
                            }
                        },
                        Change::PeerChangeRemove => {
                            if self.discovered_peers.remove(&peer_change.id) {
                                warn!("Remote peer ({:#016x}) dropped by signalling. Existing connections with this peer will dangle.", peer_change.id);
                            } else {
                                warn!("Undiscovered remote peer ({:#016x}) dropped by signalling. Local discovered peers is out of sync!", peer_change.id);
                            }
                        },
                    }
                }
                // if this is the first discovery
                // queue creation of offers for each remote peer (creates local peer connections)
                if self.first_discovery {
                    self.first_discovery = false;
                    let create_offer_tasks = peer_changes.iter()
                        .filter_map(|peer_change|
                            if peer_change.change() == Change::PeerChangeAdd {
                                let remote_id = peer_change.id;
                                info!("starting local signalling chain. (remote: {remote_id:#016x}");
                                Some((self.create_offer_task)(remote_id))
                            } else {
                                None
                            }
                        );
                    self.unordered_futs.extend(create_offer_tasks);
                }
            },
            // Local peer connections generate offers...
            // Offers are signalled to the remote peers...
            SignallingState::CreateOffer(offer_set) => {
                debug!("create offer task completed ({id:#016x})");
                // perform signalling of offer
                let remote_id = client.signal_offer(id, offer_set.remote_id, offer_set.desc, offer_set.candidates).await?;
                debug!("signalling of offer completed ({id:#016x})");
                info!("offer signalled to peer. (remote peer: {remote_id:#016x})");
                // do nothing, local signalling chain continues when answer listener receives an answer
            },
            // Listen for answers from the remote peers...
            SignallingState::AnswerListener(listener, answer_set) => {
                debug!("answer listener task completed ({id:#016x})");
                // requeue reset answer listener future
                // queue receive answer
                self.unordered_futs.extend([
                    answer_listener_task(listener).boxed_local(),
                    (self.receive_answer_task)(answer_set),
                ]);
            },
            // We wait to receive answer responses from the remote peers,
            // Completing a "local" signalling chain.
            // And starting a local signalling chain complete handler.
            SignallingState::ReceiveAnswer(remote_id) => {
                debug!("receive answer task completed ({id:#016x})");
                info!("local signalling chain complete. (remote peer: {remote_id:#016x})");
                // queue local signalling complete handler
                self.unordered_futs.push((self.local_sig_cplt_task)(remote_id));
            },
            // Finally, the local signalling chain complete hander exits
            SignallingState::LocalSigCplt(remote_id) => {
                debug!("local signalling cplt task completed ({id:#016x})");
                info!("local signalling handler complete. (remote peer: {remote_id:#016x})");
            },

            // Start of a "remote" signalling chain
            // OfferListener receives a remote offer.
            SignallingState::OfferListener(listener, offer_set) => {
                debug!("offer listener task completed ({id:#016x})");
                // requeue offer listener future
                // queue receive offer
                self.unordered_futs.extend([
                    offer_listener_task(listener).boxed_local(),
                    (self.receive_offer_task)(offer_set),
                ]);
            },
            // This offer is used to create a remote peer connection and generate an answer...
            // The answer is sent to the remote offerer,
            // Completing a "remote" signalling chain.
            // And starting a remote signalling chain complete handler.
            SignallingState::ReceiveOffer(answer_set) => {
                debug!("receive offer task completed ({id:#016x})");
                // perform signalling of answer
                let remote_id = client.signal_answer(id, answer_set.remote_id, answer_set.desc, answer_set.candidates).await?;
                debug!("signalling of answer completed ({id:#016x})");
                trace!("remote signalling chain complete. (remote peer: {remote_id:#016x})");
                // queue remote signalling complete handler
                self.unordered_futs.push((self.remote_sig_cplt_task)(remote_id));
            },
            // Finally, the remote signalling chain complete handler exits
            SignallingState::RemoteSigCplt(remote_id) => {
                debug!("local signalling cplt task completed ({id:#016x})");
                info!("local signalling handler complete. (remote peer: {remote_id:#016x})");
            },
        }

        Ok(())
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
    /// Private request builder
    fn build_request<M>(&self, message: M) -> Request<M> {
        Request::from_parts(self.grpc_metadata.clone(), Extensions::default(), message)
    }

    /// Private advertise helper method
    async fn advertise(&mut self) -> Result<u64, tonic::Status> {
        debug!("sending advertise request");
        let request = self.build_request(AdvertiseReq {});
        let response = self.inner.advertise(request).await?;
        let local_id = response.into_inner().local_peer.unwrap().id;
        debug!("received advertise response ({local_id:#016x})");
        Ok(local_id)
    }
    /// Private peer discover helper method
    async fn _peer_discover(&mut self, id: u64) -> Result<Vec<u64>, tonic::Status> {
        debug!("sending peer discover request ({id:#016x})");
        let request = self.build_request(
            PeerDiscoverReq {
                local_peer: Some(PeerId { id }),
            },
        );
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
        debug!("sending open offer listener request ({id:#016x})");
        let request = self.build_request(
            OfferListenerReq {
                local_peer: Some(PeerId { id }),
            },
        );
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
        debug!("sending open answer listener request ({id:#016x})");
        let request = self.build_request(
            AnswerListenerReq {
                local_peer: Some(PeerId { id }),
            },
        );
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
        debug!("sending signal answer request ({id:#016x})");
        let answer_signal = SignalAnswer {
            answerer_id: id,
            candidates: bincode::serialize(&candidates)?,
            answer: bincode::serialize(&answer)?,
        };
        let request = self.build_request(
            SignalAnswerReq {
                offerer_peer: Some(PeerId { id: remote_id }),
                answer_signal: Some(answer_signal),
            },
        );
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
        debug!("sending signal offer request ({id:#016x})");
        let offer_signal = SignalOffer {
            offerer_id: id,
            candidates: bincode::serialize(&candidates)?,
            offer: bincode::serialize(&offer)?,
        };
        let request = self.build_request(
            SignalOfferReq {
                answerer_peer: Some(PeerId { id: remote_id }),
                offer_signal: Some(offer_signal),
            },
        );
        // queue signalling of offer
        let _response = self.inner.signal_offer(request).await?;
        debug!("received signal offer response ({id:#016x})");
        Ok(remote_id)
    }
}

/// Builder of Just WebRTC signalling clients
#[derive(Debug)]
pub struct RtcSignallingClientBuilder {
    timeout: Duration,
    tls_enabled: bool,
    domain: Option<String>,
    tls_ca_pem: Option<String>,
}

impl Default for RtcSignallingClientBuilder {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_RESPONSE_DEADLINE,
            tls_enabled: false,
            domain: None,
            tls_ca_pem: None,
        }
    }
}

impl RtcSignallingClientBuilder {
    /// Set the request timeout/deadline.
    ///
    /// Default: 10 seconds
    pub fn set_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Enables TLS with provided domain and CA PEM strings
    ///
    /// Default: TLS disabled
    pub fn set_tls(mut self, domain: String, tls_ca_pem: String) -> Self {
        self.tls_enabled = true;
        self.domain = Some(domain);
        self.tls_ca_pem = Some(tls_ca_pem);
        self
    }

    /// Create a signalling client from the builder
    ///
    /// Returns resulting signalling client
    pub fn build(&self, addr: String) -> ClientResult<RtcSignallingClient> {
        let timeout = self.timeout.clone();
        // create an empty temp request to build timeout metadata
        let mut tmp_req = Request::new(());
        tmp_req.set_timeout(timeout.clone());
        // decompose into parts to get complete grpc metadata
        let (grpc_metadata, _, _) = tmp_req.into_parts();
        // create the client
        #[cfg(not(target_arch = "wasm32"))]
        let client = {
            let endpoint = if self.domain.is_some() && self.tls_ca_pem.is_some() && self.tls_enabled {
                let domain = self.domain.clone().unwrap();
                let tls_ca_pem = self.tls_ca_pem.clone().unwrap();
                let ca_certificate = tonic::transport::Certificate::from_pem(tls_ca_pem);
                let tls_config = tonic::transport::ClientTlsConfig::new()
                    .domain_name(domain)
                    .ca_certificate(ca_certificate);
                let addr = format!("https://{addr}");
                tonic::transport::Channel::from_shared(addr)
                    .map_err(|_e| ClientError::InvalidUrl)?
                    .tls_config(tls_config)?
            } else {
                let addr = format!("http://{addr}");
                tonic::transport::Channel::from_shared(addr)
                    .map_err(|_e| ClientError::InvalidUrl)?
            }
            .connect_timeout(timeout);
            let channel = endpoint.connect_lazy();
            crate::pb::rtc_signalling_client::RtcSignallingClient::new(channel)
        };
        #[cfg(target_arch = "wasm32")]
        let client = {
            if self.domain.is_some() || self.tls_ca_pem.is_some() {
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

        Ok(RtcSignallingClient {
            grpc_metadata,
            inner: client,
        })
    }
}

impl RtcSignallingClient {
    /// Start a new signalling peer
    ///
    /// Connects the peer and returns [`RtcSignallingPeer`]
    pub async fn start_peer<A, O, C>(
        &mut self
    ) -> ClientResult<RtcSignallingPeer<A, O, C>>
    where
        A: serde::Serialize + serde::de::DeserializeOwned + 'static,
        O: serde::Serialize + serde::de::DeserializeOwned + 'static,
        C: serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let local_id = self.advertise().await?;
        let peer_listener = self.open_peer_listener(local_id).await?;
        let offer_listener = self.open_offer_listener(local_id).await?;
        let answer_listener = self.open_answer_listener(local_id).await?;
        let signalling_peer = RtcSignallingPeer::new(local_id, peer_listener, offer_listener, answer_listener);
        Ok(signalling_peer)
    }
}
