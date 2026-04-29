//! /api/sessions — hub-backed gateway conversation session client.

use serde_json::Value;

use crate::{Client, Error, Result};

pub struct SessionsApi<'a> {
    pub(crate) client: &'a Client,
}

impl<'a> SessionsApi<'a> {
    /// Load messages for a session key. Returns empty vec if session doesn't exist.
    pub async fn get(&self, key: &str) -> Result<Vec<Value>> {
        let url = self
            .client
            .url(&format!("/api/sessions/{}", urlencoding(key)));
        let resp = self
            .client
            .http()
            .get(&url)
            .send()
            .await
            .map_err(Error::Http)?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map_err(Error::Http)?;
            return Err(Error::from_response(status, &bytes));
        }
        let body: serde_json::Value = resp.json().await.map_err(Error::Http)?;
        Ok(body["messages"].as_array().cloned().unwrap_or_default())
    }

    /// Save messages for a session key.
    pub async fn put(
        &self,
        key: &str,
        agent: &str,
        workspace: &str,
        messages: &[Value],
    ) -> Result<()> {
        let url = self
            .client
            .url(&format!("/api/sessions/{}", urlencoding(key)));
        let body = serde_json::json!({
            "agent": agent,
            "workspace": workspace,
            "messages": messages,
        });
        let resp = self
            .client
            .http()
            .put(&url)
            .json(&body)
            .send()
            .await
            .map_err(Error::Http)?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map_err(Error::Http)?;
            return Err(Error::from_response(status, &bytes));
        }
        Ok(())
    }

    /// Delete a session.
    pub async fn delete(&self, key: &str) -> Result<()> {
        let url = self
            .client
            .url(&format!("/api/sessions/{}", urlencoding(key)));
        let resp = self
            .client
            .http()
            .delete(&url)
            .send()
            .await
            .map_err(Error::Http)?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await.map_err(Error::Http)?;
            return Err(Error::from_response(status, &bytes));
        }
        Ok(())
    }
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '/' => vec!['%', '2', 'F'],
            ':' => vec!['%', '3', 'A'],
            ' ' => vec!['%', '2', '0'],
            c => vec![c],
        })
        .collect()
}
