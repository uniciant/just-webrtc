# Just WebRTC Examples: Signalling Server

Example utilising `just-webrtc-signalling::server` to create a signalling server for each `signalling-peer` to connect with and establish the full-mesh peer-to-peer network (on localhost).

## Usage
Run the server binary in the same environment as the peer binaries:
```sh
# install protobuf compiler
apt install -y protobuf-compiler

# run the server binary
RUST_LOG=debug cargo run
```
Upon `signalling-peer` events (connection/disconnection/etc), log messages will appear depending on the selected debug level.
