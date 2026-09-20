mod http;
mod identity;
mod processes;
mod syscalls;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub use http::{HttpObserver, HttpRequest, HttpRequests};
pub use syscalls::SyscallTracer;

pub const SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_BUFFER_BYTES: usize = 64 * 1024 * 1024;

const MAX_REMEMBERED_NAMES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceOptions {
    pub processes: bool,
    pub files: bool,
    pub network: bool,
    pub mounts: bool,
    pub buffer_bytes: usize,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            processes: true,
            files: true,
            network: true,
            mounts: true,
            buffer_bytes: DEFAULT_BUFFER_BYTES,
        }
    }
}

#[derive(Clone)]
pub struct Tracer {
    options: TraceOptions,
    recorder: Arc<Mutex<Recorder>>,
}

struct Recorder {
    next_seq: u64,
    guest_ns: u64,
    lines: VecDeque<Vec<u8>>,
    buffered_bytes: usize,
    dropped: u64,
    names_by_address: HashMap<[u8; 4], String>,
}

impl Tracer {
    pub fn new(options: TraceOptions) -> Self {
        Self {
            options,
            recorder: Arc::new(Mutex::new(Recorder {
                next_seq: 0,
                guest_ns: 0,
                lines: VecDeque::new(),
                buffered_bytes: 0,
                dropped: 0,
                names_by_address: HashMap::new(),
            })),
        }
    }

    pub fn options(&self) -> TraceOptions {
        self.options
    }

    pub fn traces_processes(&self) -> bool {
        self.options.processes
    }

    pub fn traces_files(&self) -> bool {
        self.options.files
    }

    pub fn traces_network(&self) -> bool {
        self.options.network
    }

    pub fn traces_mounts(&self) -> bool {
        self.options.mounts
    }

    fn recorder(&self) -> MutexGuard<'_, Recorder> {
        self.recorder
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_guest_ns(&self, guest_ns: u64) {
        self.recorder().guest_ns = guest_ns;
    }

    pub fn guest_ns(&self) -> u64 {
        self.recorder().guest_ns
    }

    pub fn record(&self, kind: &str, fields: &[(&str, Value)]) {
        self.record_seq(kind, fields);
    }

    pub fn record_seq(&self, kind: &str, fields: &[(&str, Value)]) -> Option<u64> {
        let wall_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or(0);

        let mut recorder = self.recorder();
        let capacity = self.options.buffer_bytes;

        if recorder.dropped > 0 {
            let count = recorder.dropped;
            let marker = recorder.line("trace.dropped", wall_ms, &[("count", count.into())]);

            if recorder.buffered_bytes + marker.len() > capacity {
                recorder.dropped += 1;
                return None;
            }
            recorder.dropped = 0;
            recorder.push(marker);
        }

        let seq = recorder.next_seq;
        let line = recorder.line(kind, wall_ms, fields);
        if recorder.buffered_bytes + line.len() > capacity {
            recorder.dropped += 1;
            return None;
        }
        recorder.push(line);

        Some(seq)
    }

    pub fn drain(&self, max_bytes: usize) -> Vec<u8> {
        let mut recorder = self.recorder();
        let mut drained = Vec::new();

        while let Some(line) = recorder.lines.front() {
            if !drained.is_empty() && drained.len() + line.len() > max_bytes {
                break;
            }
            let line = recorder.lines.pop_front().unwrap();
            recorder.buffered_bytes -= line.len();
            drained.extend_from_slice(&line);
        }

        drained
    }

    pub fn drain_text(&self, max_bytes: usize) -> String {
        let drained = self.drain(max_bytes);
        String::from_utf8(drained)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
    }

    pub fn remember_name(&self, address: [u8; 4], name: &str) {
        let mut recorder = self.recorder();
        if recorder.names_by_address.len() >= MAX_REMEMBERED_NAMES
            && !recorder.names_by_address.contains_key(&address)
        {
            recorder.names_by_address.clear();
        }
        recorder.names_by_address.insert(address, name.to_string());
    }

    pub fn name_of(&self, address: [u8; 4]) -> Option<String> {
        self.recorder().names_by_address.get(&address).cloned()
    }
}

