use serde::{Serialize, Deserialize};

/// ICECandidate is used to serialize ice candidates
#[derive(Default, Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct ICECandidate {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    pub sdp_mline_index: Option<u16>,
    pub username_fragment: Option<String>,
}

/// ICECredentialType indicates the type of credentials used to connect to
/// an ICE server.
#[derive(Default, Debug, Clone, Copy, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub enum ICECredentialType {
    /// ICECredential::Password describes username and password based
    /// credentials as described in <https://tools.ietf.org/html/rfc5389>.
    #[default]
    Password,
    /// ICECredential::Oauth describes token based credential as described
    /// in <https://tools.ietf.org/html/rfc7635>.
    /// Not supported in WebRTC 1.0 spec
    Oauth,
}

/// ICEServer describes a single STUN and TURN server that can be used by
/// the ICEAgent to establish a connection with a peer.
#[derive(Default, Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct ICEServer {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub credential_type: ICECredentialType,
}

/// ICETransportPolicy defines the ICE candidate policy surface the
/// permitted candidates. Only these candidates are used for connectivity checks.
#[derive(Default, Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize, Hash)]
pub enum ICETransportPolicy {
    /// ICETransportPolicyAll indicates any type of candidate is used.
    #[default]
    #[serde(rename = "all")]
    All,
    /// ICETransportPolicyRelay indicates only media relay candidates such
    /// as candidates passing through a TURN server are used.
    #[serde(rename = "relay")]
    Relay,
}

/// BundlePolicy affects which media tracks are negotiated if the remote
/// endpoint is not bundle-aware, and what ICE candidates are gathered. If the
/// remote endpoint is bundle-aware, all media tracks and data channels are
/// bundled onto the same transport.
#[derive(Default, Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize, Hash)]
pub enum BundlePolicy {
    /// BundlePolicyBalanced indicates to gather ICE candidates for each
    /// media type in use (audio, video, and data). If the remote endpoint is
    /// not bundle-aware, negotiate only one audio and video track on separate
    /// transports.
    #[default]
    #[serde(rename = "balanced")]
    Balanced,
    /// BundlePolicyMaxCompat indicates to gather ICE candidates for each
    /// track. If the remote endpoint is not bundle-aware, negotiate all media
    /// tracks on separate transports.
    #[serde(rename = "max-compat")]
    MaxCompat,
    /// BundlePolicyMaxBundle indicates to gather ICE candidates for only
    /// one track. If the remote endpoint is not bundle-aware, negotiate only
    /// one media track.
    #[serde(rename = "max-bundle")]
    MaxBundle,
}

/// RTCPMuxPolicy affects what ICE candidates are gathered to support
/// non-multiplexed RTCP.
#[derive(Default, Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize, Hash)]
pub enum RTCPMuxPolicy {
    /// RTCPMuxPolicyNegotiate indicates to gather ICE candidates for both
    /// RTP and RTCP candidates. If the remote-endpoint is capable of
    /// multiplexing RTCP, multiplex RTCP on the RTP candidates. If it is not,
    /// use both the RTP and RTCP candidates separately.
    #[serde(rename = "negotiate")]
    Negotiate = 1,
    /// RTCPMuxPolicyRequire indicates to gather ICE candidates only for
    /// RTP and multiplex RTCP on the RTP candidates. If the remote endpoint is
    /// not capable of rtcp-mux, session negotiation will fail.
    #[default]
    #[serde(rename = "require")]
    Require = 2,
}

/// A Configuration defines how peer-to-peer communication via PeerConnection
/// is established or re-established.
/// Configurations may be set up once and reused across multiple connections.
/// Configurations are treated as readonly. As long as they are unmodified,
/// they are safe for concurrent use.
#[derive(Default, Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct PeerConfiguration {
    /// ice_servers defines a slice describing servers available to be used by
    /// ICE, such as STUN and TURN servers.
    pub ice_servers: Vec<ICEServer>,

    /// ice_transport_policy indicates which candidates the ICEAgent is allowed
    /// to use.
    pub ice_transport_policy: ICETransportPolicy,

    /// bundle_policy indicates which media-bundling policy to use when gathering
    /// ICE candidates.
    pub bundle_policy: BundlePolicy,

    /// rtcp_mux_policy indicates which rtcp-mux policy to use when gathering ICE
    /// candidates.
    pub rtcp_mux_policy: RTCPMuxPolicy,

    /// peer_identity sets the target peer identity for the PeerConnection.
    /// The PeerConnection will not establish a connection to a remote peer
    /// unless it can be successfully authenticated with the provided name.
    pub peer_identity: String,

    /// ice_candidate_pool_size describes the size of the prefetched ICE pool.
    pub ice_candidate_pool_size: u8,
}

