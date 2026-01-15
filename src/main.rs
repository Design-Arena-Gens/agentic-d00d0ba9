mod api;
mod config;
mod engine;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ethers::types::Address;
use tracing_subscriber::EnvFilter;

use crate::{
    config::BotConfig,
    engine::{
        TradingBot,
        risk::TokenRiskReport,
        scanner::{DexScreenerScanner, GemCandidate},
    },
};

#[derive(Parser)]
#[command(author, version, about = "Agentic memecoin trading bot")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the trading engine continuously
    Run {
        #[arg(long)]
        once: bool,
    },
    /// Scan current market for memecoin opportunities
    Scan,
    /// Evaluate a specific token contract address
    Evaluate { token: Address },
    /// Perform a health check against the configured RPC
    Health,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let cli = Cli::parse();
    let config = BotConfig::from_env()?;

    match cli.command {
        Command::Run { once } => run_bot(config, once).await,
        Command::Scan => scan_market(config).await,
        Command::Evaluate { token } => evaluate_token(config, token).await,
        Command::Health => run_health_check(config).await,
    }
}

async fn run_bot(config: BotConfig, once: bool) -> Result<()> {
    let bot = Arc::new(TradingBot::new(config.clone()).await?);

    if once {
        bot.tick().await
    } else {
        let api_handle = {
            let bot = bot.clone();
            let addr = config.monitoring.bind_addr;
            tokio::spawn(async move {
                if let Err(err) = api::run(bot, addr).await {
                    tracing::error!(error = ?err, "monitoring api terminated");
                }
            })
        };

        let trading_handle = {
            let bot = bot.clone();
            tokio::spawn(async move {
                if let Err(err) = bot.run().await {
                    tracing::error!(error = ?err, "trading loop terminated");
                }
            })
        };

        tokio::select! {
            _ = trading_handle => {},
            _ = api_handle => {},
        }

        Ok(())
    }
}

async fn scan_market(config: BotConfig) -> Result<()> {
    let scanner = DexScreenerScanner::default();
    let candidates = scanner.discover_candidates(&config).await?;

    if candidates.is_empty() {
        println!("No qualifying opportunities found.");
        return Ok(());
    }

    println!("Top opportunities:");
    for candidate in candidates {
        print_candidate(&candidate);
    }
    Ok(())
}

async fn evaluate_token(config: BotConfig, token: Address) -> Result<()> {
    let scanner = DexScreenerScanner::default();
    let risk = engine::risk::RiskAnalyzer::new(config.clone());

    let candidates = scanner
        .fetch_token_candidates(&token, &config)
        .await
        .context("fetch token candidates")?;

    if candidates.is_empty() {
        println!("No live liquidity pools discovered for token {token:?}");
        return Ok(());
    }

    for candidate in candidates {
        let report = risk.evaluate_candidate(&candidate).await?;
        print_candidate(&candidate);
        print_risk(&report);
    }

    Ok(())
}

async fn run_health_check(config: BotConfig) -> Result<()> {
    let bot = TradingBot::new(config).await?;
    let status = bot.health_check().await?;
    println!("Health: {status}");
    Ok(())
}

fn print_candidate(candidate: &GemCandidate) {
    println!(
        "- {symbol} ({name}) | liquidity ${liquidity:.0} | 24h volume ${volume:.0} | price change 1h {pc:+.2}% | buy pressure {bp:.0}%",
        symbol = candidate.token_symbol,
        name = candidate.token_name,
        liquidity = candidate.liquidity_usd,
        volume = candidate.volume24h_usd,
        pc = candidate.price_change_h1,
        bp = candidate.buy_pressure_ratio * 100.0,
    );
}

fn print_risk(report: &TokenRiskReport) {
    println!(
        "  risk score {:.2} | safe: {}",
        report.score, report.is_safe
    );
    if !report.flags.is_empty() {
        println!("  flags: {}", report.flags.join(", "));
    }
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,trading_bot=debug,axum::rejection=trace")),
        )
        .with_target(false)
        .try_init();
}
