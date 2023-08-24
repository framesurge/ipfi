use std::{collections::HashMap, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Move to the root of the project
    std::env::set_current_dir("../../")?;

    let args = std::env::args().collect::<Vec<_>>();
    let num_benches = args[1].parse::<usize>()?;

    println!("Using {num_benches} runs.");
    if num_benches <= 10 {
        println!("[WARNING]: Small number of benchmark runs, results may be inaccurate.");
    } else if num_benches <= 1000 {
        println!("[INFO]: Medium number of benchmark runs, results will be useful, but more runs are advised.");
    } else if num_benches <= 10000 {
        println!("[INFO]: High number of benchmark runs, results should be accurate.")
    } else {
        println!("[INFO]: Extremely high number of benchmark runs, results should be highly accurate.");
    }

    // This will store the results of the benchmarks for each program, and then they can be efficiently combined,
    // rather than running them for every possible combination of two
    let mut results = HashMap::new();

    println!("Benchmarking...");
    let programs = ["ipfi_blocking", "ipfi_async", "grpc"];
    for op in programs {
        let metrics = bench_avg(op, num_benches)?;
        results.insert(op, metrics);
    }

    // Now, for every combination of two in that list, produce a comparative benchmark
    let mut final_output = String::new();
    for i in 0..programs.len() {
        for j in i + 1..programs.len() {
            let first = &programs[i];
            let second = &programs[j];

            let mut output = format!("--- {} vs {} ---\n", first, second);

            // This will print the results
            output += &bench_x_against_y(first, second, &results)?;
            output += "\n\n";
            final_output.push_str(&output);
        }
    }

    println!("{final_output}");

    Ok(())
}

/// Combines the two given programs' benchmarks to produce human-readable results. This returns the string to print.
fn bench_x_against_y(
    x: &str,
    y: &str,
    results: &HashMap<&str, HashMap<String, f64>>,
) -> Result<String, Box<dyn std::error::Error>> {
    let x_metrics = results.get(x).ok_or("missing metrics")?;
    let y_metrics = results.get(y).ok_or("missing metrics")?;

    let mut lines = Vec::new();
    // All programs should have the same keys
    let mut keys: Vec<&String> = x_metrics.keys().collect();
    keys.sort();
    for k in keys {
        let x_val = x_metrics.get(k).unwrap();

        let y_val = y_metrics
            .get(k)
            .expect("metric disparity (different programs)");

        // I am well aware that there are cleaner ways to do this, but I favour the clarity of this verbose approach
        let faster;
        let slower;
        let faster_time;
        let slower_time;
        if x_val < y_val {
            faster = x;
            slower = y;
            faster_time = x_val;
            slower_time = y_val;
        } else {
            faster = y;
            slower = x;
            faster_time = y_val;
            slower_time = x_val;
        };

        // We are comparing speeds, not times, which resolves a substantial amount of statistical ambiguity!
        let faster_speed = 1.0 / faster_time;
        let slower_speed = 1.0 / slower_time;

        let times_faster = faster_speed / slower_speed;
        // (new - old)/old * 100 = (new/old - 1) * 100 = (times_faster - 1) * 100
        let percent_diff = (times_faster - 1.0) * 100.0;

        let line = format!("Metric '{k}': {faster} ({faster_time:.1}μs) is {percent_diff:.1}% ({times_faster:.1}x) faster than {slower} ({slower_time:.1}μs).");
        lines.push(line);
    }

    Ok(lines.join("\n"))
}

fn bench_avg(name: &str, times: usize) -> Result<HashMap<String, f64>, Box<dyn std::error::Error>> {
    // Cargo naming conventions
    let cargo_name = &name.replace("_", "-");

    // Build the server and client once at the start
    let server_build = Command::new("cargo")
        .args(&[
            "build",
            "--release",
            "--bin",
            &format!("{}-bench-server", cargo_name),
        ])
        .current_dir(&format!("benches/{}", name))
        .output()?;
    let client_build = Command::new("cargo")
        .args(&[
            "build",
            "--release",
            "--bin",
            &format!("{}-bench-client", cargo_name),
        ])
        .current_dir(&format!("benches/{}", name))
        .output()?;
    if !server_build.status.success() {
        return Err("failed to build server".into());
    } else if !client_build.status.success() {
        return Err("failed to build client".into());
    }
    // Start running the server
    let mut server =
        Command::new(&format!("target/release/{}-bench-server", cargo_name)).spawn()?;
    // Give the server time to start up
    std::thread::sleep(std::time::Duration::from_secs(5));

    let mut avg = bench_program(cargo_name)?;
    for i in 0..(times - 1) {
        let m = bench_program(cargo_name)?;
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
