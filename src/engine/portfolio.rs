use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use ethers::types::{Address, U256};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::config::BotConfig;

use super::{
    scanner::GemCandidate,
    trader::{ExecutionResult, ExitOrder, ExitReason, Trader},
};

const STORAGE_FILE: &str = "portfolio_state.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub id: Uuid,
    pub token: Address,
    pub base_token: Address,
    pub token_symbol: String,
    pub base_spent: U256,
    pub token_amount: U256,
    pub entry_token_price_usd: f64,
    pub entry_base_price_usd: f64,
    pub base_token_decimals: u8,
    pub entry_timestamp: OffsetDateTime,
    pub last_value_usd: f64,
    pub last_updated_at: OffsetDateTime,
    pub risk_score: f64,
    pub take_profit_bps: u32,
    pub stop_loss_bps: u32,
    pub entry_tx: String,
}

impl Position {
    pub fn from_execution(
        candidate: &GemCandidate,
        execution: ExecutionResult,
        risk_score: f64,
        take_profit_bps: u32,
        stop_loss_bps: u32,
        entry_base_price_usd: f64,
        base_token_decimals: u8,
    ) -> Self {
        let entry_token_price_usd = candidate.price_usd;

        let entry_timestamp = execution.timestamp;
        let entry_value_usd =
            format_amount(execution.base_spent, base_token_decimals) * entry_base_price_usd;

        Self {
            id: Uuid::new_v4(),
            token: candidate.token_address,
            base_token: candidate.base_token,
            token_symbol: candidate.token_symbol.clone(),
            base_spent: execution.base_spent,
            token_amount: execution.tokens_acquired,
            entry_token_price_usd,
            entry_base_price_usd,
            base_token_decimals,
            entry_timestamp,
            last_value_usd: entry_value_usd,
            last_updated_at: entry_timestamp,
            risk_score,
            take_profit_bps,
            stop_loss_bps,
            entry_tx: format!("{:?}", execution.tx_hash),
        }
    }

    pub fn entry_value_usd(&self) -> f64 {
        format_amount(self.base_spent, self.base_token_decimals) * self.entry_base_price_usd
    }
}

#[derive(Debug, Default)]
pub struct Portfolio {
    positions: HashMap<Uuid, Position>,
    storage_path: PathBuf,
}

impl Portfolio {
    pub fn load() -> Result<Self> {
        let path = PathBuf::from(STORAGE_FILE);
        if !path.exists() {
            return Ok(Self {
                positions: HashMap::new(),
                storage_path: path,
            });
        }

        let data = fs::read_to_string(&path).context("read portfolio file")?;
        let positions: HashMap<Uuid, Position> =
            serde_json::from_str(&data).context("parse portfolio json")?;

        Ok(Self {
            positions,
            storage_path: path,
        })
    }

