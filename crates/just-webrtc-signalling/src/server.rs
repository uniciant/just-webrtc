use std::{collections::HashMap, net::SocketAddr, pin::Pin, sync::{atomic::AtomicU64, Arc}, time::Duration};

use futures_util::{Stream, StreamExt};
use log::{debug, info, warn};

use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Result, Status};

use crate::pb::{
    rtc_signalling_server::{RtcSignalling, RtcSignallingServer}, AdvertiseReq, AdvertiseRsp, AnswerListenerReq, AnswerListenerRsp, Change, OfferListenerReq, OfferListenerRsp, PeerChange, PeerDiscoverReq, PeerDiscoverRsp, PeerId, PeerListenerReq, PeerListenerRsp, SignalAnswerReq, SignalAnswerRsp, SignalOfferReq, SignalOfferRsp
};

static GENERATOR: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Listeners {
    offer_listener: RwLock<Option<mpsc::Sender<Result<OfferListenerRsp>>>>,
    answer_listener: RwLock<Option<mpsc::Sender<Result<AnswerListenerRsp>>>>,
}

/// Mapped signalling channels
pub struct Signalling {
    peers: RwLock<HashMap<u64, Listeners>>,
    peer_broadcast: broadcast::Sender<Result<PeerListenerRsp>>,
}

impl Signalling {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(16);
        Self { peers: RwLock::new(HashMap::new()), peer_broadcast: tx }
    }
}

impl Default for Signalling {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RtcSignallingService {
    inner: Arc<Signalling>,
}

impl RtcSignallingService {
    pub fn new_svc(signalling: Arc<Signalling>) -> RtcSignallingServer<RtcSignallingService> {
        RtcSignallingServer::new(RtcSignallingService { inner: signalling })
    }
}

#[tonic::async_trait]
impl RtcSignalling for RtcSignallingService {
    async fn advertise(
        &self,
        _request: Request<AdvertiseReq>
    ) -> Result<Response<AdvertiseRsp>> {
        debug!("received advertise request");
        // generate ID and insert new entry (write lock scope)
        let id = {
            let id = GENERATOR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut peers = self.inner.peers.write().await;
            if peers.contains_key(&id) {
                return Err(Status::internal("generated duplicate ID!"))
            }
            peers.insert(id, Listeners::default());
            id
        };
        // transmit change to peer listeners
        let peer_change = PeerChange { id, change: Change::PeerChangeAdd as i32 };
        match self.inner.peer_broadcast.send(Ok(PeerListenerRsp { peer_changes: vec![peer_change] })) {
            Ok(rx_count) => debug!("broadcast new peer to {rx_count} listeners"),
            Err(_) => warn!("no active peer listeners!"),
        };
        // return generated ID
        info!("new peer: {id:#016x}");
        Ok(Response::new(AdvertiseRsp { local_peer: Some(PeerId { id })}))
    }

    async fn peer_discover(
        &self,
        request: Request<PeerDiscoverReq>
    ) -> Result<Response<PeerDiscoverRsp>> {
        let local_peer = request.into_inner().local_peer.ok_or(Status::invalid_argument("missing local peer ID"))?;
        debug!("received peer discover request ({:#016x})", local_peer.id);
        let peers = self.inner.peers.read().await;
        let remote_peers = peers.iter()
            .filter_map(|(id, _)| {
                if id == &local_peer.id { None } else { Some(PeerId { id: *id }) }
            })
            .collect();
        Ok(Response::new(PeerDiscoverRsp { remote_peers }))
    }

