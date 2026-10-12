//! One keeper: one provider, one on-chain reporter key. Run one per provider; the oracle applies
//! a result only when two of them agree.

use std::sync::Arc;

use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

use crate::{
    chain::{self, Chain, ChainError},
    model::Claim,
    planner::{self, Plan},
    sources::{ScoreSource, SourceError},
};

/// A match this keeper watches: the oracle's fixture root, and this provider's own ID for it.
#[derive(Clone, Debug)]
pub struct Watch {
    pub fixture_root: [u8; 32],
    pub external_id: String,
}

#[derive(Debug)]
pub enum Tick {
    /// The oracle account doesn't exist yet.
    NotCreated,
    /// The result is final or voided.
    Settled,
    /// Chain and provider already agree.
    UpToDate,
    /// Holding back, and why.
    Waiting(&'static str),
    /// A claim was sent.
    Submitted { claim: Claim, signature: String },
    /// The provider couldn't be read or reported something we won't act on. Logged, not fatal:
    /// the next tick tries again, and an abnormal match is for a human (override or void).
    SourceUnavailable(String),
}

#[derive(Debug, thiserror::Error)]
pub enum KeeperError {
    #[error(transparent)]
    Chain(#[from] ChainError),
}

pub struct Keeper<C: Chain> {
    pub chain: C,
    pub reporter: Keypair,
    pub source: Arc<dyn ScoreSource>,
    /// The reporter's attestation account, unless the reporter is a root signer.
    pub attestation: Option<Pubkey>,
}

impl<C: Chain> Keeper<C> {
    pub fn new(chain: C, reporter: Keypair, source: Arc<dyn ScoreSource>) -> Self {
        let attestation = Some(chain::individual_authority_pda(&reporter.pubkey()));
        Self { chain, reporter, source, attestation }
    }

    /// Looks at one match once: reads the chain, reads the provider, and sends the next claim
    /// if there is one.
    pub async fn tick(&self, watch: &Watch) -> Result<Tick, KeeperError> {
        let address = chain::oracle_pda(&watch.fixture_root);
        let Some(on_chain) = self.chain.oracle(&address).await? else {
            return Ok(Tick::NotCreated);
        };
        if on_chain.is_finalized() || on_chain.is_voided() {
            return Ok(Tick::Settled);
        }

        let observation = match self.source.observe(&watch.external_id).await {
            Ok(o) => o,
            Err(SourceError::Abnormal(why)) => return Ok(Tick::SourceUnavailable(format!("abnormal match: {why}"))),
            Err(e) => return Ok(Tick::SourceUnavailable(e.to_string())),
        };

        match planner::plan(&on_chain, &observation) {
            Plan::Settled => Ok(Tick::Settled),
            Plan::UpToDate => Ok(Tick::UpToDate),
            Plan::Waiting(why) => Ok(Tick::Waiting(why)),
            Plan::Claim(claim) => {
                let ix: Instruction =
                    chain::submit_ix(&self.reporter.pubkey(), self.attestation, &watch.fixture_root, &claim);
                let signature = self.chain.send(&self.reporter, &[ix]).await?;
                Ok(Tick::Submitted { claim, signature })
            }
        }
    }
}
