mod commands;
mod discovery;
mod output;
mod socket;

use clap::{Parser, Subcommand};

use commands::browser::BrowserCmd;
use commands::metadata::MetadataCmd;
use commands::remote::RemoteCmd;
use commands::split::SplitCmd;
use commands::terminal::TerminalCmd;
use commands::workspace::WorkspaceCmd;

#[derive(Parser)]
#[command(name = "limux-cli", about = "Control a running limux instance")]
struct Cli {
    /// Path to the limux Unix socket
    #[arg(long, global = true)]
    socket: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Test the connection
    Ping,
    /// Show the server version
    Version,
    /// List all surface IDs
    ListSurfaces,

    // Workspace commands
    #[command(flatten)]
    Workspace(WorkspaceCmd),

    // Split commands
    #[command(flatten)]
    Split(SplitCmd),

    // Browser commands
    #[command(flatten)]
    Browser(BrowserCmd),

    // Metadata commands
    #[command(flatten)]
    Metadata(MetadataCmd),

    // Remote SSH commands
    #[command(flatten)]
    Remote(RemoteCmd),

    // Terminal I/O commands
    #[command(flatten)]
    Terminal(TerminalCmd),
}

fn main() {
    let cli = Cli::parse();

    let socket_path = match discovery::resolve(cli.socket.as_deref()) {
        Ok(path) => path,
        Err(msg) => {
            output::handle(Err(msg), cli.json);
            return;
        }
    };

    let result = match &cli.command {
        Command::Ping => socket::send_command(&socket_path, "ping"),
        Command::Version => socket::send_command(&socket_path, "version"),
        Command::ListSurfaces => socket::send_command(&socket_path, "list_surfaces"),
        Command::Workspace(cmd) => cmd.run(&socket_path),
        Command::Split(cmd) => cmd.run(&socket_path),
        Command::Browser(cmd) => cmd.run(&socket_path),
        Command::Metadata(cmd) => cmd.run(&socket_path),
        Command::Remote(cmd) => cmd.run(&socket_path),
        Command::Terminal(cmd) => cmd.run(&socket_path),
    };

    output::handle(result, cli.json);
}