    async fn open_peer_listener(
        &self,
        request: Request<PeerListenerReq>
    ) -> Result<Response<Self::OpenPeerListenerStream>> {
        let local_peer = request.into_inner().local_peer.ok_or(Status::invalid_argument("missing local peer ID"))?;
        debug!("received open peer listener request ({:#016x})", local_peer.id);
        // collect initial peer 'changes'
        let initial_peer_changes = {
            let peers = self.inner.peers.read().await;
            peers.iter()
                .filter_map(|(id, _)| {
                    if id == &local_peer.id { None } else { Some(PeerChange { id: *id, change: Change::PeerChangeAdd as i32 }) }
                })
                .collect()
        };
        // create stream from broadcast of received peer changes
        let mut rx = self.inner.peer_broadcast.subscribe();
        let outbound = async_stream::stream! {
            // load first result with initial peer changes
            let mut result = Ok(PeerListenerRsp { peer_changes: initial_peer_changes });
            loop {
                yield result;
                result = match rx.recv().await {
                    Ok(message) => message,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_missed)) =>
                        Err(Status::data_loss("peer listener has lagged behind. peer changes have been lost!")),
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        Err(Status::internal("peer changes broadcast is closed!"))
                    }
                };
            }
        };
        // return stream
        Ok(Response::new(outbound.boxed()))
    }
    type OpenPeerListenerStream = Pin<Box<dyn Stream<Item = Result<PeerListenerRsp>> + Send + 'static>>;

    async fn signal_offer(
        &self,
        request: Request<SignalOfferReq>,
    ) -> Result<Response<SignalOfferRsp>> {
        debug!("check");
        let message = request.into_inner();
        let answerer_peer = message.answerer_peer.ok_or(Status::invalid_argument("missing answerer peer ID"))?;
        let offer_signal = message.offer_signal.ok_or(Status::invalid_argument("missing offer signal"))?;
        debug!("received signal offer request ({:#016x})", offer_signal.offerer_id);
        // transmit offer to listener
        let peers = self.inner.peers.read().await;
        let peer = peers.get(&answerer_peer.id).ok_or(Status::failed_precondition("answerer peer not advertised!"))?;
        let offer_listener = peer.offer_listener.read().await;
        if let Some(tx) = offer_listener.as_ref() {
            if tx.send(Ok(OfferListenerRsp { offer_signal: Some(offer_signal) })).await.is_err() {
                return Err(Status::failed_precondition("offer listener receiver was dropped! peer unavailable!"));
            }
        } else {
            return Err(Status::failed_precondition("answerer peer not listening for offers!"));
        }
        info!("offer signal forwarded to answerer peer ({:#016x})", answerer_peer.id);
        Ok(Response::new(SignalOfferRsp { }))
    }

    async fn open_offer_listener(
        &self,
        request: Request<OfferListenerReq>,
    ) -> Result<Response<Self::OpenOfferListenerStream>> {
        let message = request.into_inner();
        let local_peer = message.local_peer.ok_or(Status::invalid_argument("missing local peer ID"))?;
        debug!("received open offer listener request ({:#016x})", local_peer.id);
        // create channel for transferring offer signals
        let (tx, rx) = mpsc::channel::<Result<OfferListenerRsp>>(16);
        // load channel with initial empty response
        tx.try_send(Ok(OfferListenerRsp { offer_signal: None })).unwrap();
        // create offer listener
        let peers = self.inner.peers.read().await;
        let peer = peers.get(&local_peer.id).ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        let mut offer_listener = peer.offer_listener.write().await;
        if offer_listener.replace(tx).is_some() {
            return Err(Status::already_exists("offer listener already exists!"))
        }
        // return stream
        Ok(Response::new(ReceiverStream::new(rx)))
    }
    type OpenOfferListenerStream = ReceiverStream<Result<OfferListenerRsp>>;

    async fn signal_answer(
        &self,
        request: Request<SignalAnswerReq>,
    ) -> Result<Response<SignalAnswerRsp>> {
        let message = request.into_inner();
        let offerer_peer = message.offerer_peer.ok_or(Status::invalid_argument("missing offerer peer ID"))?;
        let answer_signal = message.answer_signal.ok_or(Status::invalid_argument("missing answer signal"))?;
        debug!("received signal answer request ({:#016x})", answer_signal.answerer_id);
        // deliver answer to offerer peer
        let peers = self.inner.peers.read().await;
        let peer = peers.get(&offerer_peer.id).ok_or(Status::failed_precondition("answerer peer not advertised!"))?;
        let answer_listener = peer.answer_listener.read().await;
        if let Some(tx) = answer_listener.as_ref() {
            if tx.send(Ok(AnswerListenerRsp { answer_signal: Some(answer_signal) })).await.is_err() {
                return Err(Status::internal("answer listener receiver was dropped! peer unavailable!"));
            }
        } else {
            return Err(Status::failed_precondition("offerer peer not listening for answers!"));
        }
        info!("answer signal forwarded to offerer peer ({:#016x})", offerer_peer.id);
        Ok(Response::new(SignalAnswerRsp { }))
    }

    async fn open_answer_listener(
        &self,
        request: Request<AnswerListenerReq>,
    ) -> Result<Response<Self::OpenAnswerListenerStream>> {
        let message = request.into_inner();
        let local_peer = message.local_peer.ok_or(Status::invalid_argument("missing local peer ID"))?;
        debug!("received open answer listener request ({:#016x})", local_peer.id);
        // create channel for transferring answer signals
        let (tx, rx) = mpsc::channel::<Result<AnswerListenerRsp>>(16);
        // load channel with initial empty response
        tx.try_send(Ok(AnswerListenerRsp { answer_signal: None })).unwrap();
        // create answer listener
        let peers = self.inner.peers.read().await;
        let peer = peers.get(&local_peer.id).ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        let mut answer_listener = peer.answer_listener.write().await;
        if let Some(_tx) = answer_listener.replace(tx) {
            return Err(Status::already_exists("offer listener already exists!"))
        }
        // return stream
        Ok(Response::new(ReceiverStream::new(rx)))
    }
    type OpenAnswerListenerStream = ReceiverStream<Result<AnswerListenerRsp>>;
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
        let tls_config = tonic::transport::ServerTlsConfig::new()
            .identity(server_identity);
        builder.tls_config(tls_config)?
    } else {
        builder
    };
    // start server
    info!("Running native gRPC signalling server ({addr})");
    builder
        .add_service(rtc_signalling_svc)
        .serve(addr).await?;
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
        let tls_config = tonic::transport::ServerTlsConfig::new()
            .identity(server_identity);
        builder.tls_config(tls_config)?
    } else {
        builder
    };
    // start server
    info!("Running web gRPC signalling server ({addr})");
    builder
        .add_service(rtc_signalling_svc)
        .serve(addr).await?;
    Ok(())
}
