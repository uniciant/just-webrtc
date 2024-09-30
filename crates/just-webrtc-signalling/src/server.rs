//! Just WebRTC Signalling full-mesh server/services

use crate::pb::rtc_signalling_server::{RtcSignalling, RtcSignallingServer};
use crate::pb::{
    AdvertiseReq, AdvertiseRsp, AnswerListenerReq, AnswerListenerRsp, OfferListenerReq,
    OfferListenerRsp, PeerChange, PeerDiscoverReq, PeerDiscoverRsp, PeerId, PeerListenerReq,
    PeerListenerRsp, SignalAnswerReq, SignalAnswerRsp, SignalOfferReq, SignalOfferRsp, TeardownReq,
    TeardownRsp,
};
use futures_util::{Stream, StreamExt};
use log::{debug, info, warn};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{atomic::AtomicU64, Arc, OnceLock, RwLock};
use std::time::Duration;
use tonic::{Request, Response, Result, Status};

static GENERATOR: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default)]
struct Listeners {
    offer_listener_tx: OnceLock<flume::Sender<Result<OfferListenerRsp>>>,
    answer_listener_tx: OnceLock<flume::Sender<Result<AnswerListenerRsp>>>,
}

/// Mapped signalling channels
#[derive(Debug)]
pub struct Signalling {
    peers: RwLock<HashMap<u64, Listeners>>,
    peer_broadcast_tx: async_broadcast::Sender<Result<PeerListenerRsp>>,
    peer_broadcast_rx: async_broadcast::Receiver<Result<PeerListenerRsp>>,
}

impl Signalling {
    /// Create new empty signalling channels
    pub fn new() -> Self {
        let (mut tx, rx) = async_broadcast::broadcast(16);
        tx.set_overflow(true);
        Self {
            peers: RwLock::new(HashMap::new()),
            peer_broadcast_tx: tx,
            peer_broadcast_rx: rx,
        }
    }

    /// Private teardown handler
    fn handle_teardown(&self, id: u64) -> Result<()> {
        // transmit change to peer listeners
        let peer_changes = vec![PeerChange::remove(id)];
        let message = Ok(PeerListenerRsp { peer_changes });
        match self.peer_broadcast_tx.try_broadcast(message) {
            Ok(_) => debug!("broadcast peer teardown to listeners"),
            Err(_) => warn!("no active peer listeners!"),
        };
        // remove peer from peers map
        let mut peers = self.peers.write().unwrap();
        peers
            .remove(&id)
            .ok_or(Status::failed_precondition("peer ID not advertised!"))?;
        Ok(())
    }
}

impl Default for Signalling {
    fn default() -> Self {
        Self::new()
    }
}

/// A JustWebRTC Signalling Service
///
/// This service implements robust full-mesh signalling for the creation and management of WebRTC peer-to-peer connections.
#[derive(Debug)]
pub struct RtcSignallingService {
    inner: Arc<Signalling>,
}

impl RtcSignallingService {
    /// Create new `tonic`-wrapped [`RtcSignallingService`]
    pub fn new_svc(signalling: Arc<Signalling>) -> RtcSignallingServer<RtcSignallingService> {
        RtcSignallingServer::new(RtcSignallingService { inner: signalling })
    }
}

#[tonic::async_trait]
impl RtcSignalling for RtcSignallingService {
    async fn advertise(&self, _request: Request<AdvertiseReq>) -> Result<Response<AdvertiseRsp>> {
        debug!("received advertise request");
        // generate ID and insert new entry (write lock scope)
        let id = {
            let id = GENERATOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut peers = self.inner.peers.write().unwrap();
            if peers.contains_key(&id) {
                return Err(Status::internal("generated duplicate ID!"));
            }
            peers.insert(id, Listeners::default());
            id
        };
        // transmit change to peer listeners
        let peer_changes = vec![PeerChange::add(id)];
        let message = Ok(PeerListenerRsp { peer_changes });
        match self.inner.peer_broadcast_tx.try_broadcast(message) {
            Ok(_) => debug!("broadcast new peer to listeners"),
            Err(_) => warn!("no active peer listeners!"),
        };
        // return generated ID
        info!("new peer: {id:#016x}");
        Ok(Response::new(AdvertiseRsp {
            local_peer: Some(PeerId { id }),
        }))
    }

