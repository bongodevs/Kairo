use abi_stable::library::RootModule;
use abi_stable::sabi_trait::prelude::TD_Opaque;
use abi_stable::std_types::{RArc, ROk, RResult, RString};

use super::*;
use crate::FutureExt;
use crate::model::{PluginError, Severity};

struct Service;

impl HostService for Service {
    fn log(&self, _level: LogLevel, _message: RString) {}

    fn spawn(&self, _future: FfiFuture<()>) {}

    fn http(&self, _request: RString) -> FfiFuture<RResult<RString, PluginFailure>> {
        async move { ROk(RString::from("{}")) }.into_ffi()
    }
}

#[test]
fn abi_version_is_two_and_names_the_root_module() {
    assert_eq!(ABI_VERSION, 2);
    assert_eq!(PluginRootRef::NAME, "kairo plugin");
    assert_eq!(PluginRootRef::BASE_NAME, "kairo_plugin");
}

#[test]
fn future_payloads_are_stable_abi() {
    fn assert_stable_abi<T: abi_stable::StableAbi>() {}

    assert_stable_abi::<FfiFuture<RResult<RString, PluginFailure>>>();
    assert_stable_abi::<FfiFuture<()>>();
}

#[test]
fn failure_helpers_set_severity_and_availability() {
    assert_eq!(PluginFailure::common("missing").severity, FailureSeverity::Common);
    assert_eq!(PluginFailure::suspicious("odd").severity, FailureSeverity::Suspicious);
    assert_eq!(PluginFailure::fault("broken").severity, FailureSeverity::Fault);
    assert!(PluginFailure::unavailable("down").unavailable);
}

#[test]
fn failures_round_trip_through_the_model_error() {
    let error =
        PluginError { severity: Severity::Suspicious, message: "down".into(), unavailable: true };
    let failure = PluginFailure::from(error.clone());
    let restored = PluginError::from(failure);

    assert_eq!(restored, error);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_service_objects_are_shareable_across_threads() {
    let service: HostServiceBox = HostService_TO::from_value(Service, TD_Opaque);
    let service = RArc::new(service);

    let calls = (0..4).map(|_| {
        let service = RArc::clone(&service);
        tokio::spawn(async move {
            let response = service.http(RString::from("{}")).await;
            assert_eq!(response.unwrap().as_str(), "{}");
        })
    });
    for call in calls {
        call.await.unwrap();
    }
}

#[test]
fn log_levels_are_copyable_and_comparable() {
    let levels =
        [LogLevel::Trace, LogLevel::Debug, LogLevel::Info, LogLevel::Warn, LogLevel::Error];
    for (index, level) in levels.iter().enumerate() {
        assert_eq!(levels[index], *level);
    }
}

#[test]
fn plugin_failure_messages_survive_as_owned_strings() {
    let failure = PluginFailure::fault("plugin exploded");

    assert_eq!(failure.message.as_str(), "plugin exploded");
    assert!(!failure.unavailable);
}
