use super::{Environment, LayerKind, LayerSource, Platform, RawLayer, parse};

/// Embedded layer 0, parsed and validated exactly like a file. It contains
/// language definitions only; stage caps and deadlines are row-1 defaults.
pub fn built_in(platform: Platform) -> RawLayer {
    parse(
        include_bytes!("built_in.toml"),
        &LayerSource {
            kind: LayerKind::BuiltIn,
            file: "<built-in>".into(),
        },
        &Environment::default(),
        platform,
    )
}
