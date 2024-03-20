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

pub struct SignalSet {
    pub desc: SessionDescription,
    pub candidates: Vec<ICECandidate>,
    pub remote_id: u64,
}

async fn peer_listener_task(mut listener: Streaming<PeerListenerRsp>) -> Result<TaskReturnType, ClientError> {
    if let Some(message) = listener.message().await? {
        Ok(TaskReturnType::PeerListener(listener, message.peer_changes))
    } else {
        Err(ClientError::ListenerClosed)
    }
}

async fn offer_listener_task(mut listener: Streaming<OfferListenerRsp>) -> Result<TaskReturnType, ClientError> {
    if let Some(message) = listener.message().await? {
        let signal = message.offer_signal.ok_or(ClientError::InvalidResponse)?;
        let offer = bincode::deserialize(&signal.offer)?;
        let candidates = bincode::deserialize(&signal.candidates)?;
        let offer_set = SignalSet { desc: offer, candidates, remote_id: signal.offerer_id };
        Ok(TaskReturnType::OfferListener(offer_set, listener))
    } else {
        Err(ClientError::ListenerClosed)
    }
}

async fn answer_listener_task(mut listener: Streaming<AnswerListenerRsp>) -> Result<TaskReturnType, ClientError> {
    if let Some(message) = listener.message().await? {
        let signal = message.answer_signal.ok_or(ClientError::InvalidResponse)?;
        let answer = bincode::deserialize(&signal.answer)?;
        let candidates = bincode::deserialize(&signal.candidates)?;
        let answer_set = SignalSet { desc: answer, candidates, remote_id: signal.answerer_id };
        Ok(TaskReturnType::AnswerListener(answer_set, listener))
    } else  {
        Err(ClientError::ListenerClosed)
    }
}

pub struct RtcSignallingClient {
    grpc_metadata: MetadataMap,
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
        let client = crate::pb::rtc_signalling_client::RtcSignallingClient::<tonic::transport::Channel>::connect(addr).await?;
        #[cfg(target_arch = "wasm32")]
        let client = crate::pb::rtc_signalling_client::RtcSignallingClient::<tonic_web_wasm_client::Client>::new(addr);
        // advertise local peer
        // return connected client
        Ok(Self {
            grpc_metadata,
            inner: Mutex::new(client),
        })
    }
}

/// Private helper methods
impl RtcSignallingClient {
    async fn advertise(&self) -> Result<u64, tonic::Status> {
        let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(), AdvertiseReq {});
        let response = {
            let mut client = self.inner.lock().await;
            client.advertise(request).await?
        };
        let local_id = response.into_inner().local_peer.unwrap().id;
        Ok(local_id)
    }

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

    async fn signal_answer<A, C>(
        &self,
        id: u64,
        remote_id: u64,
        answer: &A,
        candidates: &C,
    ) -> Result<(), ClientError>
    where
        A: ?Sized + serde::Serialize,
        C: ?Sized + serde::Serialize,
    {
        let answer_signal = SignalAnswer {
            answerer_id: id,
            candidates: bincode::serialize(candidates)?,
            answer: bincode::serialize(answer)?,
        };
        let request = Request::from_parts(self.grpc_metadata.clone(), Extensions::default(),
            SignalAnswerReq {
                offerer_peer: Some(PeerId { id: remote_id }),
                answer_signal: Some(answer_signal),
            }
        );
        let _response = {
            let mut client = self.inner.lock().await;
            client.signal_answer(request).await?;
        };
        Ok(())
    }

    async fn signal_offer<O, C>(
        &self,
        id: u64,
        remote_id: u64,
        offer: &O,
        candidates: &C,
    ) -> Result<(), ClientError>
    where
        O: ?Sized + serde::Serialize,
        C: ?Sized + serde::Serialize,
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
            client.signal_offer(request).await?;
        };
        Ok(())
    }
}

/// State machine return types
enum TaskReturnType {
    /// Start of "local" signalling chain
    ///
    /// PeerListener receives a list of remote peers.
    /// On first iteration, for each remote peer, create a local peer connection...
    PeerListener(Streaming<PeerListenerRsp>, Vec<PeerChange>),
    /// Local peer connections generate offers...
    CreateOffer(SignalSet),
    /// Offers are signalled to the remote peers...
    SignalOffer,
    /// Listen for answers from the remote peers...
    AnswerListener(SignalSet, Streaming<AnswerListenerRsp>),
    /// Finally, we wait to receive answer responses from the remote peers.
    /// Completing a "local" signalling chain
    ReceiveAnswer(u64),
    LocalSigCplt,
    /// Start of a "remote" signalling chain
    ///
    /// OfferListener receives a remote offer.
    OfferListener(SignalSet, Streaming<OfferListenerRsp>),
    /// This offer is used to create a remote peer connection and generate an answer...
    ReceiveOffer(SignalSet),
    /// Finally, The answer is sent to the remote offerer.
    /// Completing a "remote" signalling chain
    SignalAnswer(u64),
    RemoteSigCplt,
}