    async fn teardown(&self, request: Request<TeardownReq>) -> Result<Response<TeardownRsp>> {
        let local_peer = request
            .into_inner()
            .local_peer
            .ok_or(Status::invalid_argument("missing local peer ID"))?;
        let id = local_peer.id;
        self.inner.handle_teardown(id)?;
        debug!("received teardown request ({id:#016x})");
        Ok(Response::new(TeardownRsp {}))
    }

    async fn peer_discover(
        &self,
        request: Request<PeerDiscoverReq>,
    ) -> Result<Response<PeerDiscoverRsp>> {
        let local_peer = request
            .into_inner()
            .local_peer
            .ok_or(Status::invalid_argument("missing local peer ID"))?;
        debug!("received peer discover request ({:#016x})", local_peer.id);
        let peers = self.inner.peers.read().unwrap();
        let remote_peers = peers
            .iter()
            .filter_map(|(id, _)| (id != &local_peer.id).then_some(PeerId { id: *id }))
            .collect();
        Ok(Response::new(PeerDiscoverRsp { remote_peers }))
    }

    async fn open_peer_listener(
        &self,
        request: Request<PeerListenerReq>,
    ) -> Result<Response<Self::OpenPeerListenerStream>> {
        let local_peer = request
            .into_inner()
            .local_peer
            .ok_or(Status::invalid_argument("missing local peer ID"))?;
        let local_id = local_peer.id;
        debug!("received open peer listener request ({local_id:#016x})");
        // collect initial peer 'changes'
        let initial_peer_changes = {
            let peers = self.inner.peers.read().unwrap();
            peers
                .iter()
                .filter_map(|(id, _)| (id != &local_peer.id).then_some(PeerChange::add(*id)))
                .collect()
        };
        // create stream from broadcast of received peer changes
        let mut rx = self.inner.peer_broadcast_rx.new_receiver();
        let outbound = async_stream::stream! {
            // load first result with initial peer changes
            let mut result = Ok(PeerListenerRsp { peer_changes: initial_peer_changes });
            loop {
                yield result;
                result = match rx.recv_direct().await {
                    Ok(message) => message,
                    Err(async_broadcast::RecvError::Overflowed(_missed)) =>
                        Err(Status::data_loss("peer listener has lagged behind. peer changes have been lost!")),
                    Err(async_broadcast::RecvError::Closed) => {
                        Err(Status::internal("peer changes broadcast is closed!"))
                    }
                };
            }
        };
        // return stream
        Ok(Response::new(outbound.boxed()))
    }
    type OpenPeerListenerStream =
        Pin<Box<dyn Stream<Item = Result<PeerListenerRsp>> + Send + 'static>>;

    async fn signal_offer(
        &self,
        request: Request<SignalOfferReq>,
    ) -> Result<Response<SignalOfferRsp>> {
        let message = request.into_inner();
        let answerer_peer = message
            .answerer_peer
            .ok_or(Status::invalid_argument("missing answerer peer ID"))?;
        let answerer_id = answerer_peer.id;
        let offer_signal = message
            .offer_signal
            .ok_or(Status::invalid_argument("missing offer signal"))?;
        let offerer_id = offer_signal.offerer_id;
        debug!("received signal offer request ({offerer_id:#016x})");
        // transmit offer to listener
        let result = {
            let peers = self.inner.peers.read().unwrap();
            let peer = peers
                .get(&answerer_id)
                .ok_or(Status::failed_precondition("answerer peer not advertised!"))?;
            let message = Ok(OfferListenerRsp {
                offer_signal: Some(offer_signal),
            });
            peer.offer_listener_tx
                .get()
                .ok_or(Status::failed_precondition("answerer peer not listening!"))?
                .try_send(message)
        };
        match result {
            Ok(_) => {
                info!("offer signal forwarded to answerer peer ({answerer_id:#016x})");
                Ok(Response::new(SignalOfferRsp {}))
            }
            Err(flume::TrySendError::Disconnected(_result)) => {
                // drop the peer
                self.inner.handle_teardown(answerer_id)?;
                Err(Status::failed_precondition(
                    "answerer peer has disconnected!",
                ))
            }
            Err(flume::TrySendError::Full(_result)) => {
                Err(Status::aborted("offer listener buffer full!"))
            }
        }
    }

