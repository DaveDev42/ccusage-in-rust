use anyhow::Result;
use clap::Parser;

use ccusage_rs::cli::{Cli, run};

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli)
}
