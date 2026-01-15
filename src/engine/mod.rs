pub mod portfolio;
pub mod risk;
pub mod scanner;
mod trader;

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use ethers::prelude::*;
use tokio::{sync::RwLock, time::sleep};
use tracing::{error, info, instrument};

use crate::config::BotConfig;

use self::{
    portfolio::{Portfolio, PortfolioSnapshot, Position},
    risk::{RiskAnalyzer, TokenRiskReport},
    scanner::{DexScreenerScanner, GemCandidate},
    trader::Trader,
};

const LOOP_INTERVAL: Duration = Duration::from_secs(30);

pub struct TradingBot {
    config: BotConfig,
    trader: Trader,
    scanner: DexScreenerScanner,
    risk: RiskAnalyzer,
    portfolio: Arc<RwLock<Portfolio>>,
}

impl TradingBot {
    pub async fn new(config: BotConfig) -> Result<Self> {
        let trader = Trader::new(config.clone()).await?;
        let scanner = DexScreenerScanner::default();
        let risk = RiskAnalyzer::new(config.clone());
        let portfolio = Arc::new(RwLock::new(Portfolio::load().unwrap_or_default()));

        Ok(Self {
            config,
            trader,
            scanner,
            risk,
            portfolio,
        })
    }

    #[instrument(skip(self), fields(chain = %self.config.chain))]
    pub async fn run(&self) -> Result<()> {
        info!("starting automated trading loop");
        loop {
            if let Err(err) = self.tick().await {
                error!(error = ?err, "tick failed");
            }
            sleep(LOOP_INTERVAL).await;
        }
    }

    #[instrument(skip(self))]
    pub async fn tick(&self) -> Result<()> {
        let mut portfolio = self.portfolio.write().await;
        portfolio.refresh_positions(&self.trader).await?;

        if portfolio.active_positions().len() >= self.config.strategy.max_positions {
            info!("max positions reached, skipping new entries");
            return Ok(());
        }

        let candidates = self
            .scanner
            .discover_candidates(&self.config)
            .await
            .context("discovering candidates")?;

        if candidates.is_empty() {
            info!("no candidate pairs discovered");
            return Ok(());
        }

        let mut analyzed: HashMap<Address, (GemCandidate, TokenRiskReport)> = HashMap::new();
        for candidate in candidates {
            if self
                .config
                .strategy
                .blacklisted_tokens
                .contains(&candidate.token_address)
            {
                continue;
            }

            let risk_report = self
                .risk
                .evaluate_candidate(&candidate)
                .await
                .context("risk evaluation")?;

            if !risk_report.is_safe {
                info!(
                    token = ?candidate.token_address,
                    score = risk_report.score,
                    reason = ?risk_report.flags,
                    "rejected candidate due to risk"
                );
                continue;
            }

            analyzed.insert(candidate.token_address, (candidate, risk_report));
        }

        for (token, (candidate, report)) in &analyzed {
            if portfolio.is_holding(token) {
                continue;
            }

            if !self
                .scanner
                .has_momentum(candidate, &self.config)
                .await
                .context("momentum check")?
            {
                info!(token = ?token, "insufficient momentum, skipping");
                continue;
            }

            let size = self.config.position_size_wei()?;
            let entry_base_price = self
                .trader
                .fetch_base_usd_price(candidate.base_token)
                .await
                .context("fetch base usd price")?;
            let base_decimals = self
                .trader
                .token_decimals(candidate.base_token)
                .await
                .unwrap_or(18);
            let execution = self
                .trader
                .execute_entry(token, size, candidate)
                .await
                .context("executing entry trade")?;

            portfolio.add_position(Position::from_execution(
                candidate,
                execution,
                report.score,
                self.config.strategy.take_profit_bps,
                self.config.strategy.stop_loss_bps,
                entry_base_price,
                base_decimals,
            ));
        }

        if !portfolio.positions().is_empty() {
            info!("evaluating exit conditions");
            let exits = portfolio
                .generate_exit_orders(&self.trader, &self.config)
                .await?;

            for exit in exits {
                if let Err(err) = self
                    .trader
                    .execute_exit(&exit)
                    .await
                    .context("exit execution")
                {
                    error!(position = ?exit.position_id, error = ?err, "exit failed");
                } else {
                    portfolio.close_position(exit.position_id)?;
                }
            }
        }

        portfolio.persist()?;
        Ok(())
    }

    pub async fn health_check(&self) -> Result<String> {
        let provider = self.trader.provider();
        let latest_block = provider
            .get_block_number()
            .await
            .context("fetching latest block")?;
        Ok(format!("ok:{latest_block}"))
    }

    pub async fn portfolio_snapshot(&self) -> PortfolioSnapshot {
        self.portfolio.read().await.snapshot()
    }
}
