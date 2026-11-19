//! SIGTERM at idle, during plugin load, and while a plugin call is still running.
//!
//! These drive the real binary because the bug they guard against lived in process wiring, not in
//! one function: the signal watcher used to be installed only once the HTTP server started, so a
//! signal during plugin load killed the process by default and left the guard marker behind.
//!
//! Needs `plugins/sources.kai` (see `crates/kairo-sources-plugin`) and Linux `kill`:
//!
//! ```text
//! cargo test --test shutdown_signals -- --ignored --test-threads=1 --nocapture
//! ```

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const PASSWORD: &str = "shutdown-test";

struct Fixture {
    dir: PathBuf,
    port: u16,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("kairo-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("plugins")).expect("plugin directory");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/sources.kai");
        if !source.is_file() {
            panic!("build the sources plugin first: {}", source.display());
        }
        fs::copy(source, dir.join("plugins/sources.kai")).expect("install plugin");

        // A free port chosen per test so the cases can run in one process.
        let port =
            TcpListener::bind("127.0.0.1:0").expect("free port").local_addr().unwrap().port();
        Self { dir, port }
    }

    fn marker(&self) -> PathBuf {
        self.dir.join("plugins/.guard/sources")
    }

    fn config(&self, plugin_config: &str) -> String {
        format!(
            "[server]\naddress = \"127.0.0.1\"\nport = {}\n\n\
             [lavalink.server]\npassword = \"{PASSWORD}\"\n\n\
             [plugins]\ndirectory = \"plugins\"\n\n\
             [[plugin]]\nname = \"sources\"\n{plugin_config}",
            self.port
        )
    }

    fn start(&self, plugin_config: &str) -> Child {
        fs::write(self.dir.join("config.toml"), self.config(plugin_config)).expect("config");
        Command::new(env!("CARGO_BIN_EXE_kairo"))
            .current_dir(&self.dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start server")
    }

    fn get(&self, path: &str) -> std::io::Result<String> {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {PASSWORD}\r\n\
             Connection: close\r\n\r\n"
        )?;
        let mut body = String::new();
        stream.read_to_string(&mut body)?;
        Ok(body)
    }

    fn wait_ready(&self, child: &mut Child) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.get("/v4/info").is_ok_and(|body| body.starts_with("HTTP/1.1 200")) {
                return;
            }
            assert!(child.try_wait().expect("poll child").is_none(), "server exited early");
            thread::sleep(Duration::from_millis(2));
        }
        panic!("server did not answer /v4/info");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn signal(child: &Child, name: &str) {
    let status = Command::new("kill")
        .arg(format!("-{name}"))
        .arg(child.id().to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{name} failed");
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> (ExitStatus, Duration) {
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Some(status) = child.try_wait().expect("poll child") {
            return (status, started.elapsed());
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = child.kill();
    panic!("server still running after {limit:?}");
}

/// No calls, no load: shutdown must not touch the drain window.
#[test]
#[ignore = "needs plugins/sources.kai and a built binary"]
fn sigterm_at_idle_exits_promptly() {
    let fixture = Fixture::new("sigterm-idle");
    let mut child = fixture.start("");

    fixture.wait_ready(&mut child);
    let started = Instant::now();
    signal(&child, "TERM");
    let (status, waited) = wait_for_exit(&mut child, Duration::from_secs(10));
    eprintln!("idle SIGTERM: exit after {waited:?}");

    assert_eq!(status.code(), Some(0), "graceful exit code");
    assert!(waited < Duration::from_secs(2), "idle shutdown took {waited:?}");
    assert!(!fixture.marker().exists(), "the guard marker survived a graceful stop");
    assert!(started.elapsed() < Duration::from_secs(12));
}

/// The signal lands while the plugin is still being loaded, before the HTTP server listens.
#[test]
#[ignore = "needs plugins/sources.kai and a built binary"]
fn sigterm_during_plugin_load_still_exits_cleanly() {
    let fixture = Fixture::new("sigterm-load");
    let mut child = fixture.start("");

    let deadline = Instant::now() + Duration::from_secs(20);
    while !fixture.marker().exists() {
        assert!(Instant::now() < deadline, "the plugin was never armed for loading");
        assert!(child.try_wait().expect("poll child").is_none(), "server exited early");
    }
    // Freeze the process so the marker cannot be removed under us, then stop it while loading.
    signal(&child, "STOP");
    assert!(fixture.marker().exists(), "guard marker disappeared before the signal");

    let started = Instant::now();
    signal(&child, "TERM");
    signal(&child, "CONT");
    let (status, waited) = wait_for_exit(&mut child, Duration::from_secs(10));
    eprintln!("load-time SIGTERM: exit after {waited:?}");

    assert_eq!(status.code(), Some(0), "a signal during load must not use the default handler");
    assert!(waited < Duration::from_secs(5), "load-time shutdown took {waited:?}");
    assert!(!fixture.marker().exists(), "the guard marker survived a signal during load");
    assert!(started.elapsed() < Duration::from_secs(12));
}

/// A plugin call is stuck on an endpoint that accepts and never answers. Shutdown must cap the
/// drain instead of inheriting the 10 s HTTP timeout.
#[test]
#[ignore = "needs plugins/sources.kai and a built binary"]
fn sigterm_during_a_slow_call_is_capped() {
    let blackhole = TcpListener::bind("127.0.0.1:0").expect("blackhole listener");
    let address = blackhole.local_addr().unwrap();
    let (accepted, reached) = mpsc::channel();
    thread::spawn(move || {
        if let Ok(stream) = blackhole.accept() {
            let _ = accepted.send(());
            thread::sleep(Duration::from_secs(60));
            drop(stream);
        }
    });

    let fixture = Fixture::new("sigterm-call");
    let mut child = fixture.start(&format!(
        "\n[plugin.config.jiosaavn]\nsecretKey = \"12345678\"\nendpoint = \"http://{address}/api.php\"\n"
    ));
    fixture.wait_ready(&mut child);
    assert!(
        fixture.get("/v4/plugins").is_ok_and(|body| body.contains("\"state\":\"loaded\"")),
        "the plugin is not loaded"
    );

    let port = fixture.port;
    thread::spawn(move || {
        let _ = TcpStream::connect(("127.0.0.1", port)).and_then(|mut stream| {
            stream.set_read_timeout(Some(Duration::from_secs(60)))?;
            write!(
                stream,
                "GET /v4/loadtracks?identifier=jssearch%3Anever HTTP/1.1\r\nHost: localhost\r\n\
                 Authorization: {PASSWORD}\r\nConnection: close\r\n\r\n"
            )?;
            let mut body = Vec::new();
            let _ = stream.read_to_end(&mut body);
            Ok(())
        });
    });
    reached
        .recv_timeout(Duration::from_secs(10))
        .expect("the plugin call never reached the endpoint");

    let started = Instant::now();
    signal(&child, "TERM");
    let (status, waited) = wait_for_exit(&mut child, Duration::from_secs(15));
    eprintln!("in-flight SIGTERM: exit after {waited:?}");

    assert_eq!(status.code(), Some(0), "graceful exit code");
    assert!(waited < Duration::from_secs(7), "shutdown waited for the hung call: {waited:?}");
    assert!(!fixture.marker().exists(), "the guard marker survived a shutdown during a call");
    assert!(started.elapsed() < Duration::from_secs(20));
}
