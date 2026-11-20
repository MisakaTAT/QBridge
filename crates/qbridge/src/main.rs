use qbridge::{app, config::Config};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let mut config_path = PathBuf::from("qbridge.toml");
    let mut check = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => {
                let Some(path) = arguments.next() else {
                    eprintln!("--config requires a path");
                    std::process::exit(2);
                };
                config_path = PathBuf::from(path);
            }
            "--check" => check = true,
            "--help" | "-h" => {
                println!("usage: qbridge [--config PATH] [--check]");
                return;
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    if check {
        match Config::load(&config_path).and_then(|config| config.resolve()) {
            Ok(_) => println!("configuration is valid"),
            Err(error) => {
                eprintln!("configuration error: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(error) = app::run(config_path).await {
        eprintln!("qbridge stopped: {error}");
        std::process::exit(1);
    }
}