impl Recorder {
    fn line(&mut self, kind: &str, wall_ms: u64, fields: &[(&str, Value)]) -> Vec<u8> {
        let seq = self.next_seq;
        self.next_seq += 1;

        let mut line = format!(
            "{{\"v\":{SCHEMA_VERSION},\"seq\":{seq},\"guest_ns\":{},\"wall_ms\":{wall_ms},\"kind\":{}",
            self.guest_ns,
            Value::from(kind),
        );
        for (name, value) in fields {
            line.push(',');
            line.push_str(&Value::from(*name).to_string());
            line.push(':');
            line.push_str(&value.to_string());
        }
        line.push_str("}\n");

        line.into_bytes()
    }

    fn push(&mut self, line: Vec<u8>) {
        self.buffered_bytes += line.len();
        self.lines.push_back(line);
    }
}

pub fn format_address(address: [u8; 4]) -> String {
    std::net::Ipv4Addr::from(address).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracer_with_buffer(buffer_bytes: usize) -> Tracer {
        Tracer::new(TraceOptions {
            buffer_bytes,
            ..TraceOptions::default()
        })
    }

    fn drained_lines(tracer: &Tracer) -> Vec<Value> {
        String::from_utf8(tracer.drain(usize::MAX))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn a_line_carries_the_common_fields_then_its_own_in_order() {
        let tracer = tracer_with_buffer(4096);
        tracer.set_guest_ns(1_500);
        tracer.record(
            "net.dns",
            &[("name", "pypi.org".into()), ("type", "A".into())],
        );

        let text = String::from_utf8(tracer.drain(usize::MAX)).unwrap();
        assert!(
            text.starts_with("{\"v\":1,\"seq\":0,\"guest_ns\":1500,\"wall_ms\":"),
            "{text}"
        );
        assert!(
            text.ends_with(",\"kind\":\"net.dns\",\"name\":\"pypi.org\",\"type\":\"A\"}\n"),
            "{text}"
        );
    }

    #[test]
    fn drain_returns_whole_lines_and_keeps_the_rest() {
        let tracer = tracer_with_buffer(4096);
        for index in 0..3 {
            tracer.record("mount.open", &[("index", index.into())]);
        }

        let first_line_len = tracer.recorder().lines[0].len();
        let first = tracer.drain(first_line_len + 1);
        assert_eq!(first.iter().filter(|&&byte| byte == b'\n').count(), 1);

        let rest = drained_lines(&tracer);
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0]["seq"], 1);
        assert!(tracer.drain(usize::MAX).is_empty());
    }

    #[test]
    fn a_line_longer_than_the_limit_is_still_drained() {
        let tracer = tracer_with_buffer(4096);
        tracer.record("net.http", &[("url", "x".repeat(200).into())]);
        assert!(!tracer.drain(10).is_empty());
    }

    #[test]
    fn a_full_buffer_counts_drops_and_says_so_once_there_is_room() {
        let tracer = tracer_with_buffer(300);
        for _ in 0..10 {
            tracer.record("mount.open", &[("path", "/data/file".into())]);
        }
        let kept = drained_lines(&tracer);
        assert!(kept.len() < 10);

        tracer.record("mount.open", &[("path", "/data/after".into())]);
        let after = drained_lines(&tracer);

        assert_eq!(after[0]["kind"], "trace.dropped");
        assert_eq!(after[0]["count"], 10 - kept.len() as u64);
        assert_eq!(after[1]["path"], "/data/after");
        assert_eq!(
            after[1]["seq"].as_u64().unwrap(),
            after[0]["seq"].as_u64().unwrap() + 1
        );
    }

    #[test]
    fn names_are_remembered_by_address() {
        let tracer = tracer_with_buffer(4096);
        tracer.remember_name([151, 101, 0, 223], "pypi.org");
        assert_eq!(
            tracer.name_of([151, 101, 0, 223]).as_deref(),
            Some("pypi.org")
        );
        assert_eq!(tracer.name_of([1, 1, 1, 1]), None);
    }
}
