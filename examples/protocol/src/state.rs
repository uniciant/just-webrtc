pub mod pb { neshi::include_proto!("state_pb"); }

impl prost::Name for pb::SignallingState {
    const NAME: &'static str = "SignallingState";
    const PACKAGE: &'static str = "state_pb";
}