use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use portzero_daemon::discovery::{scan_network_process_services, DiscoveredNetworkService};
use portzero_daemon::management::RegistrationStore;
use portzero_daemon::net::dns::DnsFirstHitPolicy;
use portzero_daemon::net::overlay::{OverlayConfig, OverlayNetwork};
use portzero_daemon::net::service_table::ServiceTable;
use tokio::sync::{Notify, RwLock};

const DEFAULT_ITERATIONS: usize = 20;
const DEFAULT_OVERLAY_SETTLE_MS: u64 = 6_000;
const CURL_TIMEOUT_SECS: &str = "1";

#[derive(Debug, Clone)]
struct Sample {
    order: usize,
    policy: DnsFirstHitPolicy,
    iteration: usize,
    domain: String,
    elapsed: Duration,
    attempts: Vec<Attempt>,
}

#[derive(Debug, Clone)]
struct Attempt {
    status_code: Option<i32>,
    elapsed: Duration,
    total_at_end: Duration,
}

fn main() {
    if std::env::args().any(|arg| arg == "--child-http") {
        child_http();
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");
    runtime.block_on(async {
        let iterations = std::env::var("PZ_FIRST_HIT_BENCH_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_ITERATIONS);
        let policy_filter = std::env::var("PZ_FIRST_HIT_BENCH_POLICY").ok();
        let mut next_order = 0_usize;

        if policy_filter
            .as_deref()
            .is_none_or(|p| p == "fast" || p == "all" || p == "both")
        {
            let fast = measure_policy(DnsFirstHitPolicy::FastScan, iterations, next_order).await;
            next_order += fast.len();
            print_samples(&fast);
            print_stats("dns first-hit fast-scan", &fast);
        }

        if policy_filter
            .as_deref()
            .is_none_or(|p| p == "proactive" || p == "all" || p == "both")
        {
            let proactive =
                measure_policy(DnsFirstHitPolicy::ProactiveVip, iterations, next_order).await;
            next_order += proactive.len();
            print_samples(&proactive);
            print_stats("dns first-hit proactive-vip", &proactive);
        }

        if policy_filter
            .as_deref()
            .is_none_or(|p| p == "proactive-hold" || p == "all" || p == "both")
        {
            let proactive_hold =
                measure_policy(DnsFirstHitPolicy::ProactiveVipHold, iterations, next_order).await;
            print_samples(&proactive_hold);
            print_stats("dns first-hit proactive-vip-hold", &proactive_hold);
        }
    });
}

async fn measure_policy(
    policy: DnsFirstHitPolicy,
    iterations: usize,
    order_offset: usize,
) -> Vec<Sample> {
    let dns_rescan = Arc::new(Notify::new());
    let overlay = Arc::new(
        OverlayNetwork::start(
            OverlayConfig {
                dns_first_hit_policy: policy,
                ..OverlayConfig::default()
            },
            dns_rescan.clone(),
        )
        .await
        .expect("start overlay; run this benchmark with the privileges required for TUN/DNS"),
    );
    let settle = std::env::var("PZ_FIRST_HIT_BENCH_OVERLAY_SETTLE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_OVERLAY_SETTLE_MS);
    if settle > 0 {
        tokio::time::sleep(Duration::from_millis(settle)).await;
    }

    let store: RegistrationStore = Arc::new(RwLock::new(HashMap::new()));
    let refresh = tokio::spawn(run_fast_refresh(
        overlay.clone(),
        store.clone(),
        dns_rescan.clone(),
    ));

    let mut samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let domain = format!("bench-first-hit-{}-{i}.portzero.local", policy_name(policy));
        let (mut child, ready_at) = spawn_child_http(&domain);
        let sample = wait_for_curl_success(policy, order_offset + i, i, &domain, ready_at);
        samples.push(sample);
        let _ = child.kill();
        let _ = child.wait();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    refresh.abort();
    overlay.shutdown().await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    samples
}

async fn run_fast_refresh(
    overlay: Arc<OverlayNetwork>,
    store: RegistrationStore,
    dns_rescan: Arc<Notify>,
) {
    let log_refresh = std::env::var_os("PZ_FIRST_HIT_BENCH_REFRESH_LOG").is_some();
    let started_at = Instant::now();
    let mut refresh_count = 0_u64;
    loop {
        dns_rescan.notified().await;
        refresh_count += 1;
        let refresh_start = Instant::now();
        if log_refresh {
            eprintln!(
                "refresh-start count={refresh_count} since_start={}",
                fmt_duration(started_at.elapsed())
            );
        }
        let Some(services) = scan_network_process_services(&store).await else {
            if log_refresh {
                eprintln!(
                    "refresh-timeout count={refresh_count} elapsed={}",
                    fmt_duration(refresh_start.elapsed())
                );
            }
            continue;
        };
        if log_refresh {
            let names = services
                .iter()
                .map(|svc| format!("{}:{}->{}", svc.name, svc.service_port, svc.real_addr))
                .collect::<Vec<_>>()
                .join(",");
            eprintln!(
                "refresh-services count={refresh_count} elapsed={} services={} names=[{}]",
                fmt_duration(refresh_start.elapsed()),
                services.len(),
                names
            );
        }
        let table = build_overlay_table(&services);
        if let Err(e) = overlay.update_services(table).await {
            eprintln!("fast refresh failed: {e:#}");
        } else if log_refresh {
            eprintln!(
                "refresh-applied count={refresh_count} elapsed={}",
                fmt_duration(refresh_start.elapsed())
            );
        }
    }
}

fn build_overlay_table(services: &[DiscoveredNetworkService]) -> ServiceTable {
    let mut table = ServiceTable::new();
    for svc in services {
        table.register(svc.name.clone(), svc.real_addr, svc.service_port, svc.pid);
    }
    table
}

fn spawn_child_http(domain: &str) -> (Child, Instant) {
    let exe = std::env::current_exe().expect("current bench exe");
    let mut child = Command::new(exe)
        .arg("--child-http")
        .env("PZ_TUNNEL", domain)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child HTTP listener");

    let stdout = child.stdout.as_mut().expect("child stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read child readiness");
    assert!(
        line.starts_with("ready "),
        "child did not report readiness: {line:?}"
    );
    (child, Instant::now())
}

fn child_http() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    println!("ready {}", listener.local_addr().expect("local addr"));
    std::io::stdout().flush().ok();

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            break;
        };
        handle_http(&mut stream);
    }
}