    async fn open_offer_listener(
        &self,
        request: Request<OfferListenerReq>,
    ) -> Result<Response<Self::OpenOfferListenerStream>> {
        let message = request.into_inner();
        let local_peer = message
            .local_peer
            .ok_or(Status::invalid_argument("missing local peer ID"))?;
        let local_id = local_peer.id;
        debug!("received open offer listener request ({local_id:#016x})");
        // create offer listener
        let peers = self.inner.peers.read().unwrap();
        let peer = peers
            .get(&local_peer.id)
            .ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        // create new spsc channel
        let (offer_listener_tx, offer_listener_rx) = flume::bounded(16);
        // load channel with initial empty response
        offer_listener_tx
            .try_send(Ok(OfferListenerRsp { offer_signal: None }))
            .unwrap();
        // give tx side to peer map
        peer.offer_listener_tx
            .set(offer_listener_tx)
            .map_err(|_| Status::failed_precondition("offer listener already exists!"))?;
        // return new receiver stream
        Ok(Response::new(offer_listener_rx.into_stream()))
    }
    type OpenOfferListenerStream = flume::r#async::RecvStream<'static, Result<OfferListenerRsp>>;

    async fn signal_answer(
        &self,
        request: Request<SignalAnswerReq>,
    ) -> Result<Response<SignalAnswerRsp>> {
        let message = request.into_inner();
        let offerer_peer = message
            .offerer_peer
            .ok_or(Status::invalid_argument("missing offerer peer ID"))?;
        let offerer_id = offerer_peer.id;
        let answer_signal = message
            .answer_signal
            .ok_or(Status::invalid_argument("missing answer signal"))?;
        let answerer_id = answer_signal.answerer_id;
        debug!("received signal answer request ({answerer_id:#016x})");
        // transmit answer to offerer peer
        let result = {
            let peers = self.inner.peers.read().unwrap();
            let peer = peers
                .get(&offerer_id)
                .ok_or(Status::failed_precondition("offerer peer not advertised!"))?;
            let message = Ok(AnswerListenerRsp {
                answer_signal: Some(answer_signal),
            });
            peer.answer_listener_tx
                .get()
                .ok_or(Status::failed_precondition("offerer peer not listening!"))?
                .try_send(message)
        };
        match result {
            Ok(_) => {
                info!("answer signal forwarded to offerer peer ({offerer_id:#016x})");
                Ok(Response::new(SignalAnswerRsp {}))
            }
            Err(flume::TrySendError::Disconnected(_message)) => {
                // drop the peer
                self.inner.handle_teardown(offerer_id)?;
                Err(Status::failed_precondition(
                    "offerer peer has disconnected!",
                ))
            }
            Err(flume::TrySendError::Full(_message)) => {
                Err(Status::aborted("answer listener buffer full!"))
            }
        }
    }

    async fn open_answer_listener(
        &self,
        request: Request<AnswerListenerReq>,
    ) -> Result<Response<Self::OpenAnswerListenerStream>> {
        let message = request.into_inner();
        let local_peer = message
            .local_peer
            .ok_or(Status::invalid_argument("missing local peer ID"))?;
        let local_id = local_peer.id;
        debug!("received open answer listener request ({local_id:#016x})");
        // create answer listener
        let peers = self.inner.peers.read().unwrap();
        let peer = peers
            .get(&local_peer.id)
            .ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        // create new spsc channel
        let (answer_listener_tx, answer_listener_rx) = flume::bounded(16);
        // load channel with initial empty response
        answer_listener_tx
            .try_send(Ok(AnswerListenerRsp {
                answer_signal: None,
            }))
            .unwrap();
        // give tx side to peer map
        peer.answer_listener_tx
            .set(answer_listener_tx)
            .map_err(|_| Status::failed_precondition("answer listener already exists!"))?;
        // return stream
        Ok(Response::new(answer_listener_rx.into_stream()))
    }
    type OpenAnswerListenerStream = flume::r#async::RecvStream<'static, Result<AnswerListenerRsp>>;
}

