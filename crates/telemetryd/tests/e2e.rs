//! End-to-end: run the real daemon binary, feed it UDP packets (valid and
//! malformed), stop it with SIGTERM, and check the NDJSON output.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{encode_frame, encode_packet, Catalog, SignalValue};
use rules::LogRecord;

fn repo(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

struct Daemon {
    child: Child,
    stderr: mpsc::Receiver<String>,
    log: Vec<String>,
}

impl Daemon {
    fn start(config: &Path) -> Daemon {
        let mut child = Command::new(env!("CARGO_BIN_EXE_telemetryd"))
            .arg("--config")
            .arg(config)
            .env("RUST_LOG", "info")
            .env("NO_COLOR", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Daemon {
            child,
            stderr: rx,
            log: Vec::new(),
        }
    }

    /// Collects stderr until a line contains `needle`.
    fn wait_for(&mut self, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.stderr.recv_timeout(left) {
                Ok(line) => {
                    let found = line.contains(needle);
                    self.log.push(line);
                    if found {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        panic!("'{needle}' not seen; stderr:\n{}", self.log.join("\n"));
    }

    fn terminate(&mut self) -> std::process::ExitStatus {
        let pid = self.child.id().to_string();
        assert!(Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.log.extend(self.stderr.try_iter());
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not exit after SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn write_config(dir: &Path, port: u16, rules_file: &Path) -> PathBuf {
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"
vin = "E2E0000001"
model = "R1S"
catalog_path = "{catalog}"
rules_file = "{rules}"
[ingest]
udp_bind = "127.0.0.1:{port}"
[output]
log_dir = "{out}"
"#,
            catalog = repo("catalogs/r1s.json").display(),
            rules = rules_file.display(),
            out = dir.join("out").display(),
        ),
    )
    .unwrap();
    config
}

fn read_records(dir: &Path) -> Vec<LogRecord> {
    std::fs::read_to_string(dir.join("out/telemetry.ndjson"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad line {l}: {e}")))
        .collect()
}

#[test]
fn udp_to_ndjson_with_garbage_and_graceful_shutdown() {
    let tmp = tempfile::tempdir().unwrap();
    let port = free_udp_port();
    let config = write_config(tmp.path(), port, &repo("rulesets/default.json"));
    let mut daemon = Daemon::start(&config);
    daemon.wait_for("telemetryd started", Duration::from_secs(20));

    let catalog =
        Catalog::from_json(&std::fs::read_to_string(repo("catalogs/r1s.json")).unwrap()).unwrap();
    let sig = |name: &str| catalog.by_name(name).unwrap();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = format!("127.0.0.1:{port}");

    // ~1.6 s at 50 Hz: parked for 0.2 s, then driving hot; SOC steps down once.
    for i in 0..80u32 {
        let ts = now_us();
        let gear = if i < 10 { "P" } else { "D" };
        let soc = if i < 40 { 50.0 } else { 48.0 };
        let frames = vec![
            encode_frame(sig("gear"), ts, &SignalValue::Enum(gear.into())).unwrap(),
            encode_frame(sig("battery_soc"), ts, &SignalValue::Num(soc)).unwrap(),
            encode_frame(sig("motor_temp"), ts, &SignalValue::Num(95.0)).unwrap(),
            encode_frame(sig("vehicle_speed"), ts, &SignalValue::Num(f64::from(i))).unwrap(),
        ];
        let packet = encode_packet(&frames).unwrap();
        socket.send_to(&packet, &target).unwrap();
        // Garbage interleaved with valid traffic must not disturb anything.
        if i % 5 == 0 {
            socket.send_to(b"\xde\xad\x01\x01garbage", &target).unwrap();
            socket
                .send_to(&packet[..packet.len() - 3], &target)
                .unwrap();
            socket.send_to(&vec![0x54; 1600], &target).unwrap();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(100));

    let status = daemon.terminate();
    assert!(status.success(), "exit status {status}");
    let log = daemon.log.join("\n");
    assert!(log.contains("signal=\"SIGTERM\""), "{log}");
    assert!(log.contains("bad_magic") && log.contains("truncated") && log.contains("oversize"));
    assert!(log.contains("telemetryd stopped"));

    let records = read_records(tmp.path());
    let mut by_rule: BTreeMap<&str, Vec<&LogRecord>> = BTreeMap::new();
    for r in &records {
        assert_eq!((r.vin.as_str(), r.model.as_str()), ("E2E0000001", "R1S"));
        assert_eq!(r.ruleset_version, 1);
        by_rule.entry(r.rule_id.as_str()).or_default().push(r);
    }

    let gears: Vec<_> = by_rule["gear_change"]
        .iter()
        .map(|r| r.value.clone())
        .collect();
    assert_eq!(
        gears,
        [SignalValue::Enum("P".into()), SignalValue::Enum("D".into())]
    );
    let socs: Vec<_> = by_rule["soc_change"]
        .iter()
        .map(|r| r.value.clone())
        .collect();
    assert_eq!(socs, [SignalValue::Num(50.0), SignalValue::Num(48.0)]);
    assert!(!by_rule["speed_1hz"].is_empty(), "{by_rule:?}");
    assert_eq!(by_rule["speed_1hz"][0].unit.as_deref(), Some("km/h"));
    // hot_motor: every 200 ms while in D and > 90 C, over ~1.4 s of driving.
    let hot = by_rule["hot_motor"].len();
    assert!((4..=9).contains(&hot), "hot_motor fired {hot} times");
    // low_soc_drive needs SOC < 20: never true here.
    assert!(!by_rule.contains_key("low_soc_drive"));
}

#[test]
fn invalid_rules_file_fails_fast() {
    let tmp = tempfile::tempdir().unwrap();
    let rules = tmp.path().join("rules.json");
    std::fs::write(
        &rules,
        r#"{"ruleset_version":1,"model":"*","rules":[{"id":"x","signal":"nope","mode":"on_change"}]}"#,
    )
    .unwrap();
    let config = write_config(tmp.path(), free_udp_port(), &rules);
    let out = Command::new(env!("CARGO_BIN_EXE_telemetryd"))
        .arg("--config")
        .arg(&config)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown signal 'nope'"), "{stderr}");
}
