//! Stub HTTP. Nothing here dials a host other than the listener it binds.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use telepatch::{SecretBackend, SecretError, SecretRef, SecretValue};
use telepatch_openbao::backend;

static ENV: Mutex<()> = Mutex::new(());

struct EnvGuard<'a> {
    _guard: MutexGuard<'a, ()>,
}

impl Drop for EnvGuard<'_> {
    fn drop(&mut self) {
        clear_env();
    }
}

fn env_lock() -> EnvGuard<'static> {
    let guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
    clear_env();
    EnvGuard { _guard: guard }
}

fn clear_env() {
    for name in [
        "BAO_ADDR",
        "BAO_TOKEN",
        "BAO_SKIP_VERIFY",
        "VAULT_ADDR",
        "VAULT_TOKEN",
        "VAULT_SKIP_VERIFY",
    ] {
        unsafe { std::env::remove_var(name) }
    }
}

fn set_env(name: &str, value: &str) {
    unsafe { std::env::set_var(name, value) }
}

struct Hit {
    path: String,
    token: TokenState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokenState {
    Missing,
    Ok,
    Bad,
}

struct Stub {
    addr: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

fn serve(token: &'static str, routes: fn(&str) -> (u16, String)) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&hits);
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut sock) = incoming else { break };
            let _ = sock.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                match sock.read(&mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            let token_state = if !head.to_ascii_lowercase().contains("x-vault-token:") {
                TokenState::Missing
            } else if header_is(&head, "x-vault-token", token) {
                TokenState::Ok
            } else {
                TokenState::Bad
            };
            recorded.lock().unwrap().push(Hit {
                path: path.clone(),
                token: token_state,
            });
            let (code, body) = routes(&path);
            let reason = match code {
                200 => "OK",
                403 => "Forbidden",
                404 => "Not Found",
                _ => "Error",
            };
            let resp = format!(
                "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
        }
    });
    Stub {
        addr: format!("http://127.0.0.1:{port}"),
        hits,
    }
}

fn header_is(head: &str, name: &str, want: &str) -> bool {
    head.lines().any(|line| {
        let Some((k, v)) = line.split_once(':') else {
            return false;
        };
        k.eq_ignore_ascii_case(name) && v.trim() == want
    })
}

fn sref(path: &str, field: &str) -> SecretRef {
    SecretRef {
        scheme: "bao".into(),
        path: path.into(),
        field: field.into(),
        var: "telegraf.influx_token".into(),
        role: "telegraf".into(),
    }
}

fn is_value(got: &Result<SecretValue, SecretError>, want: &str) -> bool {
    matches!(got, Ok(v) if v == &SecretValue::new(want))
}

fn err_text(got: &Result<SecretValue, SecretError>) -> String {
    match got {
        Err(e) => e.to_string(),
        Ok(_) => "ok".into(),
    }
}

const TOKEN: &str = "t90-stub-token";

