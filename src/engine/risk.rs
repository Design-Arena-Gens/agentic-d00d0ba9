use std::collections::BTreeMap;

use anyhow::{Context, Result, anyhow};
use ethers::types::Address;
use reqwest::Client;
use serde::Deserialize;
use time::OffsetDateTime;

use crate::config::{BotConfig, RiskHeuristicsConfig};

use super::scanner::GemCandidate;

#[derive(Debug, Clone)]
pub struct TokenRiskReport {
    pub score: f64,
    pub is_safe: bool,
    pub flags: Vec<String>,
    pub raw_security: Option<GoPlusTokenSecurity>,
    pub evaluated_at: OffsetDateTime,
}

pub struct RiskAnalyzer {
    config: BotConfig,
    client: Client,
}

impl RiskAnalyzer {
    pub fn new(config: BotConfig) -> Self {
        Self {
            config,
            client: Client::builder()
                .user_agent("agentic-memecoin-bot/1.0")
                .build()
                .expect("reqwest client build"),
        }
    }

    pub async fn evaluate_candidate(&self, candidate: &GemCandidate) -> Result<TokenRiskReport> {
        let security = self
            .fetch_security_report(candidate.token_address)
            .await
            .context("fetch security report")?;

        let mut score = 0.0;
        let mut flags = Vec::new();

        if candidate.liquidity_usd >= self.config.strategy.min_liquidity_usd {
            score += 1.0;
        } else {
            flags.push("liquidity-below-threshold".into());
        }

        if candidate.volume24h_usd >= self.config.strategy.min_daily_volume_usd {
            score += 0.8;
        } else {
            flags.push("volume-24h-low".into());
        }

        if candidate.locked_liquidity_ratio.unwrap_or(0.0) >= self.config.risk.min_lock_ratio {
            score += 1.2;
        } else {
            flags.push("insufficient-liquidity-lock".into());
        }

        if candidate
            .holder_count
            .unwrap_or(self.config.risk.min_holder_count)
            >= self.config.risk.min_holder_count
        {
            score += 0.7;
        } else {
            flags.push("holder-count-low".into());
        }

        let (security_score, mut security_flags) =
            evaluate_security_policy(&security, candidate, &self.config.risk)?;

        score += security_score;
        flags.append(&mut security_flags);

        let is_safe = score >= 2.8 && !flags.iter().any(|flag| flag.starts_with("critical"));

        Ok(TokenRiskReport {
            score,
            is_safe,
            flags,
            raw_security: security,
            evaluated_at: OffsetDateTime::now_utc(),
        })
    }

    async fn fetch_security_report(
        &self,
        token_address: Address,
    ) -> Result<Option<GoPlusTokenSecurity>> {
        let chain_id = self.config.chain as u64;
        let url = format!(
            "https://api.gopluslabs.io/api/v1/token_security/{chain_id}?contract_addresses={token}",
            chain_id = chain_id,
            token = format!("{token_address:?}")
        );

        let resp: GoPlusResponse = self
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if resp.code != 1 {
            return Err(anyhow!("goplus api error: {}", resp.message));
        }

        Ok(resp
            .result
            .and_then(|mut map| map.remove(&format!("{token_address:?}"))))
    }
}

fn evaluate_security_policy(
    security: &Option<GoPlusTokenSecurity>,
    candidate: &GemCandidate,
    risk: &RiskHeuristicsConfig,
) -> Result<(f64, Vec<String>)> {
    let mut score = 0.0;
    let mut flags = Vec::new();

    let Some(security) = security else {
        flags.push("critical:goplus-missing".into());
        return Ok((score, flags));
    };

    if security.is_honeypot() {
        flags.push("critical:honeypot-detected".into());
    } else {
        score += 1.0;
    }

    if security.trading_disabled() {
        flags.push("critical:trading-disabled".into());
    }

    if security.can_take_back_ownership() {
        flags.push("critical:owner-can-revoke".into());
    } else {
        score += 0.4;
    }

    if security.is_proxy() {
        flags.push("critical:proxy-contract".into());
    } else {
        score += 0.3;
    }

    let total_tax = security
        .buy_tax()
        .unwrap_or_default()
        .max(security.sell_tax().unwrap_or_default());
    if total_tax <= 15.0 {
        score += 0.4;
    } else {
        flags.push(format!("critical:excessive-tax:{total_tax}"));
    }

    if let Some(top_holder) = security.top10_holder_percent() {
        if top_holder <= risk.max_top_holder_percent {
            score += 0.5;
        } else {
            flags.push(format!("critical:top-holders:{top_holder:.2}"));
        }
    }

    if let Some(renounced) = candidate.contract_renounced_score {
        if renounced >= risk.min_renounced_score {
            score += 0.2;
        } else {
            flags.push(format!("renounce-score-low:{renounced}"));
        }
    }

    Ok((score, flags))
}

#[derive(Debug, Deserialize)]
struct GoPlusResponse {
    code: i64,
    message: String,
    #[serde(default)]
    result: Option<BTreeMap<String, GoPlusTokenSecurity>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoPlusTokenSecurity {
    #[serde(default)]
    pub is_honeypot: Option<String>,
    #[serde(default)]
    pub sell_tax: Option<String>,
    #[serde(default)]
    pub buy_tax: Option<String>,
    #[serde(default)]
    pub cannot_sell_all: Option<String>,
    #[serde(default)]
    pub owner_address: Option<String>,
    #[serde(default)]
    pub can_take_back_ownership: Option<String>,
    #[serde(default)]
    pub is_proxy: Option<String>,
    #[serde(default)]
    pub is_open_source: Option<String>,
    #[serde(default)]
    pub hidden_owner: Option<String>,
    #[serde(default)]
    pub is_blacklisted: Option<String>,
    #[serde(default)]
    pub trading_disabled: Option<String>,
    #[serde(default)]
    pub holder_count: Option<String>,
    #[serde(default)]
    pub total_supply: Option<String>,
    #[serde(default)]
    pub lp_holders: Option<Vec<GoPlusLpHolder>>,
    #[serde(default)]
    pub dex: Option<String>,
    #[serde(default)]
    pub creator_address: Option<String>,
    #[serde(rename = "holders", default)]
    pub top_holders: Option<Vec<GoPlusHolder>>,
}

impl GoPlusTokenSecurity {
    fn is_honeypot(&self) -> bool {
        self.is_honeypot
            .as_deref()
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    fn trading_disabled(&self) -> bool {
        self.trading_disabled
            .as_deref()
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    fn can_take_back_ownership(&self) -> bool {
        self.can_take_back_ownership
            .as_deref()
            .map(|v| v == "1")
            .unwrap_or(false)
    }

    fn is_proxy(&self) -> bool {
        self.is_proxy.as_deref().map(|v| v == "1").unwrap_or(false)
    }

    fn sell_tax(&self) -> Option<f64> {
        self.sell_tax.as_ref().and_then(|s| s.parse::<f64>().ok())
    }

    fn buy_tax(&self) -> Option<f64> {
        self.buy_tax.as_ref().and_then(|s| s.parse::<f64>().ok())
    }

    fn top10_holder_percent(&self) -> Option<f64> {
        self.top_holders.as_ref().and_then(|holders| {
            let sum: f64 = holders
                .iter()
                .take(10)
                .filter_map(|holder| holder.percent.parse::<f64>().ok())
                .sum();
            if sum > 0.0 { Some(sum) } else { None }
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoPlusHolder {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub amount: String,
    #[serde(default)]
    pub percent: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoPlusLpHolder {
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub percent: String,
}
