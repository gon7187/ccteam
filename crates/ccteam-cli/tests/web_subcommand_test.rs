//! V0.3 M5.0 — `ccteam internal web` subcommand integration test.
//!
//! Confirms the CLI surface end-to-end:
//!
//! 1. `ccteam internal web --help` advertises the four flags (bind /
//!    DSH companion bind / no-auth / token-file) so a typo doesn't silently
//!    drop one.
//! 2. `ccteam internal web --bind 127.0.0.1:0 --dsh-web-bind off --no-auth`
//!    spawns, prints the bound address on stdout, serves `GET /health` 200 +
//!    JSON, and can be terminated by `child.kill()`.
//!
//! The lib-level `serve` round-trip (in-process axum + reqwest) is
//! covered in `crates/ccteam-web/src/lib.rs` tests; this file
//! exercises the `ccteam internal web …` subprocess entry so a
//! regression in the clap → ServeOpts → serve plumbing surfaces here.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn ccteam_internal_web_help_advertises_flags() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let out = Command::new(bin)
        .args(["internal", "web", "--help"])
        .output()
        .expect("spawn ccteam internal web --help");
    assert!(
        out.status.success(),
        "ccteam internal web --help should exit 0"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for flag in ["--bind", "--dsh-web-bind", "--no-auth", "--token-file"] {
        assert!(
            stdout.contains(flag),
            "ccteam internal web --help should mention {flag}; got: {stdout}",
        );
    }
}

#[test]
fn ccteam_web_serves_health_then_exits_when_killed() {
    let bin = env!("CARGO_BIN_EXE_ccteam");
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(bin)
        .args([
            "internal",
            "web",
            "--bind",
            "127.0.0.1:0",
            "--dsh-web-bind",
            "off",
            "--no-auth",
        ])
        .env("HOME", temp.path().join("home"))
        .env("CCTEAM_HOME", temp.path().join("ccteam-home"))
        .env("CCTEAM_PROJECTS_ROOT", temp.path().join("projects"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ccteam web");

    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);

    // Server prints `ccteam web listening on http://<addr>` as its
    // first stdout line. Read with a deadline so a regression in the
    // bind announcement doesn't hang the test forever.
    let mut bind_line = String::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        bind_line.clear();
        match reader.read_line(&mut bind_line) {
            Ok(0) => break,
            Ok(_) if bind_line.contains("listening on http://") => break,
            Ok(_) => continue,
            Err(err) => {
                let _ = child.kill();
                panic!("read stdout: {err}");
            }
        }
    }
    if !bind_line.contains("listening on http://") {
        let _ = child.kill();
        panic!("server must announce bind address on stdout; got: {bind_line:?}");
    }
    let addr = bind_line
        .split("http://")
        .nth(1)
        .expect("bind line missing http://")
        .trim()
        .to_string();
    assert!(!addr.is_empty(), "bind addr empty");

    // GET /health — block until 200 or deadline.
    let url = format!("http://{addr}/health");
    let body = match stdlib_http_get(&url, Duration::from_secs(5)) {
        Ok(b) => b,
        Err(err) => {
            let _ = child.kill();
            panic!("GET {url} failed: {err}");
        }
    };
    assert!(
        body.contains("\"status\""),
        "/health body should contain status field; got: {body}",
    );
    assert!(
        body.contains("\"ok\""),
        "/health body should contain status=ok; got: {body}",
    );

    // Kill the child (SIGKILL on Unix). Clean SIGTERM-driven shutdown
    // is exercised at the lib level via the in-process axum test in
    // `ccteam-web`; here we only need to confirm the subprocess
    // shape is sane (no orphan, no hang).
    let _ = child.kill();
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if Instant::now() < exit_deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => panic!("ccteam web did not exit within 10s of kill"),
            Err(err) => panic!("try_wait: {err}"),
        }
    }
}

/// Minimal HTTP GET — pure stdlib + sockets to avoid a new dev-dep.
/// Returns the response body or an error string. Retries once per
/// 100ms until `deadline`.
fn stdlib_http_get(url: &str, deadline: Duration) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let start = Instant::now();
    let host_path = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("only http:// supported; got: {url}"))?;
    let (host, path) = host_path.split_once('/').unwrap_or((host_path, ""));
    let path = format!("/{path}");

    loop {
        match TcpStream::connect(host) {
            Ok(mut stream) => {
                let attempt = (|| -> Result<String, String> {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .map_err(|e| e.to_string())?;
                    let req = format!(
                        "GET {path} HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n",
                    );
                    stream
                        .write_all(req.as_bytes())
                        .map_err(|e| e.to_string())?;
                    let mut buf = String::new();
                    stream.read_to_string(&mut buf).map_err(|e| e.to_string())?;
                    Ok(buf
                        .split_once("\r\n\r\n")
                        .map(|(_, body)| body.to_string())
                        .unwrap_or(buf))
                })();
                match attempt {
                    Ok(body) => return Ok(body),
                    Err(_) if start.elapsed() < deadline => {
                        thread::sleep(Duration::from_millis(100));
                    }
                    Err(err) => return Err(format!("request {host}{path}: {err}")),
                }
            }
            Err(_) if start.elapsed() < deadline => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(format!("connect {host}: {err}")),
        }
    }
}
