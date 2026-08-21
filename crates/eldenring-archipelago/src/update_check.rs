//! The update banner's I/O half: fetch `/er/latest.json` once per session, in the background,
//! fail-SILENT. The pure verdict (parse, compare, wording) is `er_logic::update_check`; read its
//! module doc for what the banner says and why the CONTRACT comparison is the whole point.
//!
//! FAIL-SILENT IS A REQUIREMENT, NOT A COURTESY: a dead site, a proxy, DNS, a truncated body --
//! none of it may affect play or emit more than one debug-level line. The fetch runs on its own
//! thread with socket timeouts; the game thread only ever polls a lock-free mailbox.
//!
//! Gated like a probe (`shared::probes` plumbing, default ON): `"probes": {"update_check": false}`
//! in apconfig.json or `ER_UPDATE_CHECK=0` turns it off. Filed under probes deliberately -- it is
//! a diagnostic fetch, the plumbing already has the env-wins rule, and a second config surface
//! for one boolean is how config files rot.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const HOST: &str = "peliarch.ca";
const PATH: &str = "/er/latest.json";

static SPAWNED: AtomicBool = AtomicBool::new(false);
/// The verdict toast, ready to push. `set` at most once by the worker; `take`n by the tick.
static RESULT: OnceLock<String> = OnceLock::new();
static DELIVERED: AtomicBool = AtomicBool::new(false);

/// Kick the background check off. Idempotent; call it from anywhere post-connect.
pub fn spawn() {
    if SPAWNED.swap(true, Ordering::Relaxed) {
        return;
    }
    if !shared::probes::enabled_by_default("ER_UPDATE_CHECK", "update_check") {
        log::info!("update-check: OFF (probes.update_check=false / ER_UPDATE_CHECK=0)");
        return;
    }
    std::thread::Builder::new()
        .name("er-update-check".into())
        .spawn(|| match fetch() {
            Some(body) => {
                let ours_ver = crate::contract_gen::APWORLD_VERSION_EXPECTED;
                let ours_contract = crate::contract_gen::CONTRACT_HASH;
                match er_logic::update_check::parse_latest(&body) {
                    Some(latest) => {
                        log::info!(
                            "update-check: stable is v{} contract/{} (this build: v{} \
                                 contract/{})",
                            latest.version,
                            latest.contract,
                            ours_ver,
                            &ours_contract[..8.min(ours_contract.len())]
                        );
                        if let Some(t) =
                            er_logic::update_check::toast(ours_ver, ours_contract, &latest)
                        {
                            let _ = RESULT.set(t);
                        }
                    }
                    None => log::debug!("update-check: body did not parse; no news"),
                }
            }
            None => log::debug!("update-check: fetch failed; no news"),
        })
        .ok();
}

/// The tick's mailbox read: the toast, exactly once.
pub fn take_toast() -> Option<String> {
    if DELIVERED.load(Ordering::Relaxed) {
        return None;
    }
    let t = RESULT.get()?;
    DELIVERED.store(true, Ordering::Relaxed);
    Some(t.clone())
}

/// One HTTPS GET with rustls + webpki roots. `None` on ANY failure -- callers treat that as
/// "no news today", never as an error.
fn fetch() -> Option<String> {
    let root_store =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(HOST).ok()?;
    let mut conn = rustls::ClientConnection::new(std::sync::Arc::new(config), server_name).ok()?;
    let mut sock = TcpStream::connect((HOST, 443)).ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(8))).ok()?;
    sock.set_write_timeout(Some(Duration::from_secs(8))).ok()?;
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    let req = format!(
        "GET {PATH} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: er-archipelago-update-check\r\n\
         Accept: application/json\r\nConnection: close\r\n\r\n"
    );
    tls.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::with_capacity(4096);
    // Bounded read: latest.json is ~130 bytes; anything past 64 KiB is not our file.
    let mut buf = [0u8; 4096];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > 65536 {
                    return None;
                }
            }
            Err(_) => break, // close_notify quirks and timeouts both end the read; headers decide
        }
    }
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.lines().next()?;
    if !status.contains(" 200") {
        return None;
    }
    // Tolerate chunked encoding by stripping chunk-size lines if present: the body is one small
    // JSON object, so the crude filter (keep the braces line) is enough for this endpoint.
    let body = body.trim();
    let json = {
        let i = body.find('{')?;
        let j = body.rfind('}')?;
        &body[i..=j]
    };
    Some(json.to_string())
}