fn mounts_v2(path: &str) -> (u16, String) {
    if path.contains("/sys/internal/ui/mounts/") {
        return (
            200,
            r#"{"data":{"path":"kv/","options":{"version":"2"}}}"#.into(),
        );
    }
    if path == "/v1/auth/token/lookup-self" {
        return (200, r#"{"data":{"expire_time":null}}"#.into());
    }
    (404, "{}".into())
}

#[test]
fn v2_reads_each_path_once_and_returns_the_field() {
    let _env = env_lock();
    let stub = serve(TOKEN, |path| {
        if path == "/v1/kv/data/app" {
            return (
                200,
                r#"{"data":{"data":{"token":"v2-value","other":"should-not-leak"},"metadata":{}}}"#
                    .into(),
            );
        }
        mounts_v2(path)
    });
    set_env("BAO_ADDR", &stub.addr);
    set_env("BAO_TOKEN", TOKEN);
    let got = backend().resolve(&[sref("kv/app", "token"), sref("kv/app", "token")]);
    assert!(is_value(&got[0], "v2-value"), "field");
    assert!(is_value(&got[1], "v2-value"), "second field");
    let hits = stub.hits.lock().unwrap();
    assert!(hits.iter().all(|h| h.token == TokenState::Ok));
    let data: Vec<_> = hits.iter().filter(|h| h.path.contains("/data/")).collect();
    assert_eq!(data.len(), 1, "one read per distinct path");
    assert_eq!(
        hits.iter()
            .filter(|h| h.path.contains("/sys/internal/ui/mounts/"))
            .count(),
        1
    );
}

#[test]
fn v1_reads_data_not_the_v2_path() {
    let _env = env_lock();
    let stub = serve(TOKEN, |path| {
        if path.contains("/sys/internal/ui/mounts/") {
            return (
                200,
                r#"{"data":{"path":"secret/","options":{"version":"1"}}}"#.into(),
            );
        }
        if path == "/v1/auth/token/lookup-self" {
            return (200, r#"{"data":{"expire_time":null}}"#.into());
        }
        if path == "/v1/secret/app" {
            return (200, r#"{"data":{"token":"v1-value"}}"#.into());
        }
        (404, "{}".into())
    });
    set_env("VAULT_ADDR", &stub.addr);
    set_env("VAULT_TOKEN", TOKEN);
    let got = backend().resolve(&[sref("secret/app", "token")]);
    assert!(is_value(&got[0], "v1-value"), "v1 field");
    let hits = stub.hits.lock().unwrap();
    assert!(hits.iter().any(|h| h.path == "/v1/secret/app"));
    assert!(!hits.iter().any(|h| h.path.contains("/data/")));
}

#[test]
fn missing_path_names_the_path() {
    let _env = env_lock();
    let stub = serve(TOKEN, |path| {
        if path == "/v1/auth/token/lookup-self" {
            return (200, r#"{"data":{"expire_time":null}}"#.into());
        }
        if path.contains("/sys/internal/ui/mounts/") {
            return (
                200,
                r#"{"data":{"path":"kv/","options":{"version":"2"}}}"#.into(),
            );
        }
        (404, r#"{"errors":["no such secret"]}"#.into())
    });
    set_env("BAO_ADDR", &stub.addr);
    set_env("BAO_TOKEN", TOKEN);
    let msg = err_text(&backend().resolve(&[sref("kv/missing", "token")])[0]);
    assert_eq!(msg, "no secret at kv/missing");
}

#[test]
fn missing_field_lists_names_only() {
    let _env = env_lock();
    let stub = serve(TOKEN, |path| {
        if path == "/v1/kv/data/app" {
            return (
                200,
                r#"{"data":{"data":{"api_key":"should-not-leak","zeta":"also-hidden"}}}"#.into(),
            );
        }
        mounts_v2(path)
    });
    set_env("BAO_ADDR", &stub.addr);
    set_env("BAO_TOKEN", TOKEN);
    let msg = err_text(&backend().resolve(&[sref("kv/app", "token")])[0]);
    assert_eq!(msg, "kv/app has no field token; it has: api_key, zeta");
    assert!(!msg.contains("should-not-leak") && !msg.contains("also-hidden"));
}

#[test]
fn forbidden_is_permission_denied() {
    let _env = env_lock();
    let stub = serve(TOKEN, |path| {
        if path == "/v1/auth/token/lookup-self" {
            return (200, r#"{"data":{"expire_time":null}}"#.into());
        }
        if path.contains("/sys/internal/ui/mounts/") {
            return (
                200,
                r#"{"data":{"path":"kv/","options":{"version":"2"}}}"#.into(),
            );
        }
        (403, r#"{"errors":["permission denied"]}"#.into())
    });
    set_env("BAO_ADDR", &stub.addr);
    set_env("BAO_TOKEN", TOKEN);
    let msg = err_text(&backend().resolve(&[sref("kv/app", "token")])[0]);
    assert_eq!(msg, "permission denied reading kv/app");
    assert!(!msg.contains("permission denied\"]"));
}

#[test]
fn no_token_names_the_vars_and_does_not_connect() {
    let _env = env_lock();
    let stub = serve(TOKEN, |_path| (500, "{}".into()));
    set_env("BAO_ADDR", &stub.addr);
    let msg = err_text(&backend().resolve(&[sref("kv/app", "token")])[0]);
    assert_eq!(msg, "openbao: no token: set BAO_TOKEN or VAULT_TOKEN");
    thread::sleep(Duration::from_millis(50));
    assert!(
        stub.hits.lock().unwrap().is_empty(),
        "connected without a token"
    );
}

#[test]
fn skip_verify_warns_and_names_which_var() {
    let _env = env_lock();
    set_env("BAO_SKIP_VERIFY", "true");
    let lines = backend().warnings();
    assert_eq!(
        lines,
        vec!["openbao: certificate verification is OFF (BAO_SKIP_VERIFY)".to_string()]
    );
    unsafe { std::env::remove_var("BAO_SKIP_VERIFY") };
    set_env("VAULT_SKIP_VERIFY", "1");
    let lines = backend().warnings();
    assert_eq!(
        lines,
        vec!["openbao: certificate verification is OFF (VAULT_SKIP_VERIFY)".to_string()]
    );
}

#[test]
fn expired_token_names_the_day_and_does_not_read_the_secret() {
    let _env = env_lock();
    let stub = serve(TOKEN, |path| {
        if path == "/v1/auth/token/lookup-self" {
            return (
                200,
                r#"{"data":{"expire_time":"2020-01-02T03:04:05Z"}}"#.into(),
            );
        }
        (500, "{}".into())
    });
    set_env("BAO_ADDR", &stub.addr);
    set_env("BAO_TOKEN", TOKEN);
    let msg = err_text(&backend().resolve(&[sref("kv/app", "token")])[0]);
    assert_eq!(msg, "openbao: token expired on 2020-01-02: log in again");
    let hits = stub.hits.lock().unwrap();
    assert!(hits.iter().all(|h| h.path.contains("lookup-self")));
}
