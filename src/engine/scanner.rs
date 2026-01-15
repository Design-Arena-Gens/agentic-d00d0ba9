use std::{cmp::Ordering, str::FromStr, time::Duration};

use anyhow::{Context, Result, anyhow};
use ethers::types::Address;
use reqwest::Client;
use serde::Deserialize;
use time::{Duration as TimeDuration, OffsetDateTime};
use tracing::instrument;

use crate::config::{BotConfig, StrategyConfig};

#[derive(Debug, Clone)]
pub struct GemCandidate {
    pub pair_address: Address,
    pub token_address: Address,
    pub base_token: Address,
    pub token_symbol: String,
    pub token_name: String,
    pub price_usd: f64,
    pub liquidity_usd: f64,
    pub volume24h_usd: f64,
    pub fdv_usd: f64,
    pub price_change_m5: f64,
    pub price_change_m15: f64,
    pub price_change_h1: f64,
    pub buy_pressure_ratio: f64,
    pub holder_count: Option<u64>,
    pub locked_liquidity_ratio: Option<f64>,
    pub contract_renounced_score: Option<f64>,
    pub pair_created_at: OffsetDateTime,
    pub dex_id: String,
    pub confidence: f64,
    pub safety_flags: Vec<String>,
    pub usd_per_base: f64,
}

#[derive(Debug, Default)]
pub struct DexScreenerScanner {
    client: Client,
}

impl DexScreenerScanner {
    pub async fn discover_candidates(&self, config: &BotConfig) -> Result<Vec<GemCandidate>> {
        let chain_key = chain_to_dexscreener_key(config.chain)
            .ok_or_else(|| anyhow!("chain not supported by DexScreener"))?;

        let mut pairs = self
            .fetch_trending_pairs(chain_key)
            .await
            .context("fetch_trending_pairs")?;

        pairs.extend(self.fetch_latest_pairs(chain_key).await.unwrap_or_default());

        let mut candidates = vec![];
        for pair in pairs {
            if let Some(candidate) = self
                .to_candidate(pair, &config.strategy)
                .context("convert pair to candidate")?
            {
                if candidate.liquidity_usd >= config.strategy.min_liquidity_usd
                    && candidate.volume24h_usd >= config.strategy.min_daily_volume_usd
                    && (OffsetDateTime::now_utc() - candidate.pair_created_at)
                        >= TimeDuration::minutes(config.strategy.min_age_minutes as i64)
                {
                    candidates.push(candidate);
                }
            }
        }

        candidates.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(Ordering::Equal)
        });
        candidates.truncate(12);
        Ok(candidates)
    }

    pub async fn fetch_token_candidates(
        &self,
        token: &Address,
        config: &BotConfig,
    ) -> Result<Vec<GemCandidate>> {
        let chain_key = chain_to_dexscreener_key(config.chain)
            .ok_or_else(|| anyhow!("chain not supported by DexScreener"))?;

        let pairs = self
            .fetch_pairs_for_token(token)
            .await
            .context("fetch token pairs")?;

        let mut candidates = Vec::new();
        for pair in pairs {
            if pair.chain_id != chain_key {
                continue;
            }
            if let Some(candidate) = self.to_candidate(pair, &config.strategy)? {
                candidates.push(candidate);
            }
        }
        Ok(candidates)
    }

    #[instrument(skip(self, candidate, config))]
    pub async fn has_momentum(&self, candidate: &GemCandidate, config: &BotConfig) -> Result<bool> {
        let window = config.strategy.price_momentum_window_minutes as f64;
        let score = (candidate.price_change_m5 * 0.4)
            + (candidate.price_change_m15 * 0.35)
            + (candidate.price_change_h1 * 0.25);
        Ok(score >= 8.0 && candidate.buy_pressure_ratio >= 0.55 && window >= 5.0)
    }

    async fn fetch_trending_pairs(&self, chain_key: &str) -> Result<Vec<DexScreenerPair>> {
        let url = format!(
            "https://api.dexscreener.com/latest/dex/trending/{chain}",
            chain = chain_key
        );
        let resp: DexScreenerPairsResponse = self
            .client
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.pairs.unwrap_or_default())
    }

    async fn fetch_latest_pairs(&self, chain_key: &str) -> Result<Vec<DexScreenerPair>> {
        let url = format!(
            "https://api.dexscreener.com/latest/dex/pairs/{chain}",
            chain = chain_key
        );
        let resp: DexScreenerPairsResponse = self
            .client
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.pairs.unwrap_or_default())
    }

    async fn fetch_pairs_for_token(&self, token: &Address) -> Result<Vec<DexScreenerPair>> {
        let url = format!(
            "https://api.dexscreener.com/latest/dex/tokens/{token}",
            token = format!("{token:?}")
        );
        let resp: DexScreenerPairsResponse = self
            .client
            .get(url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.pairs.unwrap_or_default())
    }

    fn to_candidate(
        &self,
        pair: DexScreenerPair,
        strategy: &StrategyConfig,
    ) -> Result<Option<GemCandidate>> {
        let token_address = Address::from_str(&pair.base_token.address)?;
        let base_token = Address::from_str(&pair.quote_token.address)?;
        if strategy
            .blacklisted_tokens
            .iter()
            .any(|addr| addr == &token_address)
        {
            return Ok(None);
        }

        let confidence = compute_confidence_score(&pair);
        let safety_flags = collect_safety_flags(&pair);

        let buy_pressure_ratio = {
            let buys = pair.txns.m5.buys.max(1) as f64;
            let sells = pair.txns.m5.sells.max(1) as f64;
            buys / (buys + sells)
        };

        let pair_created_at = pair
            .pair_created_at
            .and_then(|ms| OffsetDateTime::from_unix_timestamp(ms / 1000).ok())
            .unwrap_or_else(|| OffsetDateTime::now_utc() - TimeDuration::hours(1));

        let price_native = pair.price_native.unwrap_or(0.0);
        let usd_per_base = if price_native > 0.0 && pair.price_usd.unwrap_or(0.0) > 0.0 {
            pair.price_usd.unwrap() / price_native
        } else {
            0.0
        };

        Ok(Some(GemCandidate {
            pair_address: Address::from_str(&pair.pair_address)?,
            token_address,
            base_token,
            token_symbol: pair.base_token.symbol,
            token_name: pair.base_token.name,
            price_usd: pair.price_usd.unwrap_or_default(),
            liquidity_usd: pair.liquidity.usd.unwrap_or_default(),
            volume24h_usd: pair.volume.h24.unwrap_or_default(),
            fdv_usd: pair.fdv.unwrap_or_default(),
            price_change_m5: pair.price_change.m5.unwrap_or_default(),
            price_change_m15: pair.price_change.m15.unwrap_or_default(),
            price_change_h1: pair.price_change.h1.unwrap_or_default(),
            buy_pressure_ratio,
            holder_count: pair.info.as_ref().and_then(|info| info.holders),
            locked_liquidity_ratio: pair.liquidity.locked,
            contract_renounced_score: pair.info.as_ref().and_then(|info| info.renounced),
            pair_created_at,
            dex_id: pair.dex_id,
            confidence,
            safety_flags,
            usd_per_base,
        }))
    }
}

