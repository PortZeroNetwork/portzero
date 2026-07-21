use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use portzero_daemon::discovery::{scan_network_process_services, scan_network_services};
use portzero_daemon::management::RegistrationStore;
use tokio::sync::RwLock;

const BENCH_DOMAIN: &str = "bench-discovery.portzero.local:80";
const ITERATIONS: usize = 25;

fn main() {
    if std::env::args().any(|arg| arg == "--child-listener") {
        child_listener();
        return;
    }

    let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");
    runtime.block_on(async {
        let mut child = spawn_child_listener();
        let store: RegistrationStore = Arc::new(RwLock::new(HashMap::new()));

        wait_until_discovered(&store).await;

        let fast = measure_fast_scan(&store).await;
        let full = measure_full_scan(&store).await;

        print_stats("fast process-only overlay scan", &fast);
        print_stats("full overlay scan", &full);

        let _ = child.kill();
        let _ = child.wait();
    });
}

fn child_listener() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    println!("ready {}", listener.local_addr().expect("local addr"));
    std::io::stdout().flush().ok();

    for stream in listener.incoming() {
        if stream.is_err() {
            break;
        }
    }
}

fn spawn_child_listener() -> Child {
    let exe = std::env::current_exe().expect("current bench exe");
    let mut child = Command::new(exe)
        .arg("--child-listener")
        .env("PZ_TUNNEL", BENCH_DOMAIN)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child listener");

    let stdout = child.stdout.as_mut().expect("child stdout");
    let mut reader = std::io::BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read child readiness");
    assert!(
        line.starts_with("ready "),
        "child did not report readiness: {line:?}"
    );

    child
}

async fn wait_until_discovered(store: &RegistrationStore) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if scan_network_process_services(store)
            .await
            .unwrap_or_default()
            .iter()
            .any(|svc| svc.name == "bench-discovery")
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("bench child was not discovered before timeout");
}

async fn measure_fast_scan(store: &RegistrationStore) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let services = scan_network_process_services(store)
            .await
            .unwrap_or_default();
        assert!(
            services.iter().any(|svc| svc.name == "bench-discovery"),
            "fast scan did not find bench child"
        );
        samples.push(start.elapsed());
    }
    samples
}

async fn measure_full_scan(store: &RegistrationStore) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        let services = scan_network_services(store).await;
        assert!(
            services.iter().any(|svc| svc.name == "bench-discovery"),
            "full scan did not find bench child"
        );
        samples.push(start.elapsed());
    }
    samples
}

fn print_stats(label: &str, samples: &[Duration]) {
    let mut sorted = samples.to_vec();
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
