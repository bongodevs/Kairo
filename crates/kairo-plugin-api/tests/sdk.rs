use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Mutex as AsyncMutex;

use abi_stable::sabi_trait::prelude::TD_Opaque;
use abi_stable::std_types::{RArc, ROk, RResult, RString};
use futures_util::future::join_all;
use serde_json::{Value, json};

use super::*;
use crate::FutureExt;
use crate::abi::{HostService, HostService_TO, HostServiceBox, PluginInstance};
use crate::model::{Headers, Severity, Source, Track, TrackInfo};

static TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());
static LOGS: Mutex<Vec<(LogLevel, String)>> = Mutex::new(Vec::new());
static HTTP_REQUESTS: Mutex<Vec<HttpRequest>> = Mutex::new(Vec::new());
static SHUTDOWNS: AtomicUsize = AtomicUsize::new(0);
static FAILED_INIT_SHUTDOWNS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Fixture;

impl Plugin for Fixture {
    fn init(&mut self, host: &Host, _config: Value) -> Result<Manifest, PluginError> {
        host.info("fixture ready");
        Ok(Manifest::new("fixture", "1.0.0")
            .source(Source::new("demo").prefix("demo").host("example.test")))
    }

    async fn load(&self, _host: &Host, request: LoadRequest) -> Result<LoadResult, PluginError> {
        if request.identifier == "panic" {
            panic!("fixture panic");
        }
        let Some(query) = request.search("demo:") else {
            return Ok(LoadResult::empty());
        };
        let info = TrackInfo::new(query, query, "Fixture", request.source.as_str(), 42_000);
        Ok(LoadResult::Track(Track::new(info).plugin_data("opaque")?))
    }

    async fn open(&self, _host: &Host, request: OpenRequest) -> Result<Open, PluginError> {
        let suffix = request.track.plugin_data_ref().unwrap_or("missing");
        Ok(Open::url(format!("https://example.test/{suffix}.opus")))
    }

    fn shutdown(&mut self) {
        SHUTDOWNS.fetch_add(1, Ordering::Relaxed);
    }
}

crate::export_plugin!(Fixture);

#[derive(Default)]
struct FailingFixture;

impl Plugin for FailingFixture {
    fn init(&mut self, _host: &Host, _config: Value) -> Result<Manifest, PluginError> {
        Err(PluginError::fault("fixture init failed"))
    }

    async fn load(&self, _host: &Host, _request: LoadRequest) -> Result<LoadResult, PluginError> {
        Err(PluginError::fault("uninitialized"))
    }

    async fn open(&self, _host: &Host, _request: OpenRequest) -> Result<Open, PluginError> {
        Err(PluginError::fault("uninitialized"))
    }

    fn shutdown(&mut self) {
        FAILED_INIT_SHUTDOWNS.fetch_add(1, Ordering::Relaxed);
    }
}

struct Service;

impl HostService for Service {
    fn log(&self, level: LogLevel, message: RString) {
        LOGS.lock().unwrap().push((level, message.into_string()));
    }

    fn spawn(&self, _future: FfiFuture<()>) {}

    fn http(&self, request: RString) -> FfiFuture<RResult<RString, PluginFailure>> {
        async move {
            let request: HttpRequest = serde_json::from_str(request.as_str()).unwrap();
            let response = HttpResponse {
                status: 200,
                headers: Headers::new(),
                body: request.body.clone().unwrap_or_default(),
            };
            HTTP_REQUESTS.lock().unwrap().push(request);
            ROk(RString::from(serde_json::to_string(&response).unwrap()))
        }
        .into_ffi()
    }
}

fn service() -> RArc<HostServiceBox> {
    RArc::new(HostService_TO::from_value(Service, TD_Opaque))
}

fn create(config: &str) -> RResult<PluginInstance, PluginFailure> {
    let root = crate::sdk::root::<Fixture>();
    (root.create())(service(), RString::from(config))
}

fn instance() -> PluginInstance {
    match create("{}") {
        ROk(instance) => instance,
        RErr(error) => panic!("fixture init failed: {}", error.message),
    }
}

async fn call(plugin: &PluginBox, method: &str, request: Value) -> RResult<Value, PluginFailure> {
    let request = RString::from(serde_json::to_string(&request).unwrap());
    let response = match method {
        "source.load" => plugin.load(request),
        "track.open" => plugin.open(request),
        method => panic!("unknown fixture method {method}"),
    };
    response.await.map(|body| serde_json::from_str(body.as_str()).unwrap())
}

#[test]
fn sdk_builders_keep_required_defaults() {
    let manifest = Manifest::new("fixture", "1.0.0").source(Source::new("demo"));
    let track = TrackInfo::new("id", "title", "author", "demo", 12);

    assert_eq!(manifest.abi, crate::ABI_VERSION);
    assert_eq!(manifest.sources[0].name, "demo");
    assert!(track.is_seekable);
    assert!(!track.is_stream);
    assert_eq!(LoadResult::empty(), LoadResult::Empty(()));
}

#[test]
fn error_helpers_set_severity_and_availability() {
    assert_eq!(PluginError::common("missing").severity, Severity::Common);
    assert_eq!(PluginError::suspicious("odd").severity, Severity::Suspicious);
    assert_eq!(PluginError::fault("broken").severity, Severity::Fault);
    assert!(PluginError::unavailable("down").unavailable);
}