fn compute_confidence_score(pair: &DexScreenerPair) -> f64 {
    let liquidity = pair.liquidity.usd.unwrap_or(0.0).ln_1p();
    let volume = pair.volume.h24.unwrap_or(0.0).ln_1p();
    let change = pair.price_change.h1.unwrap_or(0.0).max(0.0);
    let locks = pair.liquidity.locked.unwrap_or(0.0) / 100.0;
    liquidity * 0.25 + volume * 0.3 + change * 0.3 + locks * 0.15
}

fn collect_safety_flags(pair: &DexScreenerPair) -> Vec<String> {
    let mut flags = vec![];
    if let Some(liq) = pair.liquidity.usd {
        if liq < 60_000.0 {
            flags.push("low-liquidity".into());
        }
    }
    if let Some(renounced) = pair.info.as_ref().and_then(|info| info.renounced) {
        if renounced < 0.4 {
            flags.push("owner-not-renounced".into());
        }
    }
    if let Some(locked) = pair.liquidity.locked {
        if locked < 50.0 {
            flags.push("low-lock".into());
        }
    }
    flags
}

fn chain_to_dexscreener_key(chain: ethers::types::Chain) -> Option<&'static str> {
    match chain {
        ethers::types::Chain::Mainnet => Some("ethereum"),
        ethers::types::Chain::Arbitrum => Some("arbitrum"),
        ethers::types::Chain::Base => Some("base"),
        ethers::types::Chain::Optimism => Some("optimism"),
        ethers::types::Chain::Polygon => Some("polygon"),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct DexScreenerPairsResponse {
    #[serde(default)]
    pairs: Option<Vec<DexScreenerPair>>,
}

#[derive(Debug, Deserialize)]
struct DexScreenerPair {
    #[serde(rename = "chainId")]
    chain_id: String,
    #[serde(rename = "dexId")]
    dex_id: String,
    #[serde(rename = "pairAddress")]
    pair_address: String,
    #[serde(rename = "baseToken")]
    base_token: TokenMetadata,
    #[serde(rename = "quoteToken")]
    quote_token: TokenMetadata,
    #[serde(rename = "priceUsd", default)]
    price_usd: Option<f64>,
    #[serde(rename = "priceNative", default)]
    price_native: Option<f64>,
    #[serde(rename = "priceChange")]
    price_change: PriceChange,
    liquidity: PairLiquidity,
    volume: VolumeMetrics,
    txns: TransactionMetrics,
    #[serde(rename = "pairCreatedAt", default)]
    pair_created_at: Option<i64>,
    #[serde(default)]
    fdv: Option<f64>,
    #[serde(default)]
    info: Option<PairInfo>,
}

#[derive(Debug, Deserialize)]
struct PairInfo {
    #[serde(default)]
    holders: Option<u64>,
    #[serde(default)]
    renounced: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TokenMetadata {
    address: String,
    symbol: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct PriceChange {
    #[serde(default)]
    m5: Option<f64>,
    #[serde(default)]
    m15: Option<f64>,
    #[serde(default)]
    h1: Option<f64>,
    #[serde(default)]
    h6: Option<f64>,
    #[serde(default)]
    h24: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PairLiquidity {
    #[serde(default)]
    usd: Option<f64>,
    #[serde(default)]
    base: Option<f64>,
    #[serde(default)]
    quote: Option<f64>,
    #[serde(default)]
    locked: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct VolumeMetrics {
    #[serde(default)]
    h24: Option<f64>,
    #[serde(default)]
    h6: Option<f64>,
    #[serde(default)]
    h1: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TransactionMetrics {
    #[serde(rename = "m5")]
    m5: TransactionWindow,
    #[serde(rename = "m15")]
    m15: TransactionWindow,
    #[serde(rename = "h1")]
    h1: TransactionWindow,
    #[serde(rename = "h6")]
    h6: TransactionWindow,
    #[serde(rename = "h24")]
    h24: TransactionWindow,
}

#[derive(Debug, Deserialize)]
struct TransactionWindow {
    buys: u64,
    sells: u64,
}