    pub fn persist(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.positions)?;
        fs::write(&self.storage_path, json).context("write portfolio file")
    }

    pub fn add_position(&mut self, position: Position) {
        self.positions.insert(position.id, position);
    }

    pub fn positions(&self) -> Vec<&Position> {
        self.positions.values().collect()
    }

    pub fn active_positions(&self) -> Vec<&Position> {
        self.positions.values().collect()
    }

    pub fn is_holding(&self, token: &Address) -> bool {
        self.positions.values().any(|p| &p.token == token)
    }

    pub async fn refresh_positions(&mut self, trader: &Trader) -> Result<()> {
        let mut base_price_cache: HashMap<Address, f64> = HashMap::new();
        let mut base_decimal_cache: HashMap<Address, u8> = HashMap::new();
        for position in self.positions.values_mut() {
            let base_price = if let Some(price) = base_price_cache.get(&position.base_token) {
                *price
            } else {
                let price = trader
                    .fetch_base_usd_price(position.base_token)
                    .await
                    .context("fetch base usd price")?;
                base_price_cache.insert(position.base_token, price);
                price
            };

            let base_decimals = if let Some(decimals) = base_decimal_cache.get(&position.base_token)
            {
                *decimals
            } else {
                let decimals = trader
                    .token_decimals(position.base_token)
                    .await
                    .unwrap_or(position.base_token_decimals);
                base_decimal_cache.insert(position.base_token, decimals);
                decimals
            };

            let base_amount = trader
                .quote_sell(&position.token, position.token_amount, position.base_token)
                .await
                .context("quote current value")?;

            position.last_value_usd = format_amount(base_amount, base_decimals) * base_price;
            position.last_updated_at = OffsetDateTime::now_utc();
        }
        Ok(())
    }

    pub async fn generate_exit_orders(
        &self,
        trader: &Trader,
        config: &BotConfig,
    ) -> Result<Vec<ExitOrder>> {
        let mut orders = vec![];
        let mut base_price_cache: HashMap<Address, f64> = HashMap::new();
        let mut base_decimal_cache: HashMap<Address, u8> = HashMap::new();

        for position in self.positions.values() {
            let base_price = if let Some(price) = base_price_cache.get(&position.base_token) {
                *price
            } else {
                let price = trader.fetch_base_usd_price(position.base_token).await?;
                base_price_cache.insert(position.base_token, price);
                price
            };

            let base_decimals = if let Some(decimals) = base_decimal_cache.get(&position.base_token)
            {
                *decimals
            } else {
                let decimals = trader
                    .token_decimals(position.base_token)
                    .await
                    .unwrap_or(position.base_token_decimals);
                base_decimal_cache.insert(position.base_token, decimals);
                decimals
            };

            let base_amount = trader
                .quote_sell(&position.token, position.token_amount, position.base_token)
                .await?;

            let current_value_usd = format_amount(base_amount, base_decimals) * base_price;
            let entry_value = position.entry_value_usd();

            if entry_value <= 0.0 {
                continue;
            }

            let pnl_bps = ((current_value_usd / entry_value) - 1.0) * 10_000.0;

            let slippage = config.slippage_bps() as u64;
            let min_output = base_amount * U256::from(10_000 - slippage) / U256::from(10_000);

            if pnl_bps >= position.take_profit_bps as f64 {
                orders.push(ExitOrder {
                    position_id: position.id,
                    token_address: position.token,
                    base_token: position.base_token,
                    token_amount: position.token_amount,
                    min_output,
                    reason: ExitReason::TakeProfit,
                });
                continue;
            }

            if pnl_bps <= -(position.stop_loss_bps as f64) {
                orders.push(ExitOrder {
                    position_id: position.id,
                    token_address: position.token,
                    base_token: position.base_token,
                    token_amount: position.token_amount,
                    min_output,
                    reason: ExitReason::StopLoss,
                });
            }
        }

        Ok(orders)
    }

    pub fn close_position(&mut self, id: Uuid) -> Result<()> {
        self.positions
            .remove(&id)
            .context("position not found for closing")?;
        Ok(())
    }

    pub fn snapshot(&self) -> PortfolioSnapshot {
        let mut total_value_usd = 0.0;
        let mut positions = Vec::with_capacity(self.positions.len());
        for position in self.positions.values() {
            total_value_usd += position.last_value_usd;
            let entry_value = position.entry_value_usd();
            let pnl_bps = if entry_value > 0.0 {
                ((position.last_value_usd / entry_value) - 1.0) * 10_000.0
            } else {
                0.0
            };

            positions.push(PositionSnapshot {
                id: position.id,
                token: position.token,
                base_token: position.base_token,
                token_symbol: position.token_symbol.clone(),
                entry_value_usd: entry_value,
                current_value_usd: position.last_value_usd,
                pnl_bps,
                risk_score: position.risk_score,
                entry_timestamp: position.entry_timestamp,
                last_updated_at: position.last_updated_at,
                entry_tx: position.entry_tx.clone(),
            });
        }

        PortfolioSnapshot {
            total_positions: positions.len(),
            total_value_usd,
            positions,
        }
    }
}

fn format_amount(amount: U256, decimals: u8) -> f64 {
    ethers::utils::format_units(amount, decimals as u32)
        .unwrap_or_else(|_| "0".to_string())
        .parse::<f64>()
        .unwrap_or(0.0)
}

#[derive(Debug, Serialize)]
pub struct PortfolioSnapshot {
    pub total_positions: usize,
    pub total_value_usd: f64,
    pub positions: Vec<PositionSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct PositionSnapshot {
    pub id: Uuid,
    pub token: Address,
    pub base_token: Address,
    pub token_symbol: String,
    pub entry_value_usd: f64,
    pub current_value_usd: f64,
    pub pnl_bps: f64,
    pub risk_score: f64,
    pub entry_timestamp: OffsetDateTime,
    pub last_updated_at: OffsetDateTime,
    pub entry_tx: String,
}
