//! Passive per-grant delivery diagnostics (clients#445).
//!
//! Every native grant that reaches a terminal outcome appends exactly one JSON
//! line to `delivery-diagnostics.jsonl`. The operator plays normally and sends
//! the file back the same way they send `client.log`; nothing here asks them to
//! run a probe, and nothing here changes what the delivery machine does.
//!
//! What it is for: clients#445 asks where a delta that the cave provably
//! executed actually landed when the held stack did not absorb it. Bloodborne
//! consumables overflow a capped pouch into storage, and clients#443 had to
//! stop parking those completions -- which means the client now *completes*
//! exactly the case #445 wants counted, and counts it nowhere. This file is the
//! count.
//!
//! It complements, and does not replace, the manual probe in
//! bb-archipelago#203. The manual probe still owns every controlled-condition
//! question -- a unique-item insert, a deliberately at-cap arming -- because
//! those conditions do not arise on their own during play. This one owns the
//! question the probe cannot answer: what the *distribution* looks like across
//! a real session.
//!
//! Three rules this module is built around:
//!
//! * **No new guest reads.** Every field is a value the delivery machine
//!   already computed for a decision it already makes ([`GrantTrace`]), or a
//!   piece of client state the loop already tracks. There is no probe here.
//! * **Never a new failure mode.** A write failure is swallowed after one
//!   warning. Diagnostics that can park a grant are worse than no diagnostics.
//! * **Inferred is inferred except where play validated the exact shape.**
//!   The client cannot read Bloodborne's storage box, so `storage_suspected`
//!   remains a hypothesis. `storage` is reserved for the insert deficit that a
//!   player subsequently confirmed in the storage box (clients#445).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::delivery::GrantTrace;

/// The file name appended beside the receive ledger.
pub const DIAGNOSTICS_FILE_NAME: &str = "delivery-diagnostics.jsonl";

/// `delivery-diagnostics.jsonl` in the ledger's own directory.
///
/// Deliberately derived rather than configured: the launcher already places
/// `ledger.json` and `client.log` in one per-session folder, so the ledger path
/// the client is *already* given names that folder exactly. A new CLI argument
/// would add a second way to say the same thing and a way to get it wrong, and
/// unlike `--log-file` there is no case for pointing this somewhere else.
pub fn diagnostics_path_for_ledger(ledger: &Path) -> PathBuf {
    match ledger.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(DIAGNOSTICS_FILE_NAME),
        _ => PathBuf::from(DIAGNOSTICS_FILE_NAME),
    }
}

/// The client-side context a record is stamped with. Both fields are values the
/// client loop already tracks; neither is read from the guest for this record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GrantContext {
    /// `LocationContext::gameplay_ready`, or `None` when the backend declines
    /// to report a context at all (live mode without an identity accessor).
    pub gameplay_ready: Option<bool>,
    /// Whether the live event-flag accessor was armed (clients#420).
    pub event_flags_armed: bool,
}

/// One terminal grant outcome, as one line of `delivery-diagnostics.jsonl`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeliveryRecord {
    /// ISO-8601 UTC, second resolution.
    pub utc: String,
    pub unix_seconds: u64,
    /// Millisecond timestamp used to correlate rapid release floods.
    pub unix_millis: u64,
    /// Monotonic terminal-grant sequence within this client process.
    pub session_sequence: u64,
    /// Wall-clock gap from the preceding terminal grant in this process.
    pub milliseconds_since_previous_terminal: Option<u64>,
    /// The preceding record's inference, copied here so one row can answer
    /// whether it immediately followed a suspected overflow.
    pub previous_inferred_destination: Option<String>,
    pub tag: String,
    /// Parsed out of the `ap_<index>` tag the client loop mints. `None` for a
    /// tag that does not carry one (a manual grant).
    pub ap_index: Option<u64>,
    pub item_id_raw: u32,
    pub item_id_normalized: u32,
    /// `"insert"` or `"delta"`; absent when no lane was ever chosen.
    pub lane: Option<String>,
    /// `"persistent"` or `"in_frame"`.
    pub source: Option<String>,
    pub quantity: u32,
    pub observed_before: Option<u32>,
    pub expected_after: Option<u32>,
    /// The held-stack totals the verify loop read back, in order. `null` is
    /// "geometry unavailable", never zero.
    pub readbacks: Vec<Option<u32>>,
    /// True read-back count; larger than `readbacks.len()` when truncated.
    pub readbacks_seen: u32,
    /// `last_readback - expected_after`: positive is the clients#443 surplus,
    /// negative the deficit that clients#445 is about.
    pub readback_surplus: Option<i64>,
    pub native_result: Option<u32>,
    pub execution_evidence: bool,
    pub verify_polls: u32,
    pub terminal_status: String,
    pub terminal_detail: String,
    pub gameplay_ready_at_submit: Option<bool>,
    pub gameplay_ready_at_terminal: Option<bool>,
    pub event_flags_armed_at_terminal: bool,
    /// **Inference, not observation.** See [`infer_destination`].
    pub inferred_destination: String,
}

