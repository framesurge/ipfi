use std::{collections::HashMap, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Move to the root of the project
    std::env::set_current_dir("../../")?;

    let args = std::env::args().collect::<Vec<_>>();
    let num_benches = args[1].parse::<usize>()?;

    println!("Benchmarking...");

    let ipfi_metrics = bench_avg("ipfi", num_benches)?;
    let grpc_metrics = bench_avg("grpc", num_benches)?;

    // All programs should have the same keys
    let mut keys: Vec<&String> = ipfi_metrics.keys().collect();
    keys.sort();
    for k in keys {
        let ipfi_val = ipfi_metrics.get(k).unwrap();

        let grpc_val = grpc_metrics
            .get(k)
            .expect("metric disparity (different programs)");
        let grpc_differential = calc_percent_differential(*grpc_val, *ipfi_val);
        let ipfi_beats_grpc = grpc_differential > 0.0;

        println!(
            "Metric '{}': {} ({:.1}μs) is {:.1}% faster than {} ({:.1}μs).",
            k,
            if ipfi_beats_grpc { "IPFI" } else { "gRPC" },
            if ipfi_beats_grpc { ipfi_val } else { grpc_val },
            grpc_differential.abs(),
            if !ipfi_beats_grpc { "IPFI" } else { "gRPC" },
            if !ipfi_beats_grpc { ipfi_val } else { grpc_val },
        );
    }

    Ok(())
}

/// Calculates, as a percentage, how much faster `new` is than `old`. If this is negative, `old` was faster by the given amount
fn calc_percent_differential(old: f64, new: f64) -> f64 {
    let assuming_new = (old / new - 1.0) * 100.0;
    if assuming_new > 0.0 {
        assuming_new
    } else {
        -(new / old - 1.0) * 100.0
    }
}

fn bench_avg(name: &str, times: usize) -> Result<HashMap<String, f64>, Box<dyn std::error::Error>> {
    // Build the server and client once at the start
    let server_build = Command::new("cargo")
        .args(&[
            "build",
            "--release",
            "--bin",
            &format!("{}-bench-server", name),
        ])
        .current_dir(&format!("benches/{}", name))
        .output()?;
    let client_build = Command::new("cargo")
        .args(&[
            "build",
            "--release",
            "--bin",
            &format!("{}-bench-client", name),
        ])
        .current_dir(&format!("benches/{}", name))
        .output()?;
    if !server_build.status.success() {
        return Err("failed to build server".into());
    } else if !client_build.status.success() {
        return Err("failed to build client".into());
    }
    // Start running the server
    let mut server = Command::new(&format!("target/release/{}-bench-server", name)).spawn()?;
    // Give the server time to start up
    std::thread::sleep(std::time::Duration::from_secs(5));

    let mut avg = bench_program(name)?;
    for i in 0..(times - 1) {
        let m = bench_program(name)?;
        for (k, curr_avg) in avg.iter_mut() {
            let new_val = m
                .get(k)
                .expect("metric disparity (different runs of same program)");
            *curr_avg = update_avg(*curr_avg, i, *new_val);
        }
    }

    server.kill()?;

    Ok(avg)
}

fn update_avg(curr_avg: f64, num_elems: usize, new_elem: f64) -> f64 {
    (curr_avg * num_elems as f64 + new_elem) / (num_elems as f64 + 1.0)
}

fn bench_program(name: &str) -> Result<HashMap<String, f64>, Box<dyn std::error::Error>> {
    let client_output = Command::new(&format!("target/release/{}-bench-client", name)).output()?;
    let client_output = String::from_utf8(client_output.stdout)?;

    let mut metrics = HashMap::new();
    for line in client_output.lines() {
        let parts = line.split(':').collect::<Vec<_>>();
        // Skip lines that aren't key-value pairs
        if parts.len() != 2 {
            continue;
        }
        let k = parts[0].trim();
        let v = parts[1].trim();
        let v_num = v.parse::<f64>()?;

        metrics.insert(k.to_string(), v_num);
    }

    Ok(metrics)
}
