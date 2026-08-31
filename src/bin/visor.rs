//! hl-visor - Process supervisor for hyperlicked validator nodes
//!
//! # Usage
//!
//! ```bash
//! # Start node (non-validator)
//! hl-visor run-non-validator
//!
//! # Start node (validator)
//! hl-visor run-validator
//!
//! # Check version
//! hl-visor version
//!
//! # Show config
//! hl-visor config
//!
//! # Manually add upgrade binary
//! hl-visor add-upgrade v1.1.0 /path/to/hl-node
//!
//! # Check upgrade status
//! hl-visor upgrade-status
//! ```

use clap::{Parser, Subcommand};
use hyperlicked::visor::{HealthChecker, ProcessManager, UpgradeManager, VisorConfig};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tokio::signal;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "hl-visor")]
#[command(about = "Process supervisor for hyperlicked validator nodes")]
#[command(version = VERSION)]
struct Cli {
    /// Path to configuration file
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Log level (error, warn, info, debug, trace)
    #[arg(short, long, global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the daemon as a non-validator
    RunNonValidator,

    /// Run the daemon as a validator
    RunValidator,

    /// Show visor version
    Version,

    /// Show current configuration
    Config,

    /// Add an upgrade binary manually
    AddUpgrade {
        /// Version name (e.g., v1.1.0)
        version: String,

        /// Path to the binary
        binary_path: PathBuf,
    },

    /// Check upgrade status
    UpgradeStatus,

    /// Initialize visor directories and config
    Init {
        /// Path to initial binary
        #[arg(short, long)]
        binary: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    // Load configuration
    let config = if let Some(config_path) = &cli.config {
        VisorConfig::load(config_path)?
    } else {
        VisorConfig::load_default()?
    };

    match cli.command {
        Commands::RunNonValidator => run_daemon(config, false).await,
        Commands::RunValidator => run_daemon(config, true).await,
        Commands::Version => {
            println!("hl-visor {}", VERSION);
            Ok(())
        }
        Commands::Config => {
            let json = serde_json::to_string_pretty(&config)?;
            println!("{}", json);
            Ok(())
        }
        Commands::AddUpgrade {
            version,
            binary_path,
        } => add_upgrade(config, &version, &binary_path).await,
        Commands::UpgradeStatus => upgrade_status(config).await,
        Commands::Init { binary } => init_visor(config, binary.as_deref()).await,
    }
}

/// Run the daemon with supervision
async fn run_daemon(config: VisorConfig, is_validator: bool) -> anyhow::Result<()> {
    info!("Starting hl-visor (validator={})", is_validator);

    // Ensure directories exist
    config.ensure_dirs()?;

    // Check that binary exists
    let binary_path = config.current_binary();
    if !binary_path.exists() {
        error!(
            "Binary not found at {}. Run 'hl-visor init --binary <path>' first.",
            binary_path.display()
        );
        return Err(anyhow::anyhow!("Binary not found"));
    }

    // Create process manager
    let mut process_manager = ProcessManager::new(config.clone(), is_validator);

    // Create channels for coordination
    let (stop_tx, stop_rx) = mpsc::channel::<()>(1);
    let (health_stop_tx, health_stop_rx) = mpsc::channel::<()>(1);
    let (health_status_tx, mut health_status_rx) = mpsc::channel(16);

    // Create health checker
    let mut health_checker = HealthChecker::new(config.clone());

    // Create upgrade manager
    let mut upgrade_manager = UpgradeManager::new(config.clone());

    // Spawn health checker task
    let health_handle = tokio::spawn(async move {
        health_checker.run(health_stop_rx, health_status_tx).await;
    });

    // Run process manager in background
    let shutdown_signal = process_manager.shutdown_signal();
    let process_handle = tokio::spawn(async move { process_manager.run(stop_rx).await });

    // Main supervision loop
    let result = tokio::select! {
        // Handle Ctrl+C
        _ = signal::ctrl_c() => {
            info!("Received Ctrl+C, shutting down...");
            shutdown_signal.store(true, Ordering::SeqCst);
            let _ = stop_tx.send(()).await;
            let _ = health_stop_tx.send(()).await;
            Ok(())
        }

        // Handle process manager completion
        result = process_handle => {
            match result {
                Ok(Ok(())) => {
                    info!("Process manager exited normally");
                    Ok(())
                }
                Ok(Err(e)) => {
                    error!("Process manager error: {}", e);
                    Err(anyhow::anyhow!("Process manager error: {}", e))
                }
                Err(e) => {
                    error!("Process manager task failed: {}", e);
                    Err(anyhow::anyhow!("Task join error: {}", e))
                }
            }
        }

        // Handle health status updates
        _ = async {
            while let Some(status) = health_status_rx.recv().await {
                if status.healthy {
                    // Update upgrade manager with current height
                    if let Some(height) = status.block_height {
                        upgrade_manager.set_height(height);

                        // Check for pending upgrade
                        if upgrade_manager.check_upgrade_info().is_some() {
                            if upgrade_manager.should_upgrade() {
                                info!("Upgrade triggered at height {}", height);
                                // TODO: Coordinate upgrade
                            }
                        }
                    }
                }
            }
        } => {
            Ok(())
        }
    };

    // Wait for health checker to finish
    let _ = health_handle.await;

    result
}

/// Add a binary for a specific version
async fn add_upgrade(
    config: VisorConfig,
    version: &str,
    binary_path: &PathBuf,
) -> anyhow::Result<()> {
    let upgrade_manager = UpgradeManager::new(config.clone());

    // Verify the binary exists
    if !binary_path.exists() {
        error!("Binary not found at {}", binary_path.display());
        return Err(anyhow::anyhow!("Binary not found"));
    }

    // Verify signature if configured
    if !config.skip_verify {
        let sig_path = binary_path.with_extension("sig");
        if sig_path.exists() {
            info!("Verifying signature...");
            upgrade_manager.verify_signature(binary_path)?;
        } else {
            warn!("No signature file found at {}", sig_path.display());
        }
    }

    // Add the binary
    let target_path = upgrade_manager.add_binary(version, binary_path).await?;
    println!("Binary added: {}", target_path.display());

    Ok(())
}

/// Show upgrade status
async fn upgrade_status(config: VisorConfig) -> anyhow::Result<()> {
    let mut upgrade_manager = UpgradeManager::new(config.clone());

    // Check for upgrade info
    upgrade_manager.check_upgrade_info();

    let status = upgrade_manager.status();
    println!("{}", status);

    // List installed versions
    let binaries_dir = config.binaries_dir();
    if binaries_dir.exists() {
        println!("\nInstalled Versions:");
        for entry in std::fs::read_dir(&binaries_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap().to_string_lossy();

                // Check if this is the current version
                let current_link = config.current_binary_link();
                let is_current = if current_link.is_symlink() {
                    std::fs::read_link(&current_link)
                        .map(|target| target == path)
                        .unwrap_or(false)
                } else {
                    false
                };

                if is_current {
                    println!("  {} (current)", name);
                } else {
                    println!("  {}", name);
                }
            }
        }
    }