/// Where the delta most plausibly went, from read-back arithmetic alone.
///
/// * `"held"` -- the held stack accounts for the delta: the last read-back is
///   at least `expected_after`. A surplus is the clients#443 concurrent pickup,
///   which is still the held stack absorbing the grant.
/// * `"storage"` -- an insert completed but never appeared in held inventory.
///   Oz's clients#445 capture confirmed that exact shape in the Hunter's Dream
///   storage box, so this one outcome is player-validated rather than inferred.
/// * `"storage_suspected"` -- a delta cave provably executed (clients#443's
///   evidence predicate) and the held stack still came in *under*
///   `expected_after`. A capped pouch overflowing into storage produces exactly
///   this shape. So does a concurrent spend in the same window; the client
///   cannot separate them, which is why the value says *suspected*.
/// * `"unknown"` -- anything else: a parked grant, a grant with no execution
///   evidence, or no usable read-back.
///
/// The client has no read of Bloodborne's storage box. This function is
/// arithmetic over numbers the machine already had, and its output must never
/// be reported as a measurement of where an item is.
pub fn infer_destination(is_success: bool, trace: &GrantTrace) -> &'static str {
    if !is_success {
        return "unknown";
    }
    let (Some(actual), Some(wanted)) = (trace.last_readback(), trace.expected_after) else {
        return "unknown";
    };
    if actual >= wanted {
        "held"
    } else if trace.lane == Some("insert") && trace.execution_evidence {
        "storage"
    } else if trace.execution_evidence {
        "storage_suspected"
    } else {
        "unknown"
    }
}

fn ap_index_from_tag(tag: &str) -> Option<u64> {
    tag.strip_prefix("ap_")?.parse().ok()
}

