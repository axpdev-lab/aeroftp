#![cfg(feature = "proto_native_rsync")]

use clap::{Parser, ValueEnum};
use ftp_client_gui_lib::rsync_native_proto::server::{serve_stdio, ProtoServeMode, ProtoServeOptions};
use ftp_client_gui_lib::rsync_native_proto::types::ProtocolVersion;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliMode {
    Upload,
    Download,
}

#[derive(Debug, Parser)]
#[command(name = "rsync_proto_serve")]
#[command(about = "Dev-only RSNP stdio server for Strada C live tests")]
struct Cli {
    #[arg(long)]
    probe: bool,

    #[arg(long, value_enum)]
    mode: Option<CliMode>,

    #[arg(long)]
    target: Option<PathBuf>,

    #[arg(long, default_value_t = 31)]
    protocol: u32,

    #[arg(long)]
    stats: bool,
}

fn main() {
    let cli = Cli::parse();
    if cli.probe {
        println!("rsnp-proto server protocol {}", cli.protocol);
        return;
    }

    let mode = match cli.mode {
        Some(CliMode::Upload) => ProtoServeMode::Upload,
        Some(CliMode::Download) => ProtoServeMode::Download,
        None => {
            eprintln!("--mode is required unless --probe is used");
            std::process::exit(2);
        }
    };

    let target = match cli.target {
        Some(target) => target,
        None => {
            eprintln!("--target is required unless --probe is used");
            std::process::exit(2);
        }
    };

    let options = ProtoServeOptions {
        mode,
        target,
        protocol: ProtocolVersion(cli.protocol),
        emit_stats: cli.stats,
        max_frame_size: 32 * 1024 * 1024,
    };

    if let Err(error) = serve_stdio(options) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}