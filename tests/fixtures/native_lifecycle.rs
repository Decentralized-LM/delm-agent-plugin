//! A harmless native-host fixture using the actual production lifecycle module.
//! This executable has no worker launch or model API code path.
use anyhow::{Context, Result, ensure};
use delm::lifecycle::{self, HookInput, Signal};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

fn main() -> Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.get(1).map(String::as_str) == Some("validate-listing") {
        let listing: Value = serde_json::from_reader(std::io::stdin())?;
        lifecycle::validate_hook_listing(&listing, &std::env::current_exe()?)?;
        println!(
            "All required native plugin handlers are trusted and match the installed contract"
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("lifecycle-hook") {
        let mut text = String::new();
        std::io::stdin()
            .take(1024 * 1024)
            .read_to_string(&mut text)?;
        let value: Value = serde_json::from_str(&text)?;
        let cwd = PathBuf::from(value["cwd"].as_str().context("missing cwd")?);
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(cwd.join("hook-events.jsonl"))?;
        writeln!(log, "{}", value)?;
        let input: HookInput = serde_json::from_value(value)?;
        let action = lifecycle::prepare_hook(input, &std::env::current_exe()?)?;
        if let Some(delivery) = action.delivery {
            fs::write(
                cwd.join("fixture-stop.json"),
                serde_json::to_vec(&delivery.signal)?,
            )?;
        }
        if let Some(output) = action.output {
            println!("{output}");
        }
        return Ok(());
    }
    ensure!(
        args.get(1).map(String::as_str) == Some("run"),
        "expected fixture run"
    );
    ensure!(
        args.get(2).map(String::as_str) == Some("--launch-token"),
        "canonical launch omitted its invocation identity"
    );
    let binding = lifecycle::consume_launch(args.get(3).context("missing launch token")?)?;
    let target = PathBuf::from(
        args.iter()
            .position(|arg| arg == "--pid-file")
            .and_then(|index| args.get(index + 1))
            .context("missing fixture PID path")?,
    );
    let root = target.parent().context("missing fixture directory")?;
    let run_id = uuid::Uuid::new_v4().to_string();
    if args.iter().any(|arg| arg == "--delay-admission") {
        fs::write(
            root.join("fixture-preflight-ready"),
            b"No fixture worker has started",
        )?;
        std::thread::sleep(Duration::from_secs(4));
    }
    if let Err(error) = binding.register(&run_id) {
        fs::write(
            root.join("fixture-exit.json"),
            serde_json::to_vec(
                &json!({"reason":format!("admission_rejected: {error:#}"),"child_started":false}),
            )?,
        )?;
        return Ok(());
    }
    let mut child = Command::new("/bin/sleep").arg("180").spawn()?;
    fs::write(
        target.with_extension("identity"),
        serde_json::to_vec(&json!({
            "codex_thread_id":std::env::var("CODEX_THREAD_ID").ok(),"parent_pid":binding.owner.pid,"binding":binding
        }))?,
    )?;
    fs::write(
        &target,
        serde_json::to_vec(&json!({"parent":std::process::id(),"child":child.id()}))?,
    )?;
    println!("fixture-ready; actual native handshake consumed, no model");
    let until = Instant::now() + Duration::from_secs(180);
    let reason = loop {
        if let Err(error) = binding.check_resources() {
            break format!("ownership_lost: {error:#}");
        }
        if let Some(signal) = binding.pending_signal()? {
            break format!("durable_native_event: {}", signal.event);
        }
        let signal = root.join("fixture-stop.json");
        if signal.exists() {
            let signal: Signal = serde_json::from_slice(&fs::read(signal)?)?;
            if binding.accepts(&signal)? {
                break format!("native_event: {}", signal.event);
            }
        }
        if Instant::now() >= until {
            break "fixture_deadline".into();
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    child.kill().ok();
    child.wait()?;
    binding.unregister()?;
    fs::write(
        root.join("fixture-exit.json"),
        serde_json::to_vec(&json!({"reason":reason}))?,
    )?;
    Ok(())
}