/// SDPType describes the type of an SessionDescription.
#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize, Hash)]
pub enum SDPType {
    /// indicates that a description MUST be treated as an SDP offer.
    #[serde(rename = "offer")]
    Offer,
    /// indicates that a description MUST be treated as an
    /// SDP answer, but not a final answer. A description used as an SDP
    /// pranswer may be applied as a response to an SDP offer, or an update to
    /// a previously sent SDP pranswer.
    #[serde(rename = "pranswer")]
    Pranswer,
    /// indicates that a description MUST be treated as an SDP
    /// final answer, and the offer-answer exchange MUST be considered complete.
    /// A description used as an SDP answer may be applied as a response to an
    /// SDP offer or as an update to a previously sent SDP pranswer.
    #[serde(rename = "answer")]
    Answer,
    /// indicates that a description MUST be treated as
    /// cancelling the current SDP negotiation and moving the SDP offer and
    /// answer back to what it was in the previous stable state. Note the
    /// local or remote SDP descriptions in the previous stable state could be
    /// null if there has not yet been a successful offer-answer negotiation.
    #[serde(rename = "rollback")]
    Rollback,
}

/// SessionDescription is used to expose local and remote session descriptions.
#[derive(Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct SessionDescription {
    #[serde(rename = "type")]
    pub sdp_type: SDPType,
    pub sdp: String,
}

/// DataChannelOptions can be used to configure properties of the underlying
/// channel such as data reliability.
#[derive(Default, Debug, Clone, Serialize, Deserialize, Hash, PartialEq, Eq)]
pub struct DataChannelOptions {
    /// ordered indicates if data is allowed to be delivered out of order. The
    /// default value of true, guarantees that data will be delivered in order.
    pub ordered: Option<bool>,

    /// max_packet_life_time limits the time (in milliseconds) during which the
    /// channel will transmit or retransmit data if not acknowledged. This value
    /// may be clamped if it exceeds the maximum value supported.
    pub max_packet_life_time: Option<u16>,

    /// max_retransmits limits the number of times a channel will retransmit data
    /// if not successfully delivered. This value may be clamped if it exceeds
    /// the maximum value supported.
    pub max_retransmits: Option<u16>,

    /// protocol describes the subprotocol name used for this channel.
    pub protocol: Option<String>,

    /// negotiated describes if the data channel is created by the local peer or
    /// the remote peer. The default value of None tells the user agent to
    /// announce the channel in-band and instruct the other peer to dispatch a
    /// corresponding DataChannel. If set to Some(id), it is up to the application
    /// to negotiate the channel and create an DataChannel with the same id
    /// at the other peer.
    pub negotiated: Option<u16>,
}

/// DataChannelState indicates the state of a data channel.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Hash)]
pub enum DataChannelState {
    /// DataChannelStateConnecting indicates that the data channel is being
    /// established. This is the initial state of DataChannel, whether created
    /// with create_data_channel, or dispatched as a part of an DataChannelEvent.
    #[serde(rename = "connecting")]
    Connecting,
    /// DataChannelStateOpen indicates that the underlying data transport is
    /// established and communication is possible.
    #[serde(rename = "open")]
    Open,
    /// DataChannelStateClosing indicates that the procedure to close down the
    /// underlying data transport has started.
    #[serde(rename = "closing")]
    Closing,
    /// DataChannelStateClosed indicates that the underlying data transport
    /// has been closed or could not be established.
    #[serde(rename = "closed")]
    Closed,
}

/// PeerConnectionState indicates the state of the PeerConnection.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum PeerConnectionState {
    /// PeerConnectionStateNew indicates that any of the ICETransports or
    /// DTLSTransports are in the "new" state and none of the transports are
    /// in the "connecting", "checking", "failed" or "disconnected" state, or
    /// all transports are in the "closed" state, or there are no transports.
    New,
    /// PeerConnectionStateConnecting indicates that any of the
    /// ICETransports or DTLSTransports are in the "connecting" or
    /// "checking" state and none of them is in the "failed" state.
    Connecting,
    /// PeerConnectionStateConnected indicates that all ICETransports and
    /// DTLSTransports are in the "connected", "completed" or "closed" state
    /// and at least one of them is in the "connected" or "completed" state.
    Connected,
    /// PeerConnectionStateDisconnected indicates that any of the
    /// ICETransports or DTLSTransports are in the "disconnected" state
    /// and none of them are in the "failed" or "connecting" or "checking" state.
    Disconnected,
    /// PeerConnectionStateFailed indicates that any of the ICETransports
    /// or DTLSTransports are in a "failed" state.
    Failed,
    /// PeerConnectionStateClosed indicates the peer connection is closed
    /// and the isClosed member variable of PeerConnection is true.
    Closed,
}
