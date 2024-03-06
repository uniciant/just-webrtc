pub mod pb { neshi::include_proto!("metadata_pb"); }

impl prost::Name for pb::SignallingMetadata {
    const NAME: &'static str = "SignallingMetadata";
    const PACKAGE: &'static str = "metadata_pb";
}