fn handle_http(stream: &mut TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0_u8; 1024];
    let _ = stream.read(&mut buf);
    let body = b"ok\n";
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
}

fn wait_for_curl_success(
    policy: DnsFirstHitPolicy,
    order: usize,
    iteration: usize,
    domain: &str,
    ready_at: Instant,
) -> Sample {
    let url = format!("http://{domain}/");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut attempts = 0_u32;
    let mut attempt_records = Vec::new();
    let verbose = std::env::var_os("PZ_FIRST_HIT_BENCH_VERBOSE").is_some();

    loop {
        attempts += 1;
        let attempt_start = Instant::now();
        let status = Command::new("curl")
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "--max-time",
                CURL_TIMEOUT_SECS,
                "--noproxy",
                "*",
                &url,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let attempt_elapsed = attempt_start.elapsed();
        let total_at_end = ready_at.elapsed();
        let status_code = status
            .as_ref()
            .ok()
            .and_then(std::process::ExitStatus::code);
        attempt_records.push(Attempt {
            status_code,
            elapsed: attempt_elapsed,
            total_at_end,
        });
        if verbose {
            eprintln!(
                "attempt-detail order={order} policy={} iteration={iteration} attempt={attempts} status={:?} attempt={} total={}",
                policy_name(policy),
                status_code,
                fmt_duration(attempt_elapsed),
                fmt_duration(total_at_end)
            );
        }

        if matches!(status, Ok(s) if s.success()) {
            return Sample {
                order,
                policy,
                iteration,
                domain: domain.to_string(),
                elapsed: ready_at.elapsed(),
                attempts: attempt_records,
            };
        }
        assert!(
            Instant::now() < deadline,
            "curl did not succeed for {domain} after {attempts} attempts"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn print_samples(samples: &[Sample]) {
    for sample in samples {
        let attempts = sample
            .attempts
            .iter()
            .enumerate()
            .map(|(idx, attempt)| {
                format!(
                    "#{}:status={:?},attempt={},total={}",
                    idx + 1,
                    attempt.status_code,
                    fmt_duration(attempt.elapsed),
                    fmt_duration(attempt.total_at_end)
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "sample order={} policy={} iteration={} elapsed={} attempts={} domain={} {}",
            sample.order,
            policy_name(sample.policy),
            sample.iteration,
            fmt_duration(sample.elapsed),
            sample.attempts.len(),
            sample.domain,
            attempts
        );
    }
}

fn print_stats(label: &str, samples: &[Sample]) {
    let mut sorted = samples.iter().map(|s| s.elapsed).collect::<Vec<_>>();
    sorted.sort_unstable();

    let p50 = percentile(&sorted, 50);
    let p95 = percentile(&sorted, 95);
    let max = sorted.last().copied().unwrap_or_default();

    println!(
        "{label}: n={} p50={} p95={} max={}",
        sorted.len(),
        fmt_duration(p50),
        fmt_duration(p95),
        fmt_duration(max)
    );
}

fn percentile(sorted: &[Duration], pct: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}

fn fmt_duration(duration: Duration) -> String {
    format!("{:.2}ms", duration.as_secs_f64() * 1000.0)
}

fn policy_name(policy: DnsFirstHitPolicy) -> &'static str {
    match policy {
        DnsFirstHitPolicy::FastScan => "fast",
        DnsFirstHitPolicy::ProactiveVip => "proactive",
        DnsFirstHitPolicy::ProactiveVipHold => "proactive-hold",
    }
}