impl RtcSignallingClient {
    async fn signal_offer_task<O, C>(
        &self,
        id: u64,
        remote_id: u64,
        offer: O,
        candidates: C,
    ) -> Result<TaskReturnType, ClientError>
    where
        O: serde::Serialize,
        C: serde::Serialize,
    {
        self.signal_offer(id, remote_id, &offer, &candidates).await?;
        Ok(TaskReturnType::SignalOffer)
    }

    async fn signal_answer_task<A, C>(
        &self,
        id: u64,
        remote_id: u64,
        answer: A,
        candidates: C,
    ) -> Result<TaskReturnType, ClientError>
    where
        A: serde::Serialize,
        C: serde::Serialize,
    {
        self.signal_answer(id, remote_id, &answer, &candidates).await?;
        Ok(TaskReturnType::SignalAnswer(remote_id))
    }

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
        &self,
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
        let id = self.advertise().await?;
        // open peer listener & queue initial listener task
        let peer_listener = self.open_peer_listener(id).await?;
        tasks.push(peer_listener_task(peer_listener).boxed_local());
        // open offer listener & queue initial listener task
        let offer_listener = self.open_offer_listener(id).await?;
        tasks.push(offer_listener_task(offer_listener).boxed_local());
        // open answer listener & queue initial listener task
        let answer_listener = self.open_answer_listener(id).await?;
        tasks.push(answer_listener_task(answer_listener).boxed_local());

        let mut discovered_peers = HashSet::new();

        // run tasks in concurrent state machine
        while let Some(result) = tasks.next().await {
            match result? {
                TaskReturnType::PeerListener(peer_listener, peer_changes) => {
                    debug!("peer listener task completed ({id:#016x})");
                    // requeue peer listener task
                    tasks.push(peer_listener_task(peer_listener).boxed_local());
                    // save initial state of discovered peers list
                    let first_discovery = discovered_peers.is_empty();
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
                        let create_offer_tasks = peer_changes.iter()
                            .filter_map(|peer_change|
                                if peer_change.change() == Change::PeerChangeAdd {
                                    let remote_id = peer_change.id;
                                    let create_offer_task = async move {
                                        let offer_signal = create_offer(remote_id).await?;
                                        Ok(TaskReturnType::CreateOffer(offer_signal))
                                    }.boxed_local();
                                    info!("starting local signalling chain. (remote: {remote_id:#016x}");
                                    Some(create_offer_task)
                                } else {
                                    None
                                }
                            );
                        tasks.extend(create_offer_tasks);
                    }

                },
                TaskReturnType::CreateOffer(offer_set) => {
                    debug!("create offer task completed ({id:#016x})");
                    // queue signalling of offer
                    tasks.push(self.signal_offer_task(
                        id,
                        offer_set.remote_id,
                        offer_set.desc,
                        offer_set.candidates
                    ).boxed_local());
                },
                TaskReturnType::SignalOffer => {
                    debug!("signal offer task completed ({id:#016x})");
                    // do nothing
                    // chain continues when answer listener receives an answer
                },
                TaskReturnType::AnswerListener(answer_set, answer_listener) => {
                    debug!("answer listener task completed ({id:#016x})");
                    // requeue answer listener task
                    tasks.push(answer_listener_task(answer_listener).boxed_local());
                    // receive the answer
                    let receive_answer_task = async {
                        let remote_id = answer_set.remote_id;
                        receive_answer(answer_set).await?;
                        Ok(TaskReturnType::ReceiveAnswer(remote_id))
                    }.boxed_local();
                    tasks.push(receive_answer_task);
                },
                TaskReturnType::ReceiveAnswer(remote_id) => {
                    debug!("receive answer task completed ({id:#016x})");
                    trace!("local signalling chain complete. (remote peer: {remote_id:#016x})");
                    let local_sig_cplt_task = async move {
                        local_sig_cplt(remote_id).await?;
                        Ok(TaskReturnType::LocalSigCplt)
                    }.boxed_local();
                    tasks.push(local_sig_cplt_task);
                },
                TaskReturnType::LocalSigCplt => {},
                TaskReturnType::OfferListener(offer_set, offer_listener) => {
                    debug!("offer listener task completed ({id:#016x})");
                    // requeue offer listener task
                    tasks.push(offer_listener_task(offer_listener).boxed_local());
                    // receive the offer
                    let receive_offer_task = async {
                        let answer_set = receive_offer(offer_set).await?;
                        Ok(TaskReturnType::ReceiveOffer(answer_set))
                    }.boxed_local();
                    tasks.push(receive_offer_task);
                },
                TaskReturnType::ReceiveOffer(answer_set) => {
                    debug!("receive offer task completed ({id:#016x})");
                    tasks.push(self.signal_answer_task(
                        id,
                        answer_set.remote_id,
                        answer_set.desc,
                        answer_set.candidates
                    ).boxed_local());
                },
                TaskReturnType::SignalAnswer(remote_id) => {
                    debug!("signal answer task completed ({id:#016x})");
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