/// Start service for native clients
pub async fn serve(
    signalling: Arc<Signalling>,
    addr: SocketAddr,
    http2_keepalive_interval: Option<Duration>,
    http2_keepalive_timeout: Option<Duration>,
    tls_pem: Option<(String, String)>,
) -> Result<(), tonic::transport::Error> {
    let rtc_signalling_svc = RtcSignallingService::new_svc(signalling);
    let builder = tonic::transport::Server::builder()
        .http2_keepalive_interval(http2_keepalive_interval)
        .http2_keepalive_timeout(http2_keepalive_timeout);
    // configure TLS
    let mut builder = if let Some((cert_pem, key_pem)) = tls_pem {
        let server_identity = tonic::transport::Identity::from_pem(cert_pem, key_pem);
        let tls_config = tonic::transport::ServerTlsConfig::new().identity(server_identity);
        builder.tls_config(tls_config)?
    } else {
        builder
    };
    // start server
    info!("Running native gRPC signalling server ({addr})");
    builder.add_service(rtc_signalling_svc).serve(addr).await?;
    Ok(())
}

#[cfg(feature = "server-web")]
/// Start service for web clients
pub async fn serve_web(
    signalling: Arc<Signalling>,
    addr: SocketAddr,
    http2_keepalive_interval: Option<Duration>,
    http2_keepalive_timeout: Option<Duration>,
    tls_pem: Option<(String, String)>,
) -> Result<(), tonic::transport::Error> {
    let rtc_signalling_svc = RtcSignallingService::new_svc(signalling);
    // CORS layer control
    const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
    const DEFAULT_EXPOSED_HEADERS: [&str; 3] =
        ["grpc-status", "grpc-message", "grpc-status-details-bin"];
    const DEFAULT_ALLOW_HEADERS: [&str; 4] =
        ["x-grpc-web", "content-type", "x-user-agent", "grpc-timeout"];
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
        .allow_credentials(true)
        .max_age(DEFAULT_MAX_AGE)
        .expose_headers(
            DEFAULT_EXPOSED_HEADERS
                .iter()
                .cloned()
                .map(http::HeaderName::from_static)
                .collect::<Vec<http::HeaderName>>(),
        )
        .allow_headers(
            DEFAULT_ALLOW_HEADERS
                .iter()
                .cloned()
                .map(http::HeaderName::from_static)
                .collect::<Vec<http::HeaderName>>(),
        );
    let builder = tonic::transport::Server::builder()
        .accept_http1(tls_pem.is_none())
        .layer(cors)
        .layer(tonic_web::GrpcWebLayer::new())
        .http2_keepalive_interval(http2_keepalive_interval)
        .http2_keepalive_timeout(http2_keepalive_timeout);
    // configure TLS
    let mut builder = if let Some((cert_pem, key_pem)) = tls_pem {
        let server_identity = tonic::transport::Identity::from_pem(cert_pem, key_pem);
        let tls_config = tonic::transport::ServerTlsConfig::new().identity(server_identity);
        builder.tls_config(tls_config)?
    } else {
        builder
    };
    // start server
    info!("Running web gRPC signalling server ({addr})");
    builder.add_service(rtc_signalling_svc).serve(addr).await?;
    Ok(())
}

// pb type helpers

impl PeerChange {
    /// Create new peer change as addition
    fn add(id: u64) -> Self {
        Self {
            id,
            change: crate::pb::Change::PeerChangeAdd as i32,
        }
    }

    /// Create new peer change as removal
    fn remove(id: u64) -> Self {
        Self {
            id,
            change: crate::pb::Change::PeerChangeRemove as i32,
        }
    }
}
