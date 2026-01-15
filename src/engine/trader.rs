use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use ethers::{
    contract::builders::ContractCall,
    middleware::SignerMiddleware,
    prelude::*,
    providers::{Http, Provider},
    utils::parse_units,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio::time::timeout;
use tracing::{info, instrument};

use crate::config::BotConfig;

use super::scanner::GemCandidate;

abigen!(
    UniswapV2Router,
    r#"[
        {"inputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"address[]","name":"path","type":"address[]"}],"name":"getAmountsOut","outputs":[{"internalType":"uint256[]","name":"","type":"uint256[]"}],"stateMutability":"view","type":"function"},
        {"inputs":[{"internalType":"uint256","name":"amountOutMin","type":"uint256"},{"internalType":"address[]","name":"path","type":"address[]"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"name":"swapExactETHForTokensSupportingFeeOnTransferTokens","outputs":[],"stateMutability":"payable","type":"function"},
        {"inputs":[{"internalType":"uint256","name":"amountIn","type":"uint256"},{"internalType":"uint256","name":"amountOutMin","type":"uint256"},{"internalType":"address[]","name":"path","type":"address[]"},{"internalType":"address","name":"to","type":"address"},{"internalType":"uint256","name":"deadline","type":"uint256"}],"name":"swapExactTokensForETHSupportingFeeOnTransferTokens","outputs":[],"stateMutability":"nonpayable","type":"function"}
    ]"#,
    event_derives(serde::Deserialize, serde::Serialize)
);

abigen!(
    Erc20,
    r#"[
        {"inputs":[],"name":"decimals","outputs":[{"internalType":"uint8","name":"","type":"uint8"}],"stateMutability":"view","type":"function"},
        {"inputs":[{"internalType":"address","name":"","type":"address"}],"name":"balanceOf","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},
        {"inputs":[{"internalType":"address","name":"owner","type":"address"},{"internalType":"address","name":"spender","type":"address"}],"name":"allowance","outputs":[{"internalType":"uint256","name":"","type":"uint256"}],"stateMutability":"view","type":"function"},
        {"inputs":[{"internalType":"address","name":"spender","type":"address"},{"internalType":"uint256","name":"amount","type":"uint256"}],"name":"approve","outputs":[{"internalType":"bool","name":"","type":"bool"}],"stateMutability":"nonpayable","type":"function"}
    ]"#,
    event_derives(serde::Deserialize, serde::Serialize)
);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub tx_hash: TxHash,
    pub token_address: Address,
    pub base_token: Address,
    pub base_spent: U256,
    pub tokens_acquired: U256,
    pub block_number: U64,
    pub timestamp: OffsetDateTime,
}

type SigningMiddleware = SignerMiddleware<Arc<Provider<Http>>, Wallet<k256::ecdsa::SigningKey>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExitReason {
    TakeProfit,
    StopLoss,
    RiskAlert,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitOrder {
    pub position_id: uuid::Uuid,
    pub token_address: Address,
    pub base_token: Address,
    pub token_amount: U256,
    pub min_output: U256,
    pub reason: ExitReason,
}

pub struct Trader {
    config: BotConfig,
    provider: Arc<Provider<Http>>,
    client: Arc<SigningMiddleware>,
    router: UniswapV2Router<SigningMiddleware>,
    wallet_address: Address,
    http: Client,
}

impl Trader {
    pub async fn new(config: BotConfig) -> Result<Self> {
        let http_provider = Provider::<Http>::try_from(config.rpc.http_url.clone())
            .context("initializing HTTP provider")?
            .interval(Duration::from_millis(config.rpc.poll_interval_ms));

        let provider = Arc::new(http_provider);
        let key =
            std::env::var("TRADING_PRIVATE_KEY").context("TRADING_PRIVATE_KEY env var missing")?;
        let wallet: LocalWallet = key
            .parse::<LocalWallet>()
            .context("invalid private key")?
            .with_chain_id(config.chain as u64);

        let client = Arc::new(SignerMiddleware::new(provider.clone(), wallet));

        let router = UniswapV2Router::new(config.exchange.router_address, client.clone());
        let wallet_address = client.address();
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("agentic-memecoin-bot/1.0")
            .build()
            .expect("reqwest client build");

        Ok(Self {
            config,
            provider,
            client,
            router,
            wallet_address,
            http,
        })
    }

    pub fn provider(&self) -> Arc<Provider<Http>> {
        self.provider.clone()
    }

    #[instrument(skip(self, candidate))]
    pub async fn execute_entry(
        &self,
        token: &Address,
        amount_in: U256,
        candidate: &GemCandidate,
    ) -> Result<ExecutionResult> {
        let path = vec![candidate.base_token, *token];
        let expected_tokens = self
            .quote_buy(token, amount_in, candidate.base_token)
            .await?;
        let min_out = expected_tokens
            .checked_mul(U256::from(10_000u64 - self.config.slippage_bps() as u64))
            .ok_or_else(|| anyhow!("slippage multiplication overflow"))?
            / U256::from(10_000u64);

        let recipient = self.wallet_address;
        let deadline = U256::from(
            (OffsetDateTime::now_utc() + self.config.swap_deadline()).unix_timestamp() as u64,
        );

        let balance_before = self.token_balance(token).await?;

        let mut call: ContractCall<_, ()> = self
            .router
            .method(
                "swapExactETHForTokensSupportingFeeOnTransferTokens",
                (min_out, path.clone(), recipient, deadline),
            )
            .context("prepare swapExactETHForTokens call")?;
        call = call.value(amount_in).gas_price(parse_units(
            self.config.exchange.max_gas_price_gwei,
            "gwei",
        )?);

        let pending_tx = call.send().await.context("submit swap tx")?;

        let receipt = timeout(
            self.config.swap_deadline() + Duration::from_secs(30),
            pending_tx,
        )
        .await
        .context("swap tx timeout")?
        .context("swap execution reverted")?
        .ok_or_else(|| anyhow!("swap transaction dropped without receipt"))?;

        let block_number = receipt
            .block_number
            .context("missing block number in receipt")?;

        let balance_after = self.token_balance(token).await?;
        let tokens_acquired = balance_after
            .checked_sub(balance_before)
            .ok_or_else(|| anyhow!("token balance decreased unexpectedly"))?;

        let timestamp = self
            .provider
            .get_block(block_number)
            .await?
            .map(|b| b.timestamp)
            .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts.as_u64() as i64).ok())
            .unwrap_or_else(OffsetDateTime::now_utc);

        let execution = ExecutionResult {
            tx_hash: receipt.transaction_hash,
            token_address: *token,
            base_token: candidate.base_token,
            base_spent: amount_in,
            tokens_acquired,
            block_number,
            timestamp,
        };

        info!(
            token = ?token,
            tx = ?execution.tx_hash,
            tokens = %execution.tokens_acquired,
            "entry execution completed"
        );

        Ok(execution)
    }

    pub async fn execute_exit(&self, exit_order: &ExitOrder) -> Result<ExecutionResult> {
        let path = vec![exit_order.token_address, exit_order.base_token];
        let deadline = U256::from(
            (OffsetDateTime::now_utc() + self.config.swap_deadline()).unix_timestamp() as u64,
        );
        let mut tx: ContractCall<_, ()> = self
            .router
            .method(
                "swapExactTokensForETHSupportingFeeOnTransferTokens",
                (
                    exit_order.token_amount,
                    exit_order.min_output,
                    path.clone(),
                    self.wallet_address,
                    deadline,
                ),
            )
            .context("prepare swapExactTokensForETH call")?;
        tx = tx.gas_price(parse_units(
            self.config.exchange.max_gas_price_gwei,
            "gwei",
        )?);

        self.ensure_allowance(exit_order.token_address, exit_order.token_amount)
            .await?;

        let base_balance_before = self
            .provider
            .get_balance(self.wallet_address, None)
            .await
            .context("fetch native balance before exit")?;

        let pending = tx
            .send()
            .await
            .context("sending exit transaction to router")?;

        let receipt = pending
            .await
            .context("exit tx dropped")?
            .ok_or_else(|| anyhow!("exit transaction dropped without receipt"))?;
        let block_number = receipt.block_number.context("missing exit block number")?;

        let base_balance_after = self
            .provider
            .get_balance(self.wallet_address, None)
            .await
            .context("fetch native balance after exit")?;

        let redeemed = base_balance_after
            .checked_sub(base_balance_before)
            .unwrap_or(exit_order.min_output);

        let timestamp = self
            .provider
            .get_block(block_number)
            .await?
            .map(|b| b.timestamp)
            .and_then(|ts| OffsetDateTime::from_unix_timestamp(ts.as_u64() as i64).ok())
            .unwrap_or_else(OffsetDateTime::now_utc);

        Ok(ExecutionResult {
            tx_hash: receipt.transaction_hash,
            token_address: exit_order.token_address,
            base_token: exit_order.base_token,
            base_spent: redeemed,
            tokens_acquired: exit_order.token_amount,
            block_number,
            timestamp,
        })
    }

    pub async fn quote_buy(
        &self,
        token: &Address,
        amount_in: U256,
        base_token: Address,
    ) -> Result<U256> {
        let path = vec![base_token, *token];
        let result = self.router.get_amounts_out(amount_in, path).call().await?;
        result
            .last()
            .cloned()
            .ok_or_else(|| anyhow!("router getAmountsOut returned empty path"))
    }

    pub async fn quote_sell(
        &self,
        token: &Address,
        amount_in: U256,
        base_token: Address,
    ) -> Result<U256> {
        let path = vec![*token, base_token];
        let result = self.router.get_amounts_out(amount_in, path).call().await?;
        result
            .last()
            .cloned()
            .ok_or_else(|| anyhow!("router getAmountsOut returned empty path"))
    }

    pub async fn token_balance(&self, token: &Address) -> Result<U256> {
        if *token == self.wallet_address {
            return self
                .provider
                .get_balance(self.wallet_address, None)
                .await
                .context("fetching wallet eth balance");
        }

        let erc20 = Erc20::new(*token, self.provider.clone());
        erc20
            .balance_of(self.wallet_address)
            .call()
            .await
            .context("fetching erc20 balance")
    }

    pub async fn token_decimals(&self, token: Address) -> Result<u8> {
        let erc20 = Erc20::new(token, self.provider.clone());
        erc20
            .decimals()
            .call()
            .await
            .context("fetch token decimals")
    }

    async fn ensure_allowance(&self, token: Address, amount: U256) -> Result<()> {
        if token == self.wallet_address {
            return Ok(());
        }
        let erc20 = Erc20::new(token, self.client.clone());
        let allowance = erc20
            .allowance(self.wallet_address, self.config.exchange.router_address)
            .call()
            .await?;
        if allowance >= amount {
            return Ok(());
        }

        let mut approval: ContractCall<_, bool> =
            erc20.approve(self.config.exchange.router_address, U256::max_value());
        approval = approval.gas_price(parse_units(
            self.config.exchange.max_gas_price_gwei,
            "gwei",
        )?);
        let pending = approval.send().await?;
        pending.await?;
        Ok(())
    }

    pub async fn fetch_token_price_usd(
        &self,
        token: &Address,
        base_token: Address,
        usd_price_per_base: f64,
    ) -> Result<f64> {
        let amount_out = self.quote_sell(token, U256::exp10(18), base_token).await?;
        let base_decimals = self.token_decimals(base_token).await.unwrap_or(18);
        let base_float = ethers::utils::format_units(amount_out, u32::from(base_decimals))?;
        Ok(base_float.parse::<f64>()? * usd_price_per_base)
    }

    pub async fn fetch_base_usd_price(&self, base_token: Address) -> Result<f64> {
        let chain = self.config.chain_as_str();
        let key = format!("{chain}:{base_token:?}");
        let url = format!("https://coins.llama.fi/prices/current/{key}");

        let resp: LlamaPriceResponse = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        resp.coins
            .get(&key)
            .map(|price| price.price)
            .ok_or_else(|| anyhow!("missing price data for base token"))
    }
}

#[derive(Debug, Deserialize)]
struct LlamaPriceResponse {
    coins: std::collections::HashMap<String, LlamaPriceEntry>,
}

#[derive(Debug, Deserialize)]
struct LlamaPriceEntry {
    price: f64,
}
