use std::{collections::BTreeMap, net::SocketAddr, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow};
use ethers::types::{Address, Chain, U256};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RpcConfig {
    pub http_url: String,
    #[serde(default)]
    pub ws_url: Option<String>,
    #[serde(default = "RpcConfig::default_poll_interval_ms")]
    pub poll_interval_ms: u64,
}

impl RpcConfig {
    const fn default_poll_interval_ms() -> u64 {
        2000
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeConfig {
    pub router_address: Address,
    #[serde(default = "ExchangeConfig::default_slippage_bps")]
    pub max_slippage_bps: u16,
    #[serde(default = "ExchangeConfig::default_deadline_secs")]
    pub deadline_secs: u64,
    #[serde(default = "ExchangeConfig::default_max_gas_gwei")]
    pub max_gas_price_gwei: u64,
    #[serde(default)]
    pub base_tokens: Vec<Address>,
}

impl ExchangeConfig {
    const fn default_slippage_bps() -> u16 {
        300
    }

    const fn default_deadline_secs() -> u64 {
        120
    }

    const fn default_max_gas_gwei() -> u64 {
        200
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StrategyConfig {
    pub max_positions: usize,
    pub position_size_eth: f64,
    #[serde(default)]
    pub blacklisted_tokens: Vec<Address>,
    #[serde(default = "StrategyConfig::default_reward_take_profit_bps")]
    pub take_profit_bps: u32,
    #[serde(default = "StrategyConfig::default_stop_loss_bps")]
    pub stop_loss_bps: u32,
    #[serde(default = "StrategyConfig::default_price_momentum_window_minutes")]
    pub price_momentum_window_minutes: u64,
    #[serde(default = "StrategyConfig::default_min_liquidity_usd")]
    pub min_liquidity_usd: f64,
    #[serde(default = "StrategyConfig::default_min_daily_volume_usd")]
    pub min_daily_volume_usd: f64,
    #[serde(default = "StrategyConfig::default_min_age_minutes")]
    pub min_age_minutes: u64,
}

impl StrategyConfig {
    const fn default_reward_take_profit_bps() -> u32 {
        2500
    }

    const fn default_stop_loss_bps() -> u32 {
        1200
    }

    const fn default_price_momentum_window_minutes() -> u64 {
        15
    }

    const fn default_min_liquidity_usd() -> f64 {
        120_000.0
    }

    const fn default_min_daily_volume_usd() -> f64 {
        250_000.0
    }

    const fn default_min_age_minutes() -> u64 {
        45
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RiskHeuristicsConfig {
    #[serde(default = "RiskHeuristicsConfig::default_max_top_holder_percent")]
    pub max_top_holder_percent: f64,
    #[serde(default = "RiskHeuristicsConfig::default_min_lock_ratio")]
    pub min_lock_ratio: f64,
    #[serde(default = "RiskHeuristicsConfig::default_min_holder_count")]
    pub min_holder_count: u64,
    #[serde(default = "RiskHeuristicsConfig::default_min_renounced_score")]
    pub min_renounced_score: f64,
}

impl RiskHeuristicsConfig {
    const fn default_max_top_holder_percent() -> f64 {
        18.0
    }

    const fn default_min_lock_ratio() -> f64 {
        60.0
    }

    const fn default_min_holder_count() -> u64 {
        500
    }

    const fn default_min_renounced_score() -> f64 {
        0.5
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct AlertingConfig {
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub email_recipients: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MonitoringConfig {
    #[serde(default = "MonitoringConfig::default_bind_addr")]
    pub bind_addr: SocketAddr,
}

impl MonitoringConfig {
    fn default_bind_addr() -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], 8787))
    }
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            bind_addr: Self::default_bind_addr(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct BotConfig {
    pub chain: Chain,
    pub rpc: RpcConfig,
    pub strategy: StrategyConfig,
    pub exchange: ExchangeConfig,
    pub risk: RiskHeuristicsConfig,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub alerting: AlertingConfig,
    #[serde(default)]
    pub monitoring: MonitoringConfig,
}

impl BotConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        let chain_id = std::env::var("CHAIN_ID")
            .context("CHAIN_ID env var missing")?
            .parse::<u64>()
            .context("invalid CHAIN_ID")?;

        let chain =
            Chain::try_from(chain_id).map_err(|_| anyhow!("unsupported chain_id {chain_id}"))?;

        let rpc = RpcConfig {
            http_url: std::env::var("RPC_HTTP").context("RPC_HTTP env var missing")?,
            ws_url: std::env::var("RPC_WS").ok(),
            poll_interval_ms: std::env::var("RPC_POLL_INTERVAL_MS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or_else(RpcConfig::default_poll_interval_ms),
        };

        let router_address = std::env::var("ROUTER_ADDRESS")
            .context("ROUTER_ADDRESS env var missing")
            .and_then(|addr| Address::from_str(&addr).context("invalid ROUTER_ADDRESS"))?;

        let base_tokens = std::env::var("BASE_TOKENS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|addr| {
                let trimmed = addr.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Address::from_str(trimmed).ok()
                }
            })
            .collect::<Vec<_>>();

        let exchange = ExchangeConfig {
            router_address,
            max_slippage_bps: std::env::var("MAX_SLIPPAGE_BPS")
                .ok()
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or_else(ExchangeConfig::default_slippage_bps),
            deadline_secs: std::env::var("SWAP_DEADLINE_SECS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(ExchangeConfig::default_deadline_secs),
            max_gas_price_gwei: std::env::var("MAX_GAS_PRICE_GWEI")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(ExchangeConfig::default_max_gas_gwei),
            base_tokens,
        };

        let strategy = StrategyConfig {
            max_positions: std::env::var("MAX_POSITIONS")
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(4),
            position_size_eth: std::env::var("POSITION_SIZE_ETH")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.3),
            blacklisted_tokens: std::env::var("BLACKLISTED_TOKENS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|addr| {
                    let trimmed = addr.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Address::from_str(trimmed).ok()
                    }
                })
                .collect::<Vec<_>>(),
            take_profit_bps: std::env::var("TAKE_PROFIT_BPS")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or_else(StrategyConfig::default_reward_take_profit_bps),
            stop_loss_bps: std::env::var("STOP_LOSS_BPS")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or_else(StrategyConfig::default_stop_loss_bps),
            price_momentum_window_minutes: std::env::var("MOMENTUM_WINDOW_MINUTES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(StrategyConfig::default_price_momentum_window_minutes),
            min_liquidity_usd: std::env::var("MIN_LIQUIDITY_USD")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(StrategyConfig::default_min_liquidity_usd),
            min_daily_volume_usd: std::env::var("MIN_DAILY_VOLUME_USD")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(StrategyConfig::default_min_daily_volume_usd),
            min_age_minutes: std::env::var("MIN_TOKEN_AGE_MINUTES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(StrategyConfig::default_min_age_minutes),
        };

        let risk = RiskHeuristicsConfig {
            max_top_holder_percent: std::env::var("MAX_TOP_HOLDER_PERCENT")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(RiskHeuristicsConfig::default_max_top_holder_percent),
            min_lock_ratio: std::env::var("MIN_LOCK_RATIO_PERCENT")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(RiskHeuristicsConfig::default_min_lock_ratio),
            min_holder_count: std::env::var("MIN_HOLDER_COUNT")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(RiskHeuristicsConfig::default_min_holder_count),
            min_renounced_score: std::env::var("MIN_RENOUNCED_SCORE")
                .ok()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_else(RiskHeuristicsConfig::default_min_renounced_score),
        };

        let metadata = std::env::var("BOT_TAGS")
            .unwrap_or_default()
            .split(',')
            .filter_map(|kv| {
                let (k, v) = kv.trim().split_once('=')?;
                Some((k.trim().to_string(), v.trim().to_string()))
            })
            .collect::<BTreeMap<_, _>>();

        let alerting = AlertingConfig {
            webhook_url: std::env::var("ALERT_WEBHOOK").ok(),
            email_recipients: std::env::var("ALERT_EMAILS")
                .unwrap_or_default()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        };

        let monitoring = MonitoringConfig {
            bind_addr: std::env::var("MONITOR_ADDR")
                .ok()
                .and_then(|addr| addr.parse::<SocketAddr>().ok())
                .unwrap_or_else(MonitoringConfig::default_bind_addr),
        };

        Ok(Self {
            chain,
            rpc,
            strategy,
            exchange,
            risk,
            metadata,
            alerting,
            monitoring,
        })
    }

    pub fn position_size_wei(&self) -> Result<U256> {
        let wei_per_eth = U256::exp10(18);
        let scaled = wei_per_eth
            .checked_mul(U256::from(
                (self.strategy.position_size_eth * 1e6_f64) as u64,
            ))
            .ok_or_else(|| anyhow!("position size overflow"))?
            / U256::from(1_000_000u64);
        Ok(scaled)
    }

    pub fn slippage_bps(&self) -> u16 {
        self.exchange.max_slippage_bps
    }

    pub fn swap_deadline(&self) -> Duration {
        Duration::from_secs(self.exchange.deadline_secs)
    }

    pub fn chain_as_str(&self) -> &'static str {
        match self.chain {
            Chain::Mainnet => "ethereum",
            Chain::Arbitrum => "arbitrum",
            Chain::Base => "base",
            Chain::Polygon => "polygon",
            Chain::Optimism => "optimism",
            Chain::BinanceSmartChain => "bsc",
            _ => "ethereum",
        }
    }
}
