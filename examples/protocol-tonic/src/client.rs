use std::{collections::HashSet, pin::Pin, time::Duration};

use futures::{stream::FuturesUnordered, Future, FutureExt, StreamExt};
use just_webrtc::types::{ICECandidate, SessionDescription};
use log::{debug, info, trace, warn};
use tokio::sync::Mutex;
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
    #[error(transparent)]
    TonicStatus(#[from] tonic::Status),
    #[cfg(not(target_arch = "wasm32"))]
    #[error(transparent)]
    TonicTransport(#[from] tonic::transport::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub struct PeerListener {
    inner: Streaming<PeerListenerRsp>
}

impl PeerListener {
    /// Listen for discovery of remote peers
    ///
    /// Returns vector of all remote peer ids
    pub async fn listen(&mut self) -> Result<Vec<PeerChange>, ClientError> {
        match self.inner.message().await? {
            Some(message) => {
                Ok(message.peer_changes)
            },
            None => Err(ClientError::ListenerClosed)
        }
    }
}

pub struct SignalSet {
    pub desc: SessionDescription,
    pub candidates: Vec<ICECandidate>,
    pub remote_id: u64,
}

pub struct OfferListener {
    inner: Streaming<OfferListenerRsp>
}

impl OfferListener {
    /// Listen for offers from remote peers.
    ///
    /// Returns offer signal set
    pub async fn listen(&mut self) -> Result<SignalSet, ClientError> {
        match self.inner.message().await? {
            Some(message) => {
                let signal = message.offer_signal.ok_or(ClientError::InvalidResponse)?;
                let offer = bincode::deserialize(&signal.offer)?;
                let candidates = bincode::deserialize(&signal.candidates)?;
                Ok(SignalSet { desc: offer, candidates, remote_id: signal.offerer_id })
            },
            None => Err(ClientError::ListenerClosed)
        }
    }
}


pub struct AnswerListener {
    inner: Streaming<AnswerListenerRsp>
}

impl AnswerListener {
    /// Listen for answers from remote peers.
    ///
    /// Returns answer signal set
    pub async fn listen(&mut self) -> Result<SignalSet, ClientError> {
        match self.inner.message().await? {
            Some(message) => {
                let signal = message.answer_signal.ok_or(ClientError::InvalidResponse)?;
                let answer = bincode::deserialize(&signal.answer)?;
                let candidates = bincode::deserialize(&signal.candidates)?;
                Ok(SignalSet { desc: answer, candidates, remote_id: signal.answerer_id })
            },
            None => Err(ClientError::ListenerClosed)
        }
    }
}

pub struct RtcSignallingClient {
    local_id: u64,
    grpc_metadata: MetadataMap,
    discovered_peers: HashSet<u64>,

    peer_listener: Option<PeerListener>,
    offer_listener: Option<OfferListener>,
    answer_listener: Option<AnswerListener>,

    #[cfg(not(target_arch = "wasm32"))]
    inner: Mutex<crate::pb::rtc_signalling_client::RtcSignallingClient<tonic::transport::Channel>>,
    #[cfg(target_arch = "wasm32")]
    inner: Mutex<crate::pb::rtc_signalling_client::RtcSignallingClient<tonic_web_wasm_client::Client>>,
}

impl RtcSignallingClient {
    /// Create the client and open connections to the signalling service.
    ///
    /// Returns resulting signalling client
    pub async fn connect(
        addr: String,
        timeout: Option<Duration>,
    ) -> Result<Self, ClientError> {
        // create an empty temp request to build timeout metadata
        let mut tmp_req = Request::new(());
        tmp_req.set_timeout(timeout.unwrap_or(DEFAULT_REQUEST_DEADLINE));
        // decompose into parts to get complete grpc metadata
        let (grpc_metadata, _, _) = tmp_req.into_parts();
        // create the client
        #[cfg(not(target_arch = "wasm32"))]
        let mut client = crate::pb::rtc_signalling_client::RtcSignallingClient::<tonic::transport::Channel>::connect(addr).await?;
        #[cfg(target_arch = "wasm32")]
        let mut client = crate::pb::rtc_signalling_client::RtcSignallingClient::<tonic_web_wasm_client::Client>::new(addr);
        // advertise local peer
        let request = Request::from_parts(grpc_metadata.clone(), Extensions::default(), AdvertiseReq {});
        let response = client.advertise(request).await?;
        let local_id = response.into_inner().local_peer.ok_or(ClientError::InvalidResponse)?.id;
        // open peer listener
        let request = Request::from_parts(
            grpc_metadata.clone(),
            Extensions::default(),
            PeerListenerReq { local_peer: Some(PeerId { id: local_id }) }
        );
        let response = client.open_peer_listener(request).await?;
        let peer_listener = response.into_inner();
        // open offer listener
        let request = Request::from_parts(
            grpc_metadata.clone(),
            Extensions::default(),
            OfferListenerReq { local_peer: Some(PeerId { id: local_id }) }
        );
        let response = client.open_offer_listener(request).await?;
        let offer_listener = response.into_inner();
        // open answer listener
        let request = Request::from_parts(
            grpc_metadata.clone(),
            Extensions::default(),
            AnswerListenerReq { local_peer: Some(PeerId { id: local_id }) }
        );
        let response = client.open_answer_listener(request).await?;
        let answer_listener = response.into_inner();
        // return connected client
        Ok(Self {
            local_id,
            grpc_metadata,
            discovered_peers: HashSet::new(),
            peer_listener: Some(PeerListener { inner: peer_listener }),
            offer_listener: Some(OfferListener { inner: offer_listener }),
            answer_listener: Some(AnswerListener { inner: answer_listener }),
            inner: Mutex::new(client),
        })
    }

    /// Request list of remote peers
    ///
    /// Returns resulting vec of remote peer IDs
    pub async fn peer_discover(&mut self) -> Result<Vec<u64>, ClientError> {
        let request = Request::from_parts(
            self.grpc_metadata.clone(),
            Extensions::default(),
            PeerDiscoverReq { local_peer: Some(PeerId { id: self.local_id }) }
        );
        let response = {
            let mut client = self.inner.lock().await;
            client.peer_discover(request).await?
        };
        Ok(response.into_inner().remote_peers.into_iter().map(|peer| peer.id).collect())
    }
}

/// State machine return types
enum TaskReturnType {
    /// Start of "local" signalling chain
    ///
    /// PeerListener receives a list of remote peers.
    /// On first iteration, for each remote peer, create a local peer connection...
    PeerListener(PeerListener, Vec<PeerChange>),
    /// Local peer connections generate offers...
    CreateOffer(SignalSet),
    /// Offers are signalled to the remote peers...
    SignalOffer,
    /// Listen for answers from the remote peers...
    AnswerListener(SignalSet, AnswerListener),
    /// Finally, we wait to receive answer responses from the remote peers.
    /// Completing a "local" signalling chain
    ReceiveAnswer(u64),
    LocalSigCplt,
    /// Start of a "remote" signalling chain
    ///
    /// OfferListener receives a remote offer.
    OfferListener(SignalSet, OfferListener),
    /// This offer is used to create a remote peer connection and generate an answer...
    ReceiveOffer(SignalSet),
    /// Finally, The answer is sent to the remote offerer.
    /// Completing a "remote" signalling chain
    SignalAnswer(u64),
    RemoteSigCplt,
}

impl RtcSignallingClient {
    /// Run signalling client
    ///
    /// Concurrent tasks are managed internally
    pub async fn run
    <
        CreateOfferF,
        CreateOfferFut,
        ReceiveAnswerF,
        ReceiveAnswerFut,
        LocalSigCpltF,
        LocalSigCpltFut,
        ReceiveOfferF,
        ReceiveOfferFut,
        RemoteSigCpltF,
        RemoteSigCpltFut,
        E,
    >(
        &mut self,
        create_offer: &CreateOfferF,
        receive_answer: &ReceiveAnswerF,
        local_sig_cplt: &LocalSigCpltF,
        receive_offer: &ReceiveOfferF,
        remote_sig_cplt: &RemoteSigCpltF,
    ) -> Result<(), ClientError>
    where
        CreateOfferF: Fn(u64) -> CreateOfferFut,
        CreateOfferFut: Future<Output = Result<SignalSet, E>>,
        ReceiveAnswerF: Fn(SignalSet) -> ReceiveAnswerFut,
        ReceiveAnswerFut: Future<Output = Result<(), E>>,
        LocalSigCpltF: Fn(u64) -> LocalSigCpltFut,
        LocalSigCpltFut: Future<Output = Result<(), E>>,
        ReceiveOfferF: Fn(SignalSet) -> ReceiveOfferFut,
        ReceiveOfferFut: Future<Output = Result<SignalSet, E>>,
        RemoteSigCpltF: Fn(u64) -> RemoteSigCpltFut,
        RemoteSigCpltFut: Future<Output = Result<(), E>>,
        ClientError: From<E>,
    {
        // concurrent tasks
        let mut tasks: FuturesUnordered<Pin<Box<dyn Future<Output = Result<TaskReturnType, ClientError>>>>> = FuturesUnordered::new();
        // queue initial peer listener task
        let mut peer_listener = self.peer_listener.take().ok_or(ClientError::ListenerUnavailable)?;
        let peer_listener_task = async {
            let peer_changes = peer_listener.listen().await?;
            Ok(TaskReturnType::PeerListener(peer_listener, peer_changes))
        }.boxed_local();
        tasks.push(peer_listener_task);
        // queue initial offer listener task
        let mut offer_listener = self.offer_listener.take().ok_or(ClientError::ListenerUnavailable)?;
        let offer_listener_task = async {
            let offer_signal = offer_listener.listen().await?;
            Ok(TaskReturnType::OfferListener(offer_signal, offer_listener))
        }.boxed_local();
        tasks.push(offer_listener_task);
        // queue initial answer listener task
        let mut answer_listener = self.answer_listener.take().ok_or(ClientError::ListenerUnavailable)?;
        let answer_listener_task = async {
            let answer_signal = answer_listener.listen().await?;
            Ok(TaskReturnType::AnswerListener(answer_signal, answer_listener))
        }.boxed_local();
        tasks.push(answer_listener_task);
        // run tasks in concurrent state machine
        while let Some(result) = tasks.next().await {
            match result? {
                TaskReturnType::PeerListener(mut peer_listener, peer_changes) => {
                    debug!("peer listener task completed ({:#016x})", self.local_id);
                    // requeue peer listener task
                    let peer_listener_task = async {
                        let peer_changes = peer_listener.listen().await?;
                        Ok(TaskReturnType::PeerListener(peer_listener, peer_changes))
                    }.boxed_local();
                    tasks.push(peer_listener_task);
                    // save initial state of discovered peers list
                    let first_discovery = self.discovered_peers.is_empty();
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
                    // if this is the first discovery, queue creation of offers for each remote peer (creates local peer connections)
                    if first_discovery {
                        let create_offer_tasks = peer_changes.iter()
                            .filter_map(|peer_change|
                                if peer_change.change() == Change::PeerChangeAdd {
                                    let id = peer_change.id;
                                    let create_offer_task = async move {
                                        let offer_signal = create_offer(id).await?;
                                        Ok(TaskReturnType::CreateOffer(offer_signal))
                                    }.boxed_local();
                                    info!("starting local signalling chain. (remote: {id:#016x}");
                                    Some(create_offer_task)
                                } else {
                                    None
                                }
                            );
                        tasks.extend(create_offer_tasks);
                    }

                },
                TaskReturnType::CreateOffer(offer_set) => {
                    debug!("create offer task completed ({:#016x})", self.local_id);
                    // build signal and request
                    let offer_signal = SignalOffer {
                        offerer_id: self.local_id,
                        candidates: bincode::serialize(&offer_set.candidates)?,
                        offer: bincode::serialize(&offer_set.desc)?,
                    };
                    let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(),
                        SignalOfferReq {
                            answerer_peer: Some(PeerId { id: offer_set.remote_id }),
                            offer_signal: Some(offer_signal),
                        }
                    );
                    // queue signalling of offer
                    let signal_offer_task = async {
                        let mut client = self.inner.lock().await;
                        client.signal_offer(request).await?;
                        Ok(TaskReturnType::SignalOffer)
                    }.boxed_local();
                    tasks.push(signal_offer_task);
                },
                TaskReturnType::SignalOffer => {
                    debug!("signal offer task completed ({:#016x})", self.local_id);
                    // do nothing
                    // chain continues when answer listener receives an answer
                },
                TaskReturnType::AnswerListener(answer_set, mut answer_listener) => {
                    debug!("answer listener task completed ({:#016x})", self.local_id);
                    // requeue answer listener task
                    let answer_listener_task = async {
                        let answer_set = answer_listener.listen().await?;
                        Ok(TaskReturnType::AnswerListener(answer_set, answer_listener))
                    }.boxed_local();
                    tasks.push(answer_listener_task);
                    // receive the answer
                    let receive_answer_task = async {
                        let remote_id = answer_set.remote_id;
                        receive_answer(answer_set).await?;
                        Ok(TaskReturnType::ReceiveAnswer(remote_id))
                    }.boxed_local();
                    tasks.push(receive_answer_task);
                },
                TaskReturnType::ReceiveAnswer(remote_id) => {
                    debug!("receive answer task completed ({:#016x})", self.local_id);
                    trace!("local signalling chain complete. (remote peer: {remote_id:#016x})");
                    let local_sig_cplt_task = async move {
                        local_sig_cplt(remote_id).await?;
                        Ok(TaskReturnType::LocalSigCplt)
                    }.boxed_local();
                    tasks.push(local_sig_cplt_task);
                },
                TaskReturnType::LocalSigCplt => {},
                TaskReturnType::OfferListener(offer_set, mut offer_listener) => {
                    debug!("offer listener task completed ({:#016x})", self.local_id);
                    // requeue offer listener task
                    let offer_listener_task = async {
                        let offer_set = offer_listener.listen().await?;
                        Ok(TaskReturnType::OfferListener(offer_set, offer_listener))
                    }.boxed_local();
                    tasks.push(offer_listener_task);
                    // receive the offer
                    let receive_offer_task = async {
                        let answer_set = receive_offer(offer_set).await?;
                        Ok(TaskReturnType::ReceiveOffer(answer_set))
                    }.boxed_local();
                    tasks.push(receive_offer_task);
                },
                TaskReturnType::ReceiveOffer(answer_set) => {
                    debug!("receive offer task completed ({:#016x})", self.local_id);
                    let answer_signal = SignalAnswer {
                        answerer_id: self.local_id,
                        candidates: bincode::serialize(&answer_set.candidates)?,
                        answer: bincode::serialize(&answer_set.desc)?,
                    };
                    let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(),
                        SignalAnswerReq {
                            offerer_peer: Some(PeerId { id: answer_set.remote_id }),
                            answer_signal: Some(answer_signal),
                        }
                    );
                    let signal_answer_task = async {
                        let remote_id = request.get_ref().offerer_peer.as_ref().unwrap().id;
                        let mut client = self.inner.lock().await;
                        client.signal_answer(request).await?;
                        Ok(TaskReturnType::SignalAnswer(remote_id))
                    }.boxed_local();
                    tasks.push(signal_answer_task);
                },
                TaskReturnType::SignalAnswer(remote_id) => {
                    debug!("signal answer task completed ({:#016x})", self.local_id);
                    trace!("remote signalling chain complete. (remote peer: {remote_id:#016x})");
                    let remote_sig_cplt_task = async move {
                        remote_sig_cplt(remote_id).await?;
                        Ok(TaskReturnType::RemoteSigCplt)
                    }.boxed_local();
                    tasks.push(remote_sig_cplt_task);
                },
                TaskReturnType::RemoteSigCplt => {},
            }
        }


        Ok(())
    }
}
