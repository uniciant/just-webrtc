use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use just_webrtc_signalling::{
    client::RtcSignallingClientBuilder,
    server::{serve, Signalling},
    DEFAULT_NATIVE_SERVER_ADDR,
};

const PHONY_OFFER: &str = "offer";
const PHONY_OFFERER_CANDIDATES: &str = "offerer candidates";
const PHONY_ANSWER: &str = "answer";
const PHONY_ANSWER_CANDIDATES: &str = "answerer candidates";

async fn run_peer(local: bool) -> Result<()> {
    static LOCAL_SIGNALLING_COMPLETE: AtomicBool = AtomicBool::new(false);
    static REMOTE_SIGNALLING_COMPLETE: AtomicBool = AtomicBool::new(false);

    // create offer testing callback
    async fn create_offer(remote_id: u64) -> Result<(u64, (String, String))> {
        let offer = PHONY_OFFER.to_string();
        let candidates = PHONY_OFFERER_CANDIDATES.to_string();
        Ok((remote_id, (offer, candidates)))
    }
    // receive answer testing callback
    async fn receive_answer(remote_id: u64, answer_set: (String, String)) -> Result<u64> {
        assert_eq!(&answer_set.0, PHONY_ANSWER);
        assert_eq!(&answer_set.1, PHONY_ANSWER_CANDIDATES);
        Ok(remote_id)
    }
    // local signalling complete testing callback
    async fn local_sig_cplt(remote_id: u64) -> Result<u64> {
        LOCAL_SIGNALLING_COMPLETE.store(true, Ordering::Relaxed);
        Ok(remote_id)
    }
    // receive offer testing callback
    async fn receive_offer(
        remote_id: u64,
        offer_set: (String, String),
    ) -> Result<(u64, (String, String))> {
        assert_eq!(&offer_set.0, PHONY_OFFER);
        assert_eq!(&offer_set.1, PHONY_OFFERER_CANDIDATES);
        let answer = PHONY_ANSWER.to_string();
        let candidates = PHONY_ANSWER_CANDIDATES.to_string();
        Ok((remote_id, (answer, candidates)))
    }
    // remote signalling complete testing callback
    async fn remote_sig_cplt(remote_id: u64) -> Result<u64> {
        REMOTE_SIGNALLING_COMPLETE.store(true, Ordering::Relaxed);
        Ok(remote_id)
    }

    // build signalling client
    let signalling_client =
        RtcSignallingClientBuilder::default().build(DEFAULT_NATIVE_SERVER_ADDR.to_string())?;
    // start peer
    let mut signalling_peer = signalling_client.start_peer().await?;
    // set callbacks
    signalling_peer.set_on_create_offer(create_offer);
    signalling_peer.set_on_receive_answer(receive_answer);
    signalling_peer.set_on_local_sig_cplt(local_sig_cplt);
    signalling_peer.set_on_receive_offer(receive_offer);
    signalling_peer.set_on_remote_sig_cplt(remote_sig_cplt);
    // run signalling peer
    loop {
        if local && LOCAL_SIGNALLING_COMPLETE.load(Ordering::Relaxed)
            || !local && REMOTE_SIGNALLING_COMPLETE.load(Ordering::Relaxed)
        {
            signalling_peer.close().await?;
            return Ok(());
        }
        signalling_peer.step().await?;
    }
}

#[cfg(all(feature = "server", feature = "client"))]
#[tokio::test]
async fn test_signalling() -> Result<()> {
    use std::sync::Arc;
    pretty_env_logger::try_init()?;

    let signalling = Arc::new(Signalling::new());
    // start server
    let server_handle = tokio::spawn(serve(
        signalling,
        DEFAULT_NATIVE_SERVER_ADDR.parse()?,
        None,
        None,
        None,
    ));
    // start peers A (local) & B (remote)
    let peer_a_fut = run_peer(true);
    let peer_b_fut = run_peer(false);
    // try run both peers to completion
    tokio::try_join!(peer_a_fut, peer_b_fut)?;
    // grab any error from server handle
    if server_handle.is_finished() {
        server_handle.await??;
    }
    Ok(())
}
