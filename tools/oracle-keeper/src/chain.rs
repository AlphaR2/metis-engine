//! Talking to the oracle program: the accounts it uses, the instructions it takes, and a small
//! `Chain` trait so the keeper can run against a real RPC node or, in tests, the program inside
//! litesvm.

use async_trait::async_trait;
use oracle::{
    constants::{AUTHORITY_SEED, FOOTBALL_SEED, GUARDIAN, INDIVIDUAL_AUTHORITY_SEED, SETTLED_SEED},
    instructions::POracle,
    state::MarketKind,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_message::Message;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::model::{Claim, FootballSettled};

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("account {0} is not a football oracle")]
    NotAnOracle(Pubkey),
    #[error("this chain access does not support {0}")]
    Unsupported(&'static str),
}

const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");

pub fn oracle_program() -> Pubkey {
    Pubkey::new_from_array(oracle::ID.to_bytes())
}

pub fn oracle_pda(fixture_root: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[FOOTBALL_SEED, SETTLED_SEED, fixture_root], &oracle_program()).0
}

/// The oracle's RootAuthority account.
pub fn authority_pda() -> Pubkey {
    Pubkey::find_program_address(&[AUTHORITY_SEED, GUARDIAN.as_ref()], &oracle_program()).0
}

/// A reporter's own attestation account (two roots vouched for it).
pub fn individual_authority_pda(reporter: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[INDIVIDUAL_AUTHORITY_SEED, reporter.as_ref()], &oracle_program()).0
}

/// Creates the fixture. `signer` must be root or an attested authority; pass its attestation
/// account unless it is root.
pub fn create_ix(signer: &Pubkey, attestation: Option<Pubkey>, fixture_root: &[u8; 32], kickoff: i64) -> Instruction {
    let mut data = vec![POracle::CreateOracle as u8, MarketKind::Football as u8];
    data.extend_from_slice(fixture_root);
    data.extend_from_slice(&kickoff.to_le_bytes());
    let mut accounts = vec![
        AccountMeta::new(*signer, true),
        AccountMeta::new(oracle_pda(fixture_root), false),
        AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        AccountMeta::new_readonly(authority_pda(), false),
    ];
    accounts.extend(attestation.map(|a| AccountMeta::new_readonly(a, false)));
    Instruction { program_id: oracle_program(), accounts, data }
}

/// One reporter's claim about the match.
pub fn submit_ix(reporter: &Pubkey, attestation: Option<Pubkey>, fixture_root: &[u8; 32], claim: &Claim) -> Instruction {
    let mut accounts = vec![
        AccountMeta::new_readonly(*reporter, true),
        AccountMeta::new(oracle_pda(fixture_root), false),
        AccountMeta::new_readonly(authority_pda(), false),
    ];
    accounts.extend(attestation.map(|a| AccountMeta::new_readonly(a, false)));
    Instruction {
        program_id: oracle_program(),
        accounts,
        data: vec![POracle::SubmitResult as u8, claim.status as u8, claim.score, claim.penalties],
    }
}

pub fn decode_oracle(address: &Pubkey, data: &[u8]) -> Result<FootballSettled, ChainError> {
    bytemuck::checked::try_from_bytes::<FootballSettled>(data)
        .copied()
        .map_err(|_| ChainError::NotAnOracle(*address))
}