    Ok(())
}

/// Initialize visor directories and configuration
async fn init_visor(config: VisorConfig, binary: Option<&Path>) -> anyhow::Result<()> {
    info!("Initializing visor at {}", config.daemon_home.display());

    // Create directories
    config.ensure_dirs()?;

    // Save default config
    let config_path = config.daemon_home.join("visor/visor_config.json");
    if !config_path.exists() {
        config.save(&config_path)?;
        println!("Created config: {}", config_path.display());
    }

    // If binary provided, install it as v0.1.0
    if let Some(binary_path) = binary {
        if !binary_path.exists() {
            error!("Binary not found at {}", binary_path.display());
            return Err(anyhow::anyhow!("Binary not found"));
        }

        let version = format!("v{}", VERSION);
        let upgrade_manager = UpgradeManager::new(config.clone());
        let target_path = upgrade_manager.add_binary(&version, binary_path).await?;

        // Create current symlink
        let current_link = config.current_binary_link();
        let version_dir = target_path.parent().unwrap();

        if current_link.exists() || current_link.is_symlink() {
            std::fs::remove_file(&current_link).ok();
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(version_dir, &current_link)?;

        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(version_dir, &current_link)?;

        println!(
            "Installed binary: {} -> {}",
            current_link.display(),
            version_dir.display()
        );
    }

    println!("\nVisor initialized successfully!");
    println!("\nDirectory structure:");
    println!("  {}/", config.daemon_home.display());
    println!("  ├── visor/");
    println!("  │   └── visor_config.json");
    println!("  ├── binaries/");
    println!("  │   └── current -> <version>");
    println!("  └── data/");
    println!("      └── visor_logs/");

    if binary.is_none() {
        println!("\nNext steps:");
        println!("  1. Add a binary: hl-visor add-upgrade v0.1.0 /path/to/hl-node");
        println!("  2. Start the node: hl-visor run-non-validator");
    }

    Ok(())
}
