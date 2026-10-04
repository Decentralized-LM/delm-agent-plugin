use anyhow::{Context, Result};
use clap::Parser;
use delm::protocol::{Event, HostCommand};
use std::io::{BufRead, Read};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, watch},
};

/// Runtime for the explicitly invoked DeLM skill in stock Codex.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Internal protocol used by lifecycle qualification fixtures.
    #[arg(long, hide = true)]
    stdio: bool,
    #[command(subcommand)]
    command: Option<delm::cli::Command>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.stdio
        || matches!(
            &args.command,
            Some(delm::cli::Command::Run { .. } | delm::cli::Command::CapturedRun { .. })
        )
    {
        delm::lifecycle::ensure_not_worker()?;
    }
    if let Some(
        delm::cli::Command::Run { project, .. } | delm::cli::Command::CapturedRun { project, .. },
    ) = &args.command
    {
        delm::package::preserve_for_run(project)?;
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve(args))
}

async fn serve(args: Args) -> Result<()> {
    if let Some(command) = args.command {
        return delm::cli::execute(command).await;
    }
    anyhow::ensure!(
        args.stdio,
        "In Codex, enter $delm:run followed by your task. Use --help for runtime commands."
    );
    let (commands_tx, commands_rx) = mpsc::channel(64);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<Event>();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let output_cancel = cancel_tx.clone();
    let overflowed = Arc::new(AtomicBool::new(false));
    let input_overflowed = overflowed.clone();
    // A detached blocking reader does not keep Tokio shutdown alive if the
    // host closes output while leaving its input descriptor open.
    let _stdin = std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(std::io::stdin());
        loop {
            let mut line = Vec::new();
            match (&mut reader)
                .take(8 * 1024 * 1024 + 1)
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > 8 * 1024 * 1024 => break,
                Ok(_) => match serde_json::from_slice::<HostCommand>(&line) {
                    Ok(HostCommand::Stop) => {
                        let _ = cancel_tx.send(true);
                        break;
                    }
                    Ok(command) => {
                        // Never block the control reader behind a full update
                        // queue, since that would also delay reading Stop.
                        match commands_tx.try_send(command) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                input_overflowed.store(true, Ordering::Release);
                                break;
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                    Err(_) => {
                        let _ = cancel_tx.send(true);
                        break;
                    }
                },
            }
        }
        let _ = cancel_tx.send(true);
    });
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(mut event) = events_rx.recv().await {
            if matches!(event.kind.as_str(), "stopped" | "error")
                && overflowed.load(Ordering::Acquire)
            {
                event.message.push_str(
                    " Too many updates arrived at once. The last update was not accepted.",
                );
            }
            let mut line = serde_json::to_vec(&event)?;
            line.push(b'\n');
            if let Err(error) = async {
                out.write_all(&line).await?;
                out.flush().await
            }
            .await
            {
                let _ = output_cancel.send(true);
                return Err(error.into());
            }
        }
        Ok::<_, anyhow::Error>(())
    });
    let result = delm::run::serve(commands_rx, events_tx.clone(), cancel_rx).await;
    if let Err(error) = &result {
        let _ = events_tx.send(Event::new("error", format!("{error:#}")));
    }
    drop(events_tx);
    writer.await.context("Native output stopped")??;
    result
}