#[async_trait]
pub trait Chain: Send + Sync {
    /// The football oracle at `address`, or `None` if nothing exists there yet.
    async fn oracle(&self, address: &Pubkey) -> Result<Option<FootballSettled>, ChainError>;
    /// Raw data of any account, or `None` if it doesn't exist. Used by the crank, which reads the
    /// market program's own accounts as well as the oracle's.
    async fn account(&self, _address: &Pubkey) -> Result<Option<Vec<u8>>, ChainError> {
        Err(ChainError::Unsupported("reading arbitrary accounts"))
    }
    /// Every account of one type a program owns, found by size and by an 8-byte discriminator at
    /// `discriminator_offset`.
    async fn program_accounts(
        &self,
        _program: &Pubkey,
        _data_size: usize,
        _discriminator_offset: usize,
        _discriminator: &[u8; 8],
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, ChainError> {
        Err(ChainError::Unsupported("listing a program's accounts"))
    }
    /// An account's lamport balance. The crank uses it to measure what each step costs.
    async fn balance(&self, _address: &Pubkey) -> Result<u64, ChainError> {
        Err(ChainError::Unsupported("reading balances"))
    }
    /// The cluster's current unix time. Time-based rules (dispute windows, claim windows) are
    /// judged against this, the same clock the program uses.
    async fn now(&self) -> Result<i64, ChainError> {
        Err(ChainError::Unsupported("reading the clock"))
    }
    /// Signs `instructions` with `payer` (who also pays the fee) and sends them. Returns the
    /// signature once the transaction has landed.
    async fn send(&self, payer: &Keypair, instructions: &[Instruction]) -> Result<String, ChainError>;
}

/// The real thing: a Solana RPC node.
pub struct RpcChain {
    client: solana_rpc_client::nonblocking::rpc_client::RpcClient,
    url: String,
    http: reqwest::Client,
}

impl RpcChain {
    pub fn new(rpc_url: &str) -> Self {
        Self {
            client: solana_rpc_client::nonblocking::rpc_client::RpcClient::new(rpc_url.to_string()),
            url: rpc_url.to_string(),
            http: reqwest::Client::new(),
        }
    }
}

impl RpcChain {
    /// Like `send`, with several signers (the first pays). For the few steps that need more than
    /// one key, such as two roots forfeiting a creator's unclaimed fees.
    pub async fn send_signed(&self, signers: &[&Keypair], instructions: &[Instruction]) -> Result<String, ChainError> {
        let payer = signers.first().ok_or_else(|| ChainError::Rpc("no signer".into()))?;
        let blockhash = self.client.get_latest_blockhash().await.map_err(|e| ChainError::Rpc(e.to_string()))?;
        let message = Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &blockhash);
        let tx = Transaction::new(signers, message, blockhash);
        let signature = self.client.send_and_confirm_transaction(&tx).await.map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(signature.to_string())
    }
}

#[async_trait]
impl Chain for RpcChain {
    async fn oracle(&self, address: &Pubkey) -> Result<Option<FootballSettled>, ChainError> {
        let response = self
            .client
            .get_account_with_commitment(address, self.client.commitment())
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        response.value.map(|account| decode_oracle(address, &account.data)).transpose()
    }

    async fn account(&self, address: &Pubkey) -> Result<Option<Vec<u8>>, ChainError> {
        let response = self
            .client
            .get_account_with_commitment(address, self.client.commitment())
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(response.value.map(|a| a.data))
    }

    async fn program_accounts(
        &self,
        program: &Pubkey,
        data_size: usize,
        discriminator_offset: usize,
        discriminator: &[u8; 8],
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, ChainError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as BASE64;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "getProgramAccounts",
            "params": [program.to_string(), {
                "encoding": "base64",
                "filters": [
                    { "dataSize": data_size },
                    { "memcmp": { "offset": discriminator_offset, "bytes": BASE64.encode(discriminator), "encoding": "base64" } },
                ],
            }],
        });
        let response: serde_json::Value = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| ChainError::Rpc(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        if let Some(error) = response.get("error") {
            return Err(ChainError::Rpc(error.to_string()));
        }
        let rows = response["result"].as_array().ok_or_else(|| ChainError::Rpc("getProgramAccounts returned no list".into()))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let address = row["pubkey"].as_str()?.parse::<Pubkey>().ok()?;
                let data = BASE64.decode(row["account"]["data"][0].as_str()?).ok()?;
                Some((address, data))
            })
            .collect())
    }

    async fn balance(&self, address: &Pubkey) -> Result<u64, ChainError> {
        self.client.get_balance(address).await.map_err(|e| ChainError::Rpc(e.to_string()))
    }

    async fn now(&self) -> Result<i64, ChainError> {
        // The cluster's own time for its latest slot, so a window the program judges against its
        // clock is judged here against the same one.
        let slot = self.client.get_slot().await.map_err(|e| ChainError::Rpc(e.to_string()))?;
        self.client.get_block_time(slot).await.map_err(|e| ChainError::Rpc(e.to_string()))
    }

    async fn send(&self, payer: &Keypair, instructions: &[Instruction]) -> Result<String, ChainError> {
        let blockhash = self.client.get_latest_blockhash().await.map_err(|e| ChainError::Rpc(e.to_string()))?;
        let message = Message::new_with_blockhash(instructions, Some(&payer.pubkey()), &blockhash);
        let tx = Transaction::new(&[payer], message, blockhash);
        let signature = self
            .client
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(|e| ChainError::Rpc(e.to_string()))?;
        Ok(signature.to_string())
    }
}

/// Wraps a chain so a flaky RPC node doesn't turn into a missed window: reads are retried a few
/// times with growing pauses; a send is retried only when the failure was clearly in transport
/// (never reached the node, timed out, rate limited), because a transaction the program rejected
/// will be rejected again and one that may have landed is re-checked by the next pass anyway.
pub struct Retrying<C> {
    inner: C,
    attempts: u32,
    first_pause: std::time::Duration,
}

