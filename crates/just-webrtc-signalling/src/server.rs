use std::{collections::HashMap, net::SocketAddr, sync::{atomic::AtomicU64, RwLock}, time::Duration};

use log::{debug, info};

use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::{Request, Response, Result, Status};

use crate::pb::{
    rtc_signalling_server::{RtcSignalling, RtcSignallingServer}, AdvertiseReq, AdvertiseRsp, AnswerListenerReq, AnswerListenerRsp, Change, OfferListenerReq, OfferListenerRsp, PeerChange, PeerDiscoverReq, PeerDiscoverRsp, PeerId, PeerListenerReq, PeerListenerRsp, SignalAnswerReq, SignalAnswerRsp, SignalOfferReq, SignalOfferRsp
};

static GENERATOR: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct Listeners {
    peer_listener: RwLock<Option<mpsc::UnboundedSender<Result<PeerListenerRsp>>>>,
    offer_listener: RwLock<Option<mpsc::UnboundedSender<Result<OfferListenerRsp>>>>,
    answer_listener: RwLock<Option<mpsc::UnboundedSender<Result<AnswerListenerRsp>>>>,
}

pub struct RtcSignallingService {
    peers: RwLock<HashMap<u64, Listeners>>,
}

impl RtcSignallingService {
    pub fn new_svc() -> RtcSignallingServer<RtcSignallingService> {
        RtcSignallingServer::new(RtcSignallingService { peers: RwLock::new(HashMap::new()) })
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
            let mut peers = self.peers.write().unwrap();
            if peers.contains_key(&id) {
                return Err(Status::internal("generated duplicate ID!"))
            }
            peers.insert(id, Listeners::default());
            id
        };
        // transmit change to peer listeners
        let peer_change = PeerChange { id, change: Change::PeerChangeAdd as i32 };
        let peers = self.peers.read().unwrap();
        for (_peer_id, listeners) in peers.iter() {
            let peer_listener = listeners.peer_listener.read().unwrap();
            if let Some(tx) = peer_listener.as_ref() {
                if tx.send(Ok(PeerListenerRsp { peer_changes: vec![peer_change.clone()] })).is_err() {
                    return Err(Status::internal("peer listener receiver was dropped! peer unavailable!"))
                }
            }
        }
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
        let peers = self.peers.read().unwrap();
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
        // create channel for transferring peer changes
        let (tx, rx) = mpsc::unbounded_channel();
        // send initial peers
        let peers = self.peers.read().unwrap();
        let peer_changes = peers.iter()
            .filter_map(|(id, _)| {
                if id == &local_peer.id { None } else { Some(PeerChange { id: *id, change: Change::PeerChangeAdd as i32 } )}
            })
            .collect();
        tx.send(Ok(PeerListenerRsp { peer_changes } )).unwrap();
        // create peer listener
        let peer = peers.get(&local_peer.id).ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        let mut peer_listener = peer.peer_listener.write().unwrap();
        if peer_listener.replace(tx).is_some() {
            return Err(Status::already_exists("peer listener already exists!"));
        }
        // return stream
        Ok(Response::new(UnboundedReceiverStream::new(rx)))
    }
    type OpenPeerListenerStream = UnboundedReceiverStream<Result<PeerListenerRsp>>;

    async fn signal_offer(
        &self,
        request: Request<SignalOfferReq>,
    ) -> Result<Response<SignalOfferRsp>> {
        let message = request.into_inner();
        let answerer_peer = message.answerer_peer.ok_or(Status::invalid_argument("missing answerer peer ID"))?;
        let offer_signal = message.offer_signal.ok_or(Status::invalid_argument("missing offer signal"))?;
        debug!("received signal offer request ({:#016x})", offer_signal.offerer_id);
        // transmit offer to listener
        let peers = self.peers.read().unwrap();
        let peer = peers.get(&answerer_peer.id).ok_or(Status::failed_precondition("answerer peer not advertised!"))?;
        let offer_listener = peer.offer_listener.read().unwrap();
        if let Some(tx) = offer_listener.as_ref() {
            if tx.send(Ok(OfferListenerRsp { offer_signal: Some(offer_signal) })).is_err() {
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
        let (tx, rx) = mpsc::unbounded_channel::<Result<OfferListenerRsp>>();
        // create offer listener
        let peers = self.peers.read().unwrap();
        let peer = peers.get(&local_peer.id).ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        let mut offer_listener = peer.offer_listener.write().unwrap();
        if offer_listener.replace(tx).is_some() {
            return Err(Status::already_exists("offer listener already exists!"))
        }
        // return stream
        Ok(Response::new(UnboundedReceiverStream::new(rx)))
    }
    type OpenOfferListenerStream = UnboundedReceiverStream<Result<OfferListenerRsp>>;

    async fn signal_answer(
        &self,
        request: Request<SignalAnswerReq>,
    ) -> Result<Response<SignalAnswerRsp>> {
        let message = request.into_inner();
        let offerer_peer = message.offerer_peer.ok_or(Status::invalid_argument("missing offerer peer ID"))?;
        let answer_signal = message.answer_signal.ok_or(Status::invalid_argument("missing answer signal"))?;
        debug!("received signal answer request ({:#016x})", answer_signal.answerer_id);
        // deliver answer to offerer peer
        let peers = self.peers.read().unwrap();
        let peer = peers.get(&offerer_peer.id).ok_or(Status::failed_precondition("answerer peer not advertised!"))?;
        let answer_listener = peer.answer_listener.read().unwrap();
        if let Some(tx) = answer_listener.as_ref() {
            if tx.send(Ok(AnswerListenerRsp { answer_signal: Some(answer_signal) })).is_err() {
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
        let (tx, rx) = mpsc::unbounded_channel::<Result<AnswerListenerRsp>>();
        // create answer listener
        let peers = self.peers.read().unwrap();
        let peer = peers.get(&local_peer.id).ok_or(Status::failed_precondition("listener peer not advertised!"))?;
        let mut answer_listener = peer.answer_listener.write().unwrap();
        if let Some(_tx) = answer_listener.replace(tx) {
            return Err(Status::already_exists("offer listener already exists!"))
        }
        // return stream
        Ok(Response::new(UnboundedReceiverStream::new(rx)))
    }
    type OpenAnswerListenerStream = UnboundedReceiverStream<Result<AnswerListenerRsp>>;
}

/// Start service for native clients
pub async fn serve(
    addr: SocketAddr,
    http2_keepalive_interval: Option<Duration>,
    http2_keepalive_timeout: Option<Duration>,
    tls_pem: Option<(String, String)>,
) -> Result<(), tonic::transport::Error> {
    let rtc_signalling_svc = RtcSignallingService::new_svc();
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
    addr: SocketAddr,
    http2_keepalive_interval: Option<Duration>,
    http2_keepalive_timeout: Option<Duration>,
    tls_pem: Option<(String, String)>,
) -> Result<(), tonic::transport::Error> {
    let rtc_signalling_svc = RtcSignallingService::new_svc();
    // CORS layer control
    const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
    const DEFAULT_EXPOSED_HEADERS: [&str; 3] =
        ["grpc-status", "grpc-message", "grpc-status-details-bin"];
    const DEFAULT_ALLOW_HEADERS: [&str; 5] =
        ["x-grpc-web", "content-type", "x-user-agent", "grpc-timeout", "neshi-client-id"];
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
