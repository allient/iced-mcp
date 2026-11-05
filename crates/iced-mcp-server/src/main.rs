//! `iced-mcp`: an MCP server (stdio) that lets AI agents launch, inspect and
//! drive iced desktop apps that attach the `iced-mcp` bridge.
//!
//! Register it with an agent, e.g. `.mcp.json`:
//! `{"mcpServers":{"iced":{"command":"iced-mcp"}}}`.

mod accessibility;
mod bundle;
mod conn;
mod diagnose;
mod diff;
mod expect;
mod flow;
mod keys;
mod ops;
mod params;
mod record;
mod script;
mod suite;
mod tools;
mod view;

use rmcp::ServiceExt as _;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // A script's Luau VM (spawned by the server, see `script`): no runtime.
    if args.first().map(String::as_str) == Some(script::WORKER_COMMAND) {
        std::process::exit(script::worker::main(
            std::io::stdin().lock(),
            std::io::stdout(),
        ));
    }
    tokio::runtime::Runtime::new()?.block_on(serve(args))
}

async fn serve(args: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("iced-mcp {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().map(String::as_str) == Some("run-flow") {
        std::process::exit(run_flow(&args[1..]).await);
    }
    if args.first().map(String::as_str) == Some("run-script") {
        std::process::exit(run_script(&args[1..]).await);
    }
    if args.first().map(String::as_str) == Some("run-suite") {
        std::process::exit(run_suite(&args[1..]).await);
    }
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}{}\n\n{}", script::USAGE, suite::USAGE);
        return Ok(());
    }
    // stdout is the MCP channel: diagnostics go to stderr only.
    let service = tools::Server::new().serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

const USAGE: &str = "\
iced-mcp                      MCP server on stdio (what agents run)
iced-mcp run-flow [NAME]      replay .iced-mcp/flows/NAME.json; no NAME lists them
    --root DIR                project root holding .iced-mcp/ (default: current dir)
    --update-baselines        rewrite screenshot_diff baselines from this run
    --prerequisite-ok         confirm a fragment's prerequisite holds
Exit status: 0 when every step passed, 1 otherwise, 2 on bad arguments.
";

/// `iced-mcp run-script`: one Luau script, for CI and the shell.
async fn run_script(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", script::USAGE);
        return 0;
    }
    let options = match script::parse_cli(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n\n{}", script::USAGE);
            return 2;
        }
    };
    let (code, report) = tools::Server::new().run_script_cli(&options).await;
    println!("{report}");
    code
}

/// `iced-mcp run-suite`: every flow (or `--only` some), for CI.
async fn run_suite(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{}", suite::USAGE);
        return 0;
    }
    let options = match suite::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n\n{}", suite::USAGE);
            return 2;
        }
    };
    let (code, report) = tools::Server::new().run_suite_cli(&options).await;
    println!("{report}");
    code
}

/// `iced-mcp run-flow`: replays a flow without an agent, for CI.
async fn run_flow(args: &[String]) -> i32 {
    let mut params = params::FlowRunParams::default();
    let mut root = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--root" => match rest.next() {
                Some(dir) => root = Some(std::path::PathBuf::from(dir)),
                None => {
                    eprintln!("--root needs a directory\n\n{USAGE}");
                    return 2;
                }
            },
            "--update-baselines" => params.update_baselines = Some(true),
            "--prerequisite-ok" => params.prerequisite_ok = Some(true),
            name if !name.starts_with('-') && params.name.is_empty() => {
                params.name = name.to_owned()
            }
            other => {
                eprintln!("unexpected argument `{other}`\n\n{USAGE}");
                return 2;
            }
        }
    }
    let (passed, report) = tools::Server::new().run_flow_cli(params, root).await;
    println!("{report}");
    i32::from(!passed)
}
