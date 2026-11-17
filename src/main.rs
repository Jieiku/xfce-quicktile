//! CLI entry point: parse the command and delegate to the daemon or client.

mod daemon;
mod protocol;
mod state;
mod tiling;
mod x11;

use std::{env, str::FromStr};

use anyhow::{bail, Result};

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".to_string());
    match command.as_str() {
        "daemon" => daemon::run_daemon(),
        "help" | "--help" | "-h" => {
            // Single line so the exact 2-space indent and description
            // alignment survive formatting (multi-line literals would
            // embed source indentation in the output).
            println!(
                "xfce-quicktile {}\nUsage:\n  xfce-quicktile daemon       Start the background daemon\n  xfce-quicktile left         Tile/move one half-step left\n  xfce-quicktile right        Tile/move one half-step right\n  xfce-quicktile up           Tile to upper half / quadrant\n  xfce-quicktile down         Tile to lower half / quadrant\n  xfce-quicktile ping         Check daemon\n  xfce-quicktile stop         Stop daemon",
                env!("CARGO_PKG_VERSION")
            );
            Ok(())
        }
        other => {
            let command = match protocol::Command::from_str(other) {
                Ok(command) => command,
                Err(_) => bail!("unknown command `{other}`; run `xfce-quicktile --help`"),
            };
            daemon::send_command(command)
        }
    }
}
