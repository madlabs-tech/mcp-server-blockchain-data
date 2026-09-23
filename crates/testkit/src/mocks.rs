//! Scripted port implementations for routing and app tests.

use async_trait::async_trait;
use bdm_domain::{AccountAddress, AssetId, Price};
use bdm_ports::{
    EvmRpc, FxRate, FxRates, PortResult, PriceFeed, ProviderError, TokenBalance, TokenBalances,
};
use chrono::NaiveDate;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};

/// Queue of scripted results, consumed one per call. When the queue is empty, the `always`
/// result is returned if set, otherwise `Fatal("script exhausted")`.
pub struct Scripted<T> {
    queue: Mutex<VecDeque<(Duration, PortResult<T>)>>,
    always: Mutex<Option<PortResult<T>>>,
    calls: AtomicUsize,
}

impl<T: Clone> Default for Scripted<T> {
    fn default() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            always: Mutex::new(None),
            calls: AtomicUsize::new(0),
        }
    }
}

impl<T: Clone> Scripted<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_ok(&self, v: T) -> &Self {
        self.push_delayed(Duration::ZERO, Ok(v))
    }

    pub fn push_err(&self, e: ProviderError) -> &Self {
        self.push_delayed(Duration::ZERO, Err(e))
    }

    pub fn push_delayed(&self, delay: Duration, r: PortResult<T>) -> &Self {
        self.queue.lock().unwrap().push_back((delay, r));
        self
    }

    /// Result returned whenever the queue is empty.
    pub fn always(&self, r: PortResult<T>) -> &Self {
        *self.always.lock().unwrap() = Some(r);
        self
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub async fn next(&self) -> PortResult<T> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let item = self.queue.lock().unwrap().pop_front();
        match item {
            Some((delay, r)) => {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                r
            }
            None => self
                .always
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Err(ProviderError::Fatal("script exhausted".into()))),
        }
    }
}

/// Scripted `EvmRpc`; every `request` pops the next result regardless of method.
#[derive(Default)]
pub struct MockEvmRpc {
    pub chain_id: u64,
    pub script: Scripted<Value>,
}

#[async_trait]
impl EvmRpc for MockEvmRpc {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }
    async fn request(&self, _method: &str, _params: Value) -> PortResult<Value> {
        self.script.next().await
    }
}

#[derive(Default)]
pub struct MockPriceFeed {
    pub script: Scripted<Price>,
}

#[async_trait]
impl PriceFeed for MockPriceFeed {
    async fn price(&self, _asset: &AssetId, _currency: &str) -> PortResult<Price> {
        self.script.next().await
    }
}

#[derive(Default)]
pub struct MockTokenBalances {
    pub script: Scripted<Vec<TokenBalance>>,
}

#[async_trait]
impl TokenBalances for MockTokenBalances {
    async fn balances(
        &self,
        _owner: &AccountAddress,
        _assets: Option<&[AssetId]>,
    ) -> PortResult<Vec<TokenBalance>> {
        self.script.next().await
    }
}

#[derive(Default)]
pub struct MockFxRates {
    pub script: Scripted<FxRate>,
}

#[async_trait]
impl FxRates for MockFxRates {
    async fn rate(
        &self,
        _base: &str,
        _quote: &str,
        _date: Option<NaiveDate>,
    ) -> PortResult<FxRate> {
        self.script.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn script_queue_then_always_then_exhausted() {
        let s: Scripted<u8> = Scripted::new();
        s.push_ok(1).push_err(ProviderError::NotFound);
        assert_eq!(s.next().await, Ok(1));
        assert_eq!(s.next().await, Err(ProviderError::NotFound));
        assert!(matches!(s.next().await, Err(ProviderError::Fatal(_))));
        s.always(Ok(9));
        assert_eq!(s.next().await, Ok(9));
        assert_eq!(s.calls(), 4);
    }
}