#[tokio::test]
async fn host_logging_and_http_request_use_the_service() {
    let _guard = TEST_LOCK.lock().await;
    LOGS.lock().unwrap().clear();
    HTTP_REQUESTS.lock().unwrap().clear();
    let host = Host::new(service());

    host.trace("trace");
    host.debug("debug");
    host.info("info");
    host.warn("warn");
    host.error("error");
    let response: Value = host
        .request(HttpMethod::Post, "https://api.example.test/items")
        .header("Authorization", "secret")
        .json_body(&json!({ "id": 7 }))
        .unwrap()
        .json()
        .await
        .unwrap();

    let logs = LOGS.lock().unwrap();
    assert_eq!(logs.len(), 5);
    assert_eq!(logs[0], (LogLevel::Trace, "trace".into()));
    let requests = HTTP_REQUESTS.lock().unwrap();
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert_eq!(requests[0].headers["Authorization"], "secret");
    assert_eq!(requests[0].body.as_deref(), Some("{\"id\":7}"));
    assert_eq!(response, json!({ "id": 7 }));
}

#[test]
fn json_response_checks_status_and_body() {
    let response = HttpResponse { status: 200, headers: Headers::new(), body: "{\"id\":7}".into() };
    let value: Value = response_json(response).unwrap();
    let status_error = response_json::<Value>(HttpResponse {
        status: 503,
        headers: Headers::new(),
        body: "{}".into(),
    })
    .unwrap_err();
    let body_error = response_json::<Value>(HttpResponse {
        status: 200,
        headers: Headers::new(),
        body: "not-json".into(),
    })
    .unwrap_err();

    assert_eq!(value["id"], 7);
    assert_eq!(status_error.severity, Severity::Suspicious);
    assert_eq!(body_error.severity, Severity::Suspicious);
}

#[test]
fn root_creates_independent_instances_and_reports_the_abi_version() {
    let _guard = TEST_LOCK.blocking_lock();
    LOGS.lock().unwrap().clear();
    let root = crate::sdk::root::<Fixture>();

    assert_eq!((root.abi_version())(), crate::ABI_VERSION);
    let first = match create("{}") {
        ROk(instance) => instance,
        RErr(error) => panic!("first init failed: {}", error.message),
    };
    let second = match create("{}") {
        ROk(instance) => instance,
        RErr(error) => panic!("second init failed: {}", error.message),
    };
    let manifest: Manifest = serde_json::from_str(first.manifest.as_str()).unwrap();

    assert_eq!(manifest.name, "fixture");
    assert_eq!(manifest.sources[0].name, "demo");
    assert_eq!(LOGS.lock().unwrap().last().unwrap().1, "fixture ready");
    drop(first);
    drop(second);
}

#[test]
fn invalid_init_config_is_a_typed_failure() {
    let error = match create("not-json") {
        ROk(_) => panic!("invalid config initialized"),
        RErr(error) => error,
    };

    assert_eq!(error.severity, crate::FailureSeverity::Fault);
    assert!(error.message.as_str().contains("invalid plugin config"));
}

#[test]
fn failed_init_still_runs_shutdown() {
    FAILED_INIT_SHUTDOWNS.store(0, Ordering::Relaxed);
    let root = crate::sdk::root::<FailingFixture>();

    let RResult::RErr(error) = (root.create())(service(), RString::from("{}")) else {
        panic!("failing fixture initialized");
    };

    assert_eq!(error.severity, crate::FailureSeverity::Fault);
    assert_eq!(FAILED_INIT_SHUTDOWNS.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn lifecycle_calls_are_concurrent_and_panics_are_contained() {
    let _guard = TEST_LOCK.lock().await;
    SHUTDOWNS.store(0, Ordering::Relaxed);
    let instance = instance();
    let mut plugin = instance.plugin;

    assert_eq!(
        call(&plugin, "source.load", json!({})).await.unwrap_err().severity,
        crate::FailureSeverity::Suspicious
    );
    let loaded =
        call(&plugin, "source.load", json!({ "source": "demo", "identifier": "demo:hello" }))
            .await
            .unwrap();
    assert_eq!(loaded["loadType"], "track");
    assert_eq!(loaded["data"]["pluginData"], "opaque");

    let calls = (0..4).map(|index| {
        let plugin = &plugin;
        async move {
            call(
                plugin,
                "source.load",
                json!({ "source": "demo", "identifier": format!("demo:{index}") }),
            )
            .await
            .is_ok()
        }
    });
    for ok in join_all(calls).await {
        assert!(ok);
    }

    let opened =
        call(&plugin, "track.open", json!({ "source": "demo", "track": loaded["data"].clone() }))
            .await
            .unwrap();
    assert_eq!(opened["kind"], "url");
    assert_eq!(opened["url"], "https://example.test/opaque.opus");

    assert_eq!(
        call(&plugin, "source.load", json!({ "source": "demo", "identifier": "panic" }))
            .await
            .unwrap_err()
            .severity,
        crate::FailureSeverity::Fault
    );
    assert!(
        call(&plugin, "source.load", json!({ "source": "demo", "identifier": "demo:again" }))
            .await
            .is_ok()
    );

    plugin.shutdown();
    assert_eq!(SHUTDOWNS.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn empty_load_result_round_trips_as_json() {
    let _guard = TEST_LOCK.lock().await;
    let instance = instance();
    let loaded =
        call(&instance.plugin, "source.load", json!({ "source": "demo", "identifier": "missing" }))
            .await
            .unwrap();

    assert_eq!(loaded["loadType"], "empty");
    assert!(loaded["data"].is_null());
}