impl DeliveryRecord {
    /// Build a record from the trace and the terminal durable state. Every
    /// argument is already in hand at the call site; nothing is read here.
    pub fn build(
        trace: &GrantTrace,
        status: &str,
        detail: &str,
        is_success: bool,
        submit: GrantContext,
        terminal: GrantContext,
    ) -> Self {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let unix_seconds = elapsed.as_secs();
        let unix_millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        Self {
            utc: format_utc(unix_seconds),
            unix_seconds,
            unix_millis,
            session_sequence: 0,
            milliseconds_since_previous_terminal: None,
            previous_inferred_destination: None,
            tag: trace.tag.clone(),
            ap_index: ap_index_from_tag(&trace.tag),
            item_id_raw: trace.raw_id,
            item_id_normalized: trace.normalized_id,
            lane: trace.lane.map(str::to_string),
            source: trace.source.map(str::to_string),
            quantity: trace.quantity,
            observed_before: trace.observed_before,
            expected_after: trace.expected_after,
            readbacks: trace.readbacks.clone(),
            readbacks_seen: trace.readbacks_seen,
            readback_surplus: trace.readback_surplus(),
            native_result: trace.native_result,
            execution_evidence: trace.execution_evidence,
            verify_polls: trace.verify_polls,
            terminal_status: status.to_string(),
            terminal_detail: detail.to_string(),
            gameplay_ready_at_submit: submit.gameplay_ready,
            gameplay_ready_at_terminal: terminal.gameplay_ready,
            event_flags_armed_at_terminal: terminal.event_flags_armed,
            inferred_destination: infer_destination(is_success, trace).to_string(),
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` from a Unix timestamp, without pulling in a date
/// crate for one field. Civil-from-days is Howard Hinnant's algorithm.
fn format_utc(unix_seconds: u64) -> String {
    let days = (unix_seconds / 86_400) as i64;
    let seconds_of_day = unix_seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

/// Where a record goes. Injectable so the failure path is testable.
pub trait DiagnosticWriter {
    fn write_line(&mut self, line: &str) -> std::io::Result<()>;
}

/// Append one line per grant to a file, opened and flushed per write.
///
/// Open-per-line is deliberate: at one line per delivered item the cost is
/// nothing, and it means the file survives being rotated, copied or deleted
/// mid-session (the operator will be sending it while the client runs) without
/// the client holding a stale handle.
pub struct JsonlFile {
    path: PathBuf,
}

impl JsonlFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl DiagnosticWriter for JsonlFile {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.flush()
    }
}

/// The sink the delivery engine owns. Disabled by default; a failure to write
/// warns once and is then silent forever.
///
/// `record` returns nothing on purpose. There is no error for a caller to
/// propagate, because there is no failure here a delivery should ever notice.
pub struct DiagnosticSink {
    writer: Option<Box<dyn DiagnosticWriter + Send>>,
    warned: bool,
    sequence: u64,
    previous_unix_millis: Option<u64>,
    previous_inferred_destination: Option<String>,
}

impl Default for DiagnosticSink {
    fn default() -> Self {
        Self::disabled()
    }
}

impl DiagnosticSink {
    pub fn disabled() -> Self {
        Self {
            writer: None,
            warned: false,
            sequence: 0,
            previous_unix_millis: None,
            previous_inferred_destination: None,
        }
    }

    pub fn new(writer: Box<dyn DiagnosticWriter + Send>) -> Self {
        Self {
            writer: Some(writer),
            warned: false,
            sequence: 0,
            previous_unix_millis: None,
            previous_inferred_destination: None,
        }
    }

    pub fn is_armed(&self) -> bool {
        self.writer.is_some()
    }

    /// Append one record. Any failure -- serialisation or I/O -- is reported
    /// once through `warn` and then swallowed for the rest of the session.
    pub fn record(&mut self, record: &DeliveryRecord, warn: &mut dyn FnMut(&str)) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        let mut record = record.clone();
        self.sequence += 1;
        record.session_sequence = self.sequence;
        record.milliseconds_since_previous_terminal = self
            .previous_unix_millis
            .map(|previous| record.unix_millis.saturating_sub(previous));
        record.previous_inferred_destination = self.previous_inferred_destination.clone();
        self.previous_unix_millis = Some(record.unix_millis);
        self.previous_inferred_destination = Some(record.inferred_destination.clone());
        let outcome = json::to_string(&record)
            .map_err(|error| error.to_string())
            .and_then(|line| writer.write_line(&line).map_err(|error| error.to_string()));
        if let Err(error) = outcome
            && !self.warned
        {
            self.warned = true;
            warn(&format!(
                "Delivery diagnostics could not be written ({error}); deliveries are unaffected and this warning will not repeat."
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_ledger_directory_is_where_the_jsonl_lands() {
        let path = diagnostics_path_for_ledger(Path::new("sessions/abc/ledger.json"));
        assert_eq!(
            path,
            Path::new("sessions/abc").join(DIAGNOSTICS_FILE_NAME),
            "the diagnostics file rides beside ledger.json"
        );
    }

    #[test]
    fn a_bare_ledger_name_still_yields_a_usable_path() {
        assert_eq!(
            diagnostics_path_for_ledger(Path::new("ledger.json")),
            PathBuf::from(DIAGNOSTICS_FILE_NAME)
        );
    }

    #[test]
    fn the_ap_index_comes_out_of_the_tag_the_loop_already_mints() {
        assert_eq!(ap_index_from_tag("ap_17"), Some(17));
        assert_eq!(ap_index_from_tag("ap_17_equip"), None);
        assert_eq!(ap_index_from_tag("manual"), None);
    }

    #[test]
    fn the_epoch_and_a_known_instant_format_as_iso_8601() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        // 2026-08-26T00:00:00Z
        assert_eq!(format_utc(1_787_702_400), "2026-08-26T00:00:00Z");
    }

    fn trace_with(
        readbacks: &[Option<u32>],
        expected_after: Option<u32>,
        evidence: bool,
    ) -> GrantTrace {
        let mut trace = GrantTrace {
            expected_after,
            execution_evidence: evidence,
            ..GrantTrace::default()
        };
        for value in readbacks {
            trace.readbacks.push(*value);
            trace.readbacks_seen += 1;
        }
        trace
    }

    #[test]
    fn a_stack_that_absorbed_the_delta_is_inferred_held() {
        let trace = trace_with(&[Some(7)], Some(7), true);
        assert_eq!(infer_destination(true, &trace), "held");
    }

    #[test]
    fn a_concurrent_pickup_surplus_is_still_inferred_held() {
        let trace = trace_with(&[Some(8)], Some(7), true);
        assert_eq!(infer_destination(true, &trace), "held");
        assert_eq!(trace.readback_surplus(), Some(1));
    }

    #[test]
    fn an_executed_deficit_is_inferred_storage_suspected() {
        let mut trace = trace_with(&[Some(5)], Some(7), true);
        trace.lane = Some("delta");
        assert_eq!(infer_destination(true, &trace), "storage_suspected");
        assert_eq!(trace.readback_surplus(), Some(-2));
    }

    #[test]
    fn a_player_confirmed_insert_deficit_is_named_storage() {
        let mut trace = trace_with(&[Some(0)], Some(1), true);
        trace.lane = Some("insert");
        trace.native_result = Some(2);
        assert_eq!(infer_destination(true, &trace), "storage");
    }

    #[test]
    fn a_deficit_without_execution_evidence_infers_nothing() {
        let trace = trace_with(&[Some(5)], Some(7), false);
        assert_eq!(infer_destination(true, &trace), "unknown");
    }

    #[test]
    fn a_parked_grant_infers_nothing_however_the_arithmetic_reads() {
        let trace = trace_with(&[Some(7)], Some(7), true);
        assert_eq!(infer_destination(false, &trace), "unknown");
    }

    #[test]
    fn a_record_round_trips_through_the_jsonl_encoding() {
        let trace = trace_with(&[None, Some(5)], Some(7), true);
        let record = DeliveryRecord::build(
            &trace,
            "completed",
            "tag=ap_3 completed with concurrent spend or storage overflow",
            true,
            GrantContext {
                gameplay_ready: Some(true),
                event_flags_armed: true,
            },
            GrantContext {
                gameplay_ready: Some(false),
                event_flags_armed: true,
            },
        );
        let line = json::to_string(&record).expect("serialise");
        assert!(!line.contains('\n'), "one record is exactly one line");
        let decoded: DeliveryRecord = json::from_str(&line).expect("deserialise");
        assert_eq!(decoded, record);
        assert_eq!(decoded.inferred_destination, "storage_suspected");
        assert_eq!(decoded.readbacks, vec![None, Some(5)]);
        assert_eq!(decoded.gameplay_ready_at_submit, Some(true));
        assert_eq!(decoded.gameplay_ready_at_terminal, Some(false));
    }

    #[derive(Default)]
    struct FailingWriter {
        attempts: u32,
    }

    struct CapturingWriter(Arc<Mutex<Vec<String>>>);

    impl DiagnosticWriter for CapturingWriter {
        fn write_line(&mut self, line: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(line.to_string());
            Ok(())
        }
    }

    #[test]
    fn the_sink_stamps_sequence_gap_and_previous_destination() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let mut sink = DiagnosticSink::new(Box::new(CapturingWriter(lines.clone())));
        let first = DeliveryRecord {
            unix_millis: 1_000,
            inferred_destination: "storage_suspected".into(),
            ..DeliveryRecord::default()
        };
        let second = DeliveryRecord {
            unix_millis: 1_275,
            inferred_destination: "held".into(),
            ..DeliveryRecord::default()
        };
        sink.record(&first, &mut |_| {});
        sink.record(&second, &mut |_| {});
        let captured = lines.lock().unwrap();
        let first: DeliveryRecord = json::from_str(&captured[0]).unwrap();
        let second: DeliveryRecord = json::from_str(&captured[1]).unwrap();
        assert_eq!(first.session_sequence, 1);
        assert_eq!(first.milliseconds_since_previous_terminal, None);
        assert_eq!(second.session_sequence, 2);
        assert_eq!(second.milliseconds_since_previous_terminal, Some(275));
        assert_eq!(
            second.previous_inferred_destination.as_deref(),
            Some("storage_suspected")
        );
    }

    impl DiagnosticWriter for FailingWriter {
        fn write_line(&mut self, _line: &str) -> std::io::Result<()> {
            self.attempts += 1;
            Err(std::io::Error::other("the disk said no"))
        }
    }

    #[test]
    fn a_failing_writer_warns_exactly_once_and_never_returns_an_error() {
        let mut sink = DiagnosticSink::new(Box::new(FailingWriter::default()));
        let mut warnings = Vec::new();
        let record = DeliveryRecord::default();
        for _ in 0..5 {
            sink.record(&record, &mut |line| warnings.push(line.to_string()));
        }
        assert_eq!(warnings.len(), 1, "one warning for the whole session");
        assert!(
            warnings[0].contains("deliveries are unaffected"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_disabled_sink_is_a_silent_no_op() {
        let mut sink = DiagnosticSink::disabled();
        assert!(!sink.is_armed());
        let mut warnings = Vec::new();
        sink.record(&DeliveryRecord::default(), &mut |line| {
            warnings.push(line.to_string())
        });
        assert!(warnings.is_empty());
    }
}
