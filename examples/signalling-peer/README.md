# Just WebRTC Examples: Signalling Peer

Example utilising `just-webrtc` and `just-webrtc-signalling::client` to a create full-mesh peer-to-peer network. Peers can be run on both `native` and `wasm`.

## Usage

First, run a `signalling-server` example on the localhost.

Then, run as many peers as you'd like to test. You should see them all communicating with each-other!

**Running natively:**
```sh
# install protobuf compiler
apt install -y protobuf-compiler

# run the peer binary
RUST_LOG=info cargo run --package signalling-peer
```

**Running on web:**

Serve the binary via trunk and open the provided URL in your browser!

To see the messages bouncing between peers, open the browser's developer console.

NOTE: Depending on the browser and operating system, it is sometimes difficult to test local communications between two tabs. The most reliable combination for local testing I have found is between two firefox tabs, where one is in private browsing mode.

```sh
# install protobuf compiler
apt install -y protobuf-compiler
# install the rust wasm target
rustup target add wasm32-unknown-unknown
# we will use trunk to serve the wasm binary, install it
cargo install --locked trunk

# cd to the crate directory
cd examples/signalling-peer
# serve the binary
trunk serve
```
