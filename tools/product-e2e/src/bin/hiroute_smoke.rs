#[cfg(unix)]
mod unix {
    use clap::{Parser, Subcommand};
    use hiroute_product_e2e::smoke;
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    #[derive(Parser)]
    #[command(
        name = "hiroute-smoke",
        about = "Finite production smoke cases; reports preserve proof scope"
    )]
    struct Cli {
        #[command(subcommand)]
        command: Action,
    }
    #[derive(Subcommand)]
    enum Action {
        List {
            #[arg(long)]
            domain: Vec<String>,
            #[arg(long)]
            case: Vec<String>,
        },
        Validate {
            #[arg(long)]
            domain: Vec<String>,
            #[arg(long)]
            case: Vec<String>,
        },
        Run {
            #[arg(long)]
            domain: Vec<String>,
            #[arg(long)]
            case: Vec<String>,
        },
    }
    pub async fn main() -> i32 {
        let action = Cli::parse().command;
        let list_all = matches!(&action, Action::List { domain, case } if domain.is_empty() && case.is_empty());
        let (domains, ids) = match action {
            Action::List { domain, case } | Action::Validate { domain, case } => {
                return match if list_all {
                    Ok(smoke::catalog().to_vec())
                } else {
                    smoke::select(&domain, &case)
                } {
                    Ok(cases) => {
                        println!(
                            "{}",
                            serde_json::json!({"validation_only":true,"cases":cases})
                        );
                        0
                    }
                    Err(e) => {
                        eprintln!("{e}");
                        2
                    }
                };
            }
            Action::Run { domain, case } => (domain, case),
        };
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let mut termination =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => {
                    eprintln!("signal_handler_unavailable");
                    return 2;
                }
            };
        let worker = tokio::task::spawn_blocking(move || smoke::run(&root, domains, ids, cancel));
        tokio::pin!(worker);
        let outcome = tokio::select! {
            result=&mut worker=>result,
            _=tokio::signal::ctrl_c()=>{flag.store(true,Ordering::SeqCst);worker.await},
            _=termination.recv()=>{flag.store(true,Ordering::SeqCst);worker.await},
            _=tokio::time::sleep(std::time::Duration::from_secs(2700))=>{flag.store(true,Ordering::SeqCst);worker.await},
        };
        match outcome {
            Ok(Ok((path, report))) => {
                println!("{}", serde_json::json!({"report":path,"result":report}));
                report.tool_process_exit
            }
            Ok(Err(e)) => {
                eprintln!("{e}");
                2
            }
            Err(_) => {
                eprintln!("smoke_worker_failed");
                1
            }
        }
    }
}
#[tokio::main]
async fn main() {
    #[cfg(unix)]
    let exit = unix::main().await;
    #[cfg(not(unix))]
    let exit = {
        eprintln!("platform_adapter_unavailable");
        2
    };
    std::process::exit(exit);
}
