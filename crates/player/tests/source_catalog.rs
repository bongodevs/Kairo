use super::*;
use crate::source::{LoadFuture, SourceReference, SourceResult};
struct NamedSource(&'static str);

impl Source for NamedSource {
    fn name(&self) -> &str {
        self.0
    }

    fn load<'a>(&'a self, _reference: &'a SourceReference) -> LoadFuture<'a> {
        Box::pin(async { Ok(SourceResult::NotMine) })
    }
}

#[test]
fn default_configuration_registers_only_http() {
    let sources =
        Sources::configured(SourceConfig::default(), SourceCatalog::new()).expect("configure");
    assert_eq!(sources.names().collect::<Vec<_>>(), ["http"]);
}

#[test]
fn local_and_extensions_precede_http() {
    let mut catalog = SourceCatalog::new();
    catalog.add(NamedSource("provider-a"));
    catalog.add(NamedSource("provider-b"));
    let sources =
        Sources::configured(SourceConfig { http: true, local: true }, catalog).expect("configure");
    assert_eq!(sources.names().collect::<Vec<_>>(), ["local", "provider-a", "provider-b", "http"]);
}

#[test]
fn both_builtins_can_be_disabled() {
    let sources =
        Sources::configured(SourceConfig { http: false, local: false }, SourceCatalog::new())
            .expect("configure");
    assert!(sources.names().next().is_none());
}
