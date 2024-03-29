# Just WebRTC Examples

Example implementations of `just-webrtc` and `just-webrtc-signalling`

**`signalling-peer`**:

Example utilising `just-webrtc` and `just-webrtc-signalling::client` to a create full-mesh peer-to-peer network. Peers can be run on both `native` and `wasm`.

**`signalling-server`**:

Example utilising `just-webrtc-signalling::server` to create a signalling server for each `signalling-peer` to connect with and establish the full-mesh peer-to-peer network.