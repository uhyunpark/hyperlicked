use std::net::IpAddr;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, Url};
use serde::{de::DeserializeOwned, Serialize};

use super::config::ServiceConfig;
use super::wire::{
    AccountSnapshot, AssetContext, DepositRequest, DepositResponse, GenesisBlock, NonceResponse,
    OpenOrder, Orderbook, SignedRequest, SubmitResponse,
};

#[derive(Clone)]
pub(crate) struct NodeClient {
    client: Client,
    base_url: String,
}

impl NodeClient {
    pub(crate) fn new(config: &ServiceConfig) -> Result<Self> {
        let url = validate_loopback_url(&config.node_url)?;
        let client = Client::builder()
            .timeout(config.request_timeout())
            .build()
            .context("failed to create market-maker HTTP client")?;
        Ok(Self {
            client,
            base_url: url.as_str().trim_end_matches('/').to_string(),
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .with_context(|| format!("GET {path} failed"))?;
        decode_response(response, path).await
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let response = self
            .client
            .post(format!("{}{path}", self.base_url))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path} failed"))?;
        decode_response(response, path).await
    }

    pub(crate) async fn discover_chain_domain(&self) -> Result<[u8; 32]> {
        let block: GenesisBlock = self.get_json("/api/v1/sync/block/0").await?;
        parse_hash(&block.genesis_hash).context("node returned an invalid genesisHash")
    }

    pub(crate) async fn context(&self, symbol: &str) -> Result<AssetContext> {
        self.get_json(&format!("/api/v1/markets/{symbol}/ctx"))
            .await
    }

    pub(crate) async fn orderbook(&self, symbol: &str) -> Result<Orderbook> {
        self.get_json(&format!("/api/v1/markets/{symbol}/orderbook"))
            .await
    }

    pub(crate) async fn account(&self, address: &str) -> Result<AccountSnapshot> {
        self.get_json(&format!("/api/v1/accounts/{address}")).await
    }

    pub(crate) async fn nonce(&self, address: &str) -> Result<u64> {
        let response: NonceResponse = self
            .get_json(&format!("/api/v1/accounts/{address}/nonce"))
            .await?;
        response
            .nonce
            .parse()
            .context("node returned an invalid account nonce")
    }

    pub(crate) async fn orders(&self, address: &str) -> Result<Vec<OpenOrder>> {
        self.get_json(&format!("/api/v1/accounts/{address}/orders"))
            .await
    }

    pub(crate) async fn deposit(&self, address: &str, amount: i64) -> Result<()> {
        let response: DepositResponse = self
            .post_json(
                "/api/v1/deposit",
                &DepositRequest {
                    trader: address,
                    amount,
                },
            )
            .await?;
        if response.success {
            Ok(())
        } else {
            bail!(
                "dev faucet rejected deposit: {}",
                response
                    .error
                    .unwrap_or_else(|| "unknown error".to_string())
            );
        }
    }

    pub(crate) async fn submit_signed(&self, request: &SignedRequest) -> Result<String> {
        let path = "/api/v1/orders";
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .json(request)
            .send()
            .await
            .with_context(|| format!("POST {path} failed"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            if status == reqwest::StatusCode::BAD_REQUEST && is_duplicate_admission(&body) {
                return Ok(request.expected_hash.clone());
            }
            bail!("HTTP {} for {}: {}", status, path, body.trim());
        }
        let response: SubmitResponse =
            serde_json::from_str(&body).context("invalid JSON response from /api/v1/orders")?;
        if response.status != "pending" {
            bail!("node did not admit transaction: {}", response.status);
        }
        let hash = response.tx_hash.trim_start_matches("0x");
        let bytes = hex::decode(hash).context("node returned an invalid transaction hash")?;
        if bytes.len() != 32 || hash != hash.to_ascii_lowercase() {
            bail!("node returned an invalid transaction hash");
        }
        if hash != request.expected_hash {
            bail!("node returned a transaction hash that does not match the signed envelope");
        }
        Ok(hash.to_string())
    }

    pub(crate) async fn receipt(&self, hash: &str) -> reqwest::Result<reqwest::Response> {
        self.client
            .get(format!("{}/api/v1/transactions/{hash}", self.base_url))
            .send()
            .await
    }
}

fn is_duplicate_admission(body: &str) -> bool {
    body.contains("transaction already exists")
        || body.contains("duplicate signer nonce already pending")
}

pub(crate) async fn decode_response<T: DeserializeOwned>(
    response: reqwest::Response,
    path: &str,
) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("HTTP {} for {}: {}", status, path, body.trim());
    }
    response
        .json()
        .await
        .with_context(|| format!("invalid JSON response from {path}"))
}

pub(crate) fn validate_loopback_url(input: &str) -> Result<Url> {
    let url = Url::parse(input).context("node URL is not a valid URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("node URL must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("node URL must not contain credentials");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("node URL must not contain a query or fragment");
    }
    if !matches!(url.path(), "" | "/") {
        bail!("node URL must contain only the origin");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("node URL must contain a host"))?;
    let host_for_parse = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host_for_parse
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false);
    if !loopback {
        bail!("hl-mm is dev-only and accepts loopback node URLs only");
    }
    Ok(url)
}

fn parse_hash(value: &str) -> Result<[u8; 32]> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    let bytes = hex::decode(value)?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| anyhow!("expected 32-byte hash, got {} bytes", bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_node_urls_are_accepted() {
        assert!(validate_loopback_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_loopback_url("http://[::1]:8080/").is_ok());
        assert!(validate_loopback_url("https://localhost:8080").is_ok());
        assert!(validate_loopback_url("http://example.com:8080").is_err());
        assert!(validate_loopback_url("http://127.0.0.1:8080/api").is_err());
        assert!(validate_loopback_url("http://user@127.0.0.1:8080").is_err());
    }

    #[test]
    fn duplicate_admission_messages_are_recognized_for_receipt_recovery() {
        assert!(is_duplicate_admission("transaction already exists"));
        assert!(!is_duplicate_admission(
            "signer nonce already exists in mempool"
        ));
        assert!(is_duplicate_admission(
            "invalid transaction envelope: duplicate signer nonce already pending"
        ));
        assert!(!is_duplicate_admission("invalid signature"));
    }
}
