//! HTTP client for the five signer nodes.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures::future::join_all;
use serde::{Serialize, de::DeserializeOwned};
use zecured_node::config::NetworkConfig;

/// One node's answer to a broadcast request.
pub type Answer<T> = (u16, Result<T>);

#[derive(Clone)]
pub struct Nodes {
    http: reqwest::Client,
    nodes: Vec<(u16, String)>,
}

impl Nodes {
    pub fn new(cfg: &NetworkConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("HTTP client")?;
        Ok(Self {
            http,
            nodes: cfg.nodes.iter().map(|n| (n.index, n.url.trim_end_matches('/').to_owned())).collect(),
        })
    }

    pub fn indices(&self) -> Vec<u16> {
        self.nodes.iter().map(|(i, _)| *i).collect()
    }

    /// POSTs `body` to `path` on node `index`.
    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, index: u16, path: &str, body: &B) -> Result<T> {
        let (_, base) = self
            .nodes
            .iter()
            .find(|(i, _)| *i == index)
            .with_context(|| format!("unknown node {index}"))?;
        let resp = self
            .http
            .post(format!("{base}{path}"))
            .json(body)
            .send()
            .await
            .with_context(|| format!("node {index} unreachable"))?;
        let status = resp.status();
        let value: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        if !status.is_success() {
            let msg = value.get("error").and_then(|e| e.as_str()).unwrap_or("no reason given");
            return Err(anyhow!("node {index} refused ({status}): {msg}"));
        }
        serde_json::from_value(value).with_context(|| format!("node {index}: unexpected response"))
    }

    /// GETs `path` on node `index`.
    pub async fn get_json(&self, index: u16, path: &str) -> Result<serde_json::Value> {
        let (_, base) = self
            .nodes
            .iter()
            .find(|(i, _)| *i == index)
            .with_context(|| format!("unknown node {index}"))?;
        Ok(self
            .http
            .get(format!("{base}{path}"))
            .timeout(Duration::from_secs(3))
            .send()
            .await?
            .json()
            .await?)
    }

    /// POSTs the same body to `indices` concurrently.
    pub async fn post_many<B: Serialize + Sync, T: DeserializeOwned>(&self, indices: &[u16], path: &str, body: &B) -> Vec<Answer<T>> {
        join_all(indices.iter().map(|&i| async move { (i, self.post(i, path, body).await) })).await
    }

    /// POSTs a per-node body to each listed node concurrently.
    pub async fn post_each<B: Serialize + Sync, T: DeserializeOwned>(&self, bodies: &[(u16, B)], path: &str) -> Vec<Answer<T>> {
        join_all(bodies.iter().map(|(i, b)| async move { (*i, self.post(*i, path, b).await) })).await
    }
}
