use rmcp::{transport::stdio, ServiceExt};
use stapler_mcp::daemon_locator::{self, LaunchMode};
use stapler_mcp::thin_client::ThinClient;

fn main() {
    // Drop-in compatibility: `stapler-mcp --daemon` used to run the daemon.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if daemon_locator::launch_mode(&args) == LaunchMode::Daemon {
        daemon_locator::exec_daemon(args);
    }

    // Same single-threaded runtime shape as the daemon (see `stapler-mcp-daemon`).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, run_thin_client());
}

async fn run_thin_client() {
    let client = ThinClient::new();
    let service = match client.serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stapler-mcp: failed to start stdio transport: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = service.waiting().await {
        eprintln!("stapler-mcp: stdio transport error: {e}");
        std::process::exit(1);
    }
}