impl<C> Retrying<C> {
    pub fn new(inner: C) -> Self {
        Self { inner, attempts: 4, first_pause: std::time::Duration::from_millis(400) }
    }

    pub fn with_policy(inner: C, attempts: u32, first_pause: std::time::Duration) -> Self {
        Self { inner, attempts: attempts.max(1), first_pause }
    }
}

/// Whether an error says the request never got a real answer from the node.
pub fn is_transient(error: &ChainError) -> bool {
    let ChainError::Rpc(text) = error else { return false };
    let text = text.to_ascii_lowercase();
    [
        "error sending request", "timed out", "timeout", "connection", "429", "too many requests",
        "502", "503", "504", "temporarily", "node is behind", "blockhash not found",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

macro_rules! retried {
    ($self:ident, $call:expr, $always:expr) => {{
        let mut pause = $self.first_pause;
        let mut attempt = 1;
        loop {
            match $call.await {
                Err(e) if attempt < $self.attempts && ($always || is_transient(&e)) => {
                    tracing::warn!(attempt, error = %e, "rpc call failed; retrying");
                    tokio::time::sleep(pause).await;
                    pause *= 2;
                    attempt += 1;
                }
                other => break other,
            }
        }
    }};
}

#[async_trait]
impl<C: Chain> Chain for Retrying<C> {
    async fn oracle(&self, address: &Pubkey) -> Result<Option<FootballSettled>, ChainError> {
        retried!(self, self.inner.oracle(address), true)
    }
    async fn account(&self, address: &Pubkey) -> Result<Option<Vec<u8>>, ChainError> {
        retried!(self, self.inner.account(address), true)
    }
    async fn program_accounts(
        &self,
        program: &Pubkey,
        data_size: usize,
        discriminator_offset: usize,
        discriminator: &[u8; 8],
    ) -> Result<Vec<(Pubkey, Vec<u8>)>, ChainError> {
        retried!(self, self.inner.program_accounts(program, data_size, discriminator_offset, discriminator), true)
    }
    async fn balance(&self, address: &Pubkey) -> Result<u64, ChainError> {
        retried!(self, self.inner.balance(address), true)
    }
    async fn now(&self) -> Result<i64, ChainError> {
        retried!(self, self.inner.now(), true)
    }
    async fn send(&self, payer: &Keypair, instructions: &[Instruction]) -> Result<String, ChainError> {
        retried!(self, self.inner.send(payer, instructions), false)
    }
}

#[cfg(test)]
mod retry_tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Fails `failures` times with `error`, then succeeds.
    struct Flaky {
        calls: AtomicU32,
        failures: u32,
        error: &'static str,
    }

    #[async_trait]
    impl Chain for Flaky {
        async fn oracle(&self, _: &Pubkey) -> Result<Option<FootballSettled>, ChainError> {
            Ok(None)
        }
        async fn now(&self) -> Result<i64, ChainError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures { Err(ChainError::Rpc(self.error.into())) } else { Ok(42) }
        }
        async fn send(&self, _: &Keypair, _: &[Instruction]) -> Result<String, ChainError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) < self.failures { Err(ChainError::Rpc(self.error.into())) } else { Ok("sig".into()) }
        }
    }

    fn quick(inner: Flaky) -> Retrying<Flaky> {
        Retrying::with_policy(inner, 3, std::time::Duration::from_millis(1))
    }

    #[tokio::test]
    async fn a_read_that_fails_twice_succeeds_on_the_third_try() {
        let chain = quick(Flaky { calls: AtomicU32::new(0), failures: 2, error: "boom" });
        assert_eq!(chain.now().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn a_read_that_keeps_failing_gives_up_with_the_error() {
        let chain = quick(Flaky { calls: AtomicU32::new(0), failures: 99, error: "boom" });
        assert!(chain.now().await.is_err());
        assert_eq!(chain.inner.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_send_the_program_rejected_is_not_repeated() {
        let chain = quick(Flaky { calls: AtomicU32::new(0), failures: 99, error: "custom program error: 0x2d" });
        let who = Keypair::new();
        assert!(chain.send(&who, &[]).await.is_err());
        assert_eq!(chain.inner.calls.load(Ordering::SeqCst), 1, "a rejected transaction would only be rejected again");
    }

    #[tokio::test]
    async fn a_send_that_never_reached_the_node_is_repeated() {
        let chain = quick(Flaky { calls: AtomicU32::new(0), failures: 2, error: "error sending request for url" });
        let who = Keypair::new();
        assert_eq!(chain.send(&who, &[]).await.unwrap(), "sig");
    }
}
