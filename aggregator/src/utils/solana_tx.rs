//! Compiles a venue's instruction list into one **unsigned** v0 transaction.
//!
//! Panta's buy and claim builds return raw instructions plus a blockhash rather than a finished
//! transaction (its create build returns one). The wallet needs bytes it can sign, so this turns
//! the instructions into a `VersionedTransaction` with empty signature slots. It never signs and
//! never holds a key.
//!
//! Deliberately small and strict, because a transaction is what a user's money rides on:
//!
//! - the fee payer is always the caller's wallet, and it is the **only** signer allowed. An
//!   instruction that asks for any other signer cannot be satisfied by the user's wallet, so it is
//!   refused instead of being built into something that would fail, or worse, half-work
//! - every program an instruction invokes must be on an allowlist the caller supplies, so a venue
//!   response that tries to smuggle in an unexpected program is refused before it reaches a wallet
//! - no address lookup tables. Every account is listed in the message, which keeps the transaction
//!   fully inspectable by the client-side verifier (a lookup table can hide accounts)
//! - the result must fit Solana's 1232-byte packet limit
//!
//! The wire format is written out by hand and is cross-checked against the official Solana crate in
//! this module's tests, so a drift in either shows up as a failing test rather than a rejected
//! transaction in production.

use std::fmt;

/// Solana's transaction packet limit, in bytes.
pub const MAX_TRANSACTION_SIZE: usize = 1232;

/// Programs a venue's instructions may reasonably invoke besides the venue's own.
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";
pub const MEMO_PROGRAM: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
pub const MEMO_PROGRAM_V1: &str = "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo";

/// The supporting programs an order or claim transaction may use next to the venue's own.
pub const COMMON_PROGRAMS: [&str; 7] = [
    SYSTEM_PROGRAM,
    TOKEN_PROGRAM,
    TOKEN_2022_PROGRAM,
    ASSOCIATED_TOKEN_PROGRAM,
    COMPUTE_BUDGET_PROGRAM,
    MEMO_PROGRAM,
    MEMO_PROGRAM_V1,
];

pub type Pubkey = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountMeta {
    pub pubkey: Pubkey,
    pub is_signer: bool,
    pub is_writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub program_id: Pubkey,
    pub accounts: Vec<AccountMeta>,
    pub data: Vec<u8>,
}

/// Why a transaction could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    /// A pubkey or blockhash was not valid base58 of the right length.
    BadKey(String),
    /// An instruction requires a signer other than the wallet.
    ExtraSigner(String),
    /// An instruction invokes a program that is not on the allowlist.
    ProgramNotAllowed(String),
    /// No instructions at all.
    Empty,
    /// More accounts than a v0 message can index.
    TooManyAccounts(usize),
    /// The finished transaction would not fit in a packet.
    TooLarge(usize),
    /// A finished transaction a venue handed back could not be read as a Solana transaction.
    Malformed(String),
    /// A finished transaction names a fee payer or signer other than the caller's wallet.
    WrongSigner(String),
    /// A finished transaction contains an instruction that could move the wallet's funds or
    /// authority somewhere a purchase never needs to.
    ForbiddenInstruction(String),
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::BadKey(what) => write!(f, "invalid key in venue instructions: {what}"),
            CompileError::ExtraSigner(key) => write!(
                f,
                "the venue's instructions require a signer other than your wallet ({key}), which cannot be satisfied, so no transaction was built"
            ),
            CompileError::ProgramNotAllowed(key) => write!(
                f,
                "the venue's instructions invoke an unexpected program ({key}), so no transaction was built"
            ),
            CompileError::Empty => write!(f, "the venue returned no instructions"),
            CompileError::TooManyAccounts(n) => write!(f, "the transaction would reference {n} accounts; the limit is 256"),
            CompileError::TooLarge(n) => write!(f, "the transaction would be {n} bytes; the limit is {MAX_TRANSACTION_SIZE}"),
            CompileError::Malformed(what) => write!(f, "the venue's transaction could not be read ({what}), so it was not passed on"),
            CompileError::WrongSigner(what) => write!(f, "the venue's transaction does not match your wallet ({what}), so it was not passed on"),
            CompileError::ForbiddenInstruction(what) => write!(f, "the venue's transaction contains an instruction a trade never needs ({what}), so it was not passed on"),
        }
    }
}

impl std::error::Error for CompileError {}

/// Decodes a base58 string that must be exactly 32 bytes (a pubkey or a blockhash).
pub fn decode_32(text: &str) -> Result<[u8; 32], CompileError> {
    let bytes = bs58::decode(text.trim())
        .into_vec()
        .map_err(|_| CompileError::BadKey(format!("'{}' is not base58", truncate(text))))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| CompileError::BadKey(format!("'{}' decodes to {} bytes, expected 32", truncate(text), bytes.len())))
}

/// Base58 of a 32-byte key, for messages.
pub fn encode_32(key: &Pubkey) -> String {
    bs58::encode(key).into_string()
}

fn truncate(text: &str) -> String {
    text.chars().take(48).collect()
}

/// Refuses any instruction that invokes a program outside `allowed`.
pub fn check_programs(instructions: &[Instruction], allowed: &[&str]) -> Result<(), CompileError> {
    let allowed: Vec<Pubkey> = allowed.iter().filter_map(|k| decode_32(k).ok()).collect();
    for ix in instructions {
        if !allowed.contains(&ix.program_id) {
            return Err(CompileError::ProgramNotAllowed(encode_32(&ix.program_id)));
        }
    }
    Ok(())
}

/// Compact-u16 ("shortvec") length prefix.
fn push_shortvec(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        byte |= 0x80;
        out.push(byte);
    }
}

/// One account in the message, with the strongest flags any instruction gave it.
struct KeyEntry {
    key: Pubkey,
    signer: bool,
    writable: bool,
}

fn upsert(keys: &mut Vec<KeyEntry>, key: Pubkey, signer: bool, writable: bool) {
    match keys.iter_mut().find(|k| k.key == key) {
        Some(entry) => {
            entry.signer |= signer;
            entry.writable |= writable;
        }
        None => keys.push(KeyEntry { key, signer, writable }),
    }
}

/// Builds the unsigned transaction bytes: an empty signature slot for the wallet, then the v0
/// message. The caller base64-encodes the result.
pub fn compile_unsigned_v0(
    fee_payer: Pubkey,
    instructions: &[Instruction],
    recent_blockhash: [u8; 32],
) -> Result<Vec<u8>, CompileError> {
    if instructions.is_empty() {
        return Err(CompileError::Empty);
    }

    // The payer is always first, always a writable signer.
    let mut keys: Vec<KeyEntry> = Vec::new();
    upsert(&mut keys, fee_payer, true, true);
    for ix in instructions {
        for meta in &ix.accounts {
            upsert(&mut keys, meta.pubkey, meta.is_signer, meta.is_writable);
        }
        // The program is invoked, never signs, and is not written to.
        upsert(&mut keys, ix.program_id, false, false);
    }

    if let Some(other) = keys.iter().find(|k| k.signer && k.key != fee_payer) {
        return Err(CompileError::ExtraSigner(encode_32(&other.key)));
    }
    if keys.len() > 256 {
        return Err(CompileError::TooManyAccounts(keys.len()));
    }

    // Solana orders accounts: writable signers, readonly signers, writable non-signers, readonly
    // non-signers. Within a group they are sorted by key, as the official compiler does, so the
    // bytes are identical to what it would produce; only the payer is pinned to the front.
    let mut ordered: Vec<&KeyEntry> = Vec::with_capacity(keys.len());
    for (signer, writable) in [(true, true), (true, false), (false, true), (false, false)] {
        let mut group: Vec<&KeyEntry> = keys.iter().filter(|k| k.signer == signer && k.writable == writable).collect();
        group.sort_by(|a, b| (a.key != fee_payer).cmp(&(b.key != fee_payer)).then_with(|| a.key.cmp(&b.key)));
        ordered.extend(group);
    }
    let num_signers = ordered.iter().filter(|k| k.signer).count();
    let readonly_signed = ordered.iter().filter(|k| k.signer && !k.writable).count();
    let readonly_unsigned = ordered.iter().filter(|k| !k.signer && !k.writable).count();
    let index_of = |key: &Pubkey| ordered.iter().position(|k| &k.key == key).expect("every key was collected above") as u8;

    let mut message = vec![0x80]; // v0 prefix
    message.extend_from_slice(&[num_signers as u8, readonly_signed as u8, readonly_unsigned as u8]);
    push_shortvec(&mut message, ordered.len());
    for entry in &ordered {
        message.extend_from_slice(&entry.key);
    }
    message.extend_from_slice(&recent_blockhash);
    push_shortvec(&mut message, instructions.len());
    for ix in instructions {
        message.push(index_of(&ix.program_id));
        push_shortvec(&mut message, ix.accounts.len());
        for meta in &ix.accounts {
            message.push(index_of(&meta.pubkey));
        }
        push_shortvec(&mut message, ix.data.len());
        message.extend_from_slice(&ix.data);
    }
    push_shortvec(&mut message, 0); // no address lookup tables

    let mut transaction = Vec::with_capacity(1 + 64 * num_signers + message.len());
    push_shortvec(&mut transaction, num_signers);
    transaction.extend(std::iter::repeat_n(0u8, 64 * num_signers));
    transaction.extend_from_slice(&message);

    if transaction.len() > MAX_TRANSACTION_SIZE {
        return Err(CompileError::TooLarge(transaction.len()));
    }
    Ok(transaction)
}

// ── Inspecting a transaction a venue already built ───────────────────────────────────────────────
//
// Jupiter returns a finished transaction, not instructions, so there is nothing to compile: the
// only safe thing to do with it is read it and refuse to pass it on unless it is what a trade
// should be. The wallet's own client-side check is the last line of defence; this is the one that
// stops a compromised or spoofed venue response at the API, before it ever reaches a wallet.

/// A cursor over transaction bytes that fails instead of panicking on short input.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CompileError> {
        let end = self.pos.checked_add(n).filter(|end| *end <= self.bytes.len());
        let end = end.ok_or_else(|| CompileError::Malformed("ends early".to_string()))?;
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CompileError> {
        Ok(self.take(1)?[0])
    }

    /// Compact-u16 length prefix, at most three bytes and at most 65535.
    fn shortvec(&mut self) -> Result<usize, CompileError> {
        let mut value = 0usize;
        for i in 0..3 {
            let byte = self.u8()?;
            value |= usize::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return if value <= usize::from(u16::MAX) {
                    Ok(value)
                } else {
                    Err(CompileError::Malformed("length prefix out of range".to_string()))
                };
            }
        }
        Err(CompileError::Malformed("length prefix too long".to_string()))
    }
}

/// One instruction of an inspected transaction, with accounts still as message indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedInstruction {
    pub program_index: usize,
    pub account_indexes: Vec<usize>,
    pub data: Vec<u8>,
}

/// What a serialized transaction contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedTransaction {
    pub num_required_signatures: usize,
    /// The accounts listed in the message itself. Accounts pulled in through an address lookup
    /// table come after these and are not listed here.
    pub static_keys: Vec<Pubkey>,
    pub instructions: Vec<InspectedInstruction>,
    pub lookup_tables: usize,
}

/// Reads a serialized legacy or v0 transaction.
pub fn inspect_transaction(bytes: &[u8]) -> Result<InspectedTransaction, CompileError> {
    let mut r = Reader { bytes, pos: 0 };
    let signature_count = r.shortvec()?;
    r.take(signature_count.checked_mul(64).ok_or_else(|| CompileError::Malformed("too many signatures".to_string()))?)?;

    let first = r.u8()?;
    let versioned = first & 0x80 != 0;
    if versioned && first & 0x7f != 0 {
        return Err(CompileError::Malformed(format!("unsupported message version {}", first & 0x7f)));
    }
    // A legacy message has no prefix: its first byte is already the header's signature count.
    let num_required_signatures = if versioned { usize::from(r.u8()?) } else { usize::from(first) };
    r.take(2)?; // readonly signed, readonly unsigned
    if signature_count != num_required_signatures {
        return Err(CompileError::Malformed(format!(
            "{signature_count} signature slots for {num_required_signatures} required signers"
        )));
    }

    let key_count = r.shortvec()?;
    let mut static_keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        let key: Pubkey = r.take(32)?.try_into().expect("took 32 bytes");
        static_keys.push(key);
    }
    r.take(32)?; // recent blockhash

    let instruction_count = r.shortvec()?;
    let mut instructions = Vec::with_capacity(instruction_count);
    for _ in 0..instruction_count {
        let program_index = usize::from(r.u8()?);
        let account_count = r.shortvec()?;
        let account_indexes = r.take(account_count)?.iter().map(|i| usize::from(*i)).collect();
        let data_len = r.shortvec()?;
        let data = r.take(data_len)?.to_vec();
        instructions.push(InspectedInstruction { program_index, account_indexes, data });
    }

    let mut lookup_tables = 0;
    if versioned {
        lookup_tables = r.shortvec()?;
        for _ in 0..lookup_tables {
            r.take(32)?;
            let writable = r.shortvec()?;
            r.take(writable)?;
            let readonly = r.shortvec()?;
            r.take(readonly)?;
        }
    }
    if r.pos != bytes.len() {
        return Err(CompileError::Malformed("bytes left over after the message".to_string()));
    }
    Ok(InspectedTransaction { num_required_signatures, static_keys, instructions, lookup_tables })
}

/// Token-program instruction tags that move tokens or hand over control of them.
const TOKEN_TRANSFER: u8 = 3;
const TOKEN_APPROVE: u8 = 4;
const TOKEN_SET_AUTHORITY: u8 = 6;
const TOKEN_CLOSE_ACCOUNT: u8 = 9;
const TOKEN_TRANSFER_CHECKED: u8 = 12;
const TOKEN_APPROVE_CHECKED: u8 = 13;

/// System-program instruction tags (a little-endian u32) that move SOL or hand over an account.
const SYSTEM_CREATE_ACCOUNT: u32 = 0;
const SYSTEM_ASSIGN: u32 = 1;
const SYSTEM_TRANSFER: u32 = 2;
const SYSTEM_WITHDRAW_NONCE: u32 = 5;
const SYSTEM_AUTHORIZE_NONCE: u32 = 7;
const SYSTEM_ASSIGN_WITH_SEED: u32 = 10;
const SYSTEM_TRANSFER_WITH_SEED: u32 = 11;

/// The most lamports one top-level System instruction may move: enough for a token account's rent, not
/// a drain. The same figure the web client's verifier uses, so the two layers never disagree about it.
pub const MAX_SYSTEM_LAMPORTS: u64 = 5_000_000;

/// Refuses a finished transaction that is not a plain trade for `wallet`.
///
/// A purchase, sale or claim needs exactly these things, so anything else is refused:
///
/// - the wallet is the fee payer and the **only** signer, so nothing in it needs a key we don't
///   hold and nothing can be co-signed by a party the user never sees
/// - every program it invokes is on `allowed` (the venue's own plus the common supporting ones).
///   A program id can never come from an address lookup table, so this holds even when tables are
///   used
/// - at the top level it never transfers tokens, approves a delegate, reassigns an account or changes
///   an authority. The money for a trade moves inside the venue's program; a plain token transfer
///   next to it is what a tampered transaction looks like. SOL may move only in rent-sized amounts
///   (creating or funding an account), and closing a token account is allowed only when its
///   remainder goes back to the wallet
///
/// This cannot prove the venue's own program does what it says. It removes the cheap attacks, and
/// the preview shown to the user (cost, contracts, payout) remains what they approve.
pub fn verify_venue_transaction(transaction_b64: &str, wallet: &str, allowed: &[&str]) -> Result<(), CompileError> {
    use base64::Engine as _;

    let wallet = decode_32(wallet)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(transaction_b64.trim())
        .map_err(|_| CompileError::Malformed("not base64".to_string()))?;
    if bytes.len() > MAX_TRANSACTION_SIZE {
        return Err(CompileError::TooLarge(bytes.len()));
    }
    let tx = inspect_transaction(&bytes)?;

    if tx.num_required_signatures != 1 {
        return Err(CompileError::WrongSigner(format!("it needs {} signatures, expected only yours", tx.num_required_signatures)));
    }
    if tx.static_keys.first() != Some(&wallet) {
        return Err(CompileError::WrongSigner("its fee payer is not your wallet".to_string()));
    }
    if tx.instructions.is_empty() {
        return Err(CompileError::Empty);
    }

    let allowed: Vec<Pubkey> = allowed.iter().filter_map(|k| decode_32(k).ok()).collect();
    let token_programs = [decode_32(TOKEN_PROGRAM)?, decode_32(TOKEN_2022_PROGRAM)?];
    let system_program = decode_32(SYSTEM_PROGRAM)?;

    for ix in &tx.instructions {
        // A program that is not in the static keys was claimed from a lookup table, which the
        // runtime does not allow; either way it is not one we can vouch for.
        let program = tx
            .static_keys
            .get(ix.program_index)
            .ok_or_else(|| CompileError::ProgramNotAllowed(format!("index {} (not in the message's own accounts)", ix.program_index)))?;
        if !allowed.contains(program) {
            return Err(CompileError::ProgramNotAllowed(encode_32(program)));
        }

        if token_programs.contains(program) {
            let tag = ix.data.first().copied();
            match tag {
                Some(TOKEN_TRANSFER) | Some(TOKEN_TRANSFER_CHECKED) => {
                    return Err(CompileError::ForbiddenInstruction("a direct token transfer".to_string()));
                }
                Some(TOKEN_APPROVE) | Some(TOKEN_APPROVE_CHECKED) => {
                    return Err(CompileError::ForbiddenInstruction("a token delegate approval".to_string()));
                }
                Some(TOKEN_SET_AUTHORITY) => {
                    return Err(CompileError::ForbiddenInstruction("a token authority change".to_string()));
                }
                Some(TOKEN_CLOSE_ACCOUNT) => {
                    // accounts: [account, destination, owner]
                    let destination = ix.account_indexes.get(1).and_then(|i| tx.static_keys.get(*i));
                    if destination != Some(&wallet) {
                        return Err(CompileError::ForbiddenInstruction("closing a token account into another address".to_string()));
                    }
                }
                _ => {}
            }
        } else if *program == system_program {
            let tag = ix.data.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            match tag {
                Some(SYSTEM_CREATE_ACCOUNT) | Some(SYSTEM_TRANSFER) => {
                    let lamports = ix.data.get(4..12).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")));
                    if !lamports.is_some_and(|l| l <= MAX_SYSTEM_LAMPORTS) {
                        return Err(CompileError::ForbiddenInstruction("a SOL transfer larger than an account's rent".to_string()));
                    }
                }
                Some(SYSTEM_TRANSFER_WITH_SEED) => {
                    return Err(CompileError::ForbiddenInstruction("a seeded SOL transfer".to_string()));
                }
                Some(SYSTEM_ASSIGN) | Some(SYSTEM_ASSIGN_WITH_SEED) => {
                    return Err(CompileError::ForbiddenInstruction("an account ownership change".to_string()));
                }
                Some(SYSTEM_WITHDRAW_NONCE) | Some(SYSTEM_AUTHORIZE_NONCE) => {
                    return Err(CompileError::ForbiddenInstruction("a nonce account change".to_string()));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Pubkey {
        let mut k = [0u8; 32];
        k[0] = n;
        k[31] = n.wrapping_mul(7).wrapping_add(1);
        k
    }

    fn meta(n: u8, signer: bool, writable: bool) -> AccountMeta {
        AccountMeta { pubkey: key(n), is_signer: signer, is_writable: writable }
    }

    const PAYER: u8 = 1;
    const PROGRAM_A: u8 = 50;
    const PROGRAM_B: u8 = 51;
    const BLOCKHASH: [u8; 32] = [9; 32];

    fn sample() -> Vec<Instruction> {
        vec![
            Instruction {
                program_id: key(PROGRAM_A),
                accounts: vec![meta(PAYER, true, true), meta(10, false, true), meta(11, false, false)],
                data: vec![1, 2, 3],
            },
            Instruction {
                program_id: key(PROGRAM_B),
                accounts: vec![meta(11, false, true), meta(PAYER, true, false), meta(12, false, false), meta(5, false, false)],
                data: vec![],
            },
        ]
    }

    #[test]
    fn shortvec_matches_the_documented_encoding() {
        for (value, expected) in [(0usize, vec![0x00]), (0x7f, vec![0x7f]), (0x80, vec![0x80, 0x01]), (0x3fff, vec![0xff, 0x7f]), (0x4000, vec![0x80, 0x80, 0x01])] {
            let mut out = Vec::new();
            push_shortvec(&mut out, value);
            assert_eq!(out, expected, "value {value}");
        }
    }

    #[test]
    fn the_payer_is_first_and_flags_merge_to_the_strongest() {
        let bytes = compile_unsigned_v0(key(PAYER), &sample(), BLOCKHASH).unwrap();
        // 1 signature count + 64 empty signature bytes, then the v0 prefix.
        assert_eq!(bytes[0], 1);
        assert!(bytes[1..65].iter().all(|b| *b == 0), "signature slots are empty: nothing is signed here");
        assert_eq!(bytes[65], 0x80);
        // Header: one signer. The payer is writable in the first instruction, so it stays a
        // writable signer although the second lists it readonly. Account 11 is readonly in the
        // first instruction and writable in the second, so it ends up writable. That leaves four
        // readonly non-signers: keys 5 and 12 and the two programs.
        assert_eq!(&bytes[66..69], &[1, 0, 4]);
        // Account count, then the payer first.
        assert_eq!(bytes[69], 7);
        assert_eq!(&bytes[70..102], &key(PAYER));
    }

    #[test]
    fn a_second_signer_is_refused() {
        let mut ixs = sample();
        ixs[0].accounts.push(meta(99, true, false));
        assert_eq!(compile_unsigned_v0(key(PAYER), &ixs, BLOCKHASH), Err(CompileError::ExtraSigner(encode_32(&key(99)))));
    }

    #[test]
    fn no_instructions_is_refused() {
        assert_eq!(compile_unsigned_v0(key(PAYER), &[], BLOCKHASH), Err(CompileError::Empty));
    }

    #[test]
    fn an_oversized_transaction_is_refused() {
        let mut ixs = sample();
        ixs[0].data = vec![0u8; 2_000];
        assert!(matches!(compile_unsigned_v0(key(PAYER), &ixs, BLOCKHASH), Err(CompileError::TooLarge(_))));
    }

    #[test]
    fn an_unlisted_program_is_refused_and_a_listed_one_passes() {
        let a = encode_32(&key(PROGRAM_A));
        let b = encode_32(&key(PROGRAM_B));
        assert!(check_programs(&sample(), &[a.as_str(), b.as_str()]).is_ok());
        assert_eq!(check_programs(&sample(), &[a.as_str()]), Err(CompileError::ProgramNotAllowed(b)));
        assert!(check_programs(&sample(), &[]).is_err());
    }

    #[test]
    fn keys_must_be_base58_of_32_bytes() {
        assert!(decode_32("11111111111111111111111111111111").is_ok());
        assert!(matches!(decode_32("0OIl"), Err(CompileError::BadKey(_))), "0, O, I and l are not base58");
        assert!(matches!(decode_32("3yZe7d"), Err(CompileError::BadKey(_))), "wrong length");
    }

    #[test]
    fn the_common_programs_are_valid_keys() {
        for program in COMMON_PROGRAMS {
            assert!(decode_32(program).is_ok(), "{program}");
        }
    }

    /// The hand-written encoding must equal what the official Solana crate produces for the same
    /// instructions. This is the guard against the two drifting apart.
    #[test]
    fn matches_the_official_solana_encoding() {
        use solana_hash::Hash;
        use solana_instruction::{AccountMeta as SolMeta, Instruction as SolIx};
        use solana_message::{VersionedMessage, v0};
        use solana_pubkey::Pubkey as SolKey;

        let to_sol = |ix: &Instruction| SolIx {
            program_id: SolKey::new_from_array(ix.program_id),
            accounts: ix
                .accounts
                .iter()
                .map(|m| SolMeta { pubkey: SolKey::new_from_array(m.pubkey), is_signer: m.is_signer, is_writable: m.is_writable })
                .collect(),
            data: ix.data.clone(),
        };
        let ours = sample();
        let theirs: Vec<SolIx> = ours.iter().map(to_sol).collect();

        let message = v0::Message::try_compile(&SolKey::new_from_array(key(PAYER)), &theirs, &[], Hash::new_from_array(BLOCKHASH))
            .expect("official compile");
        let official_message = VersionedMessage::V0(message).serialize();

        let ours_bytes = compile_unsigned_v0(key(PAYER), &ours, BLOCKHASH).unwrap();
        assert_eq!(&ours_bytes[65..], official_message.as_slice(), "message bytes differ from the official encoding");
    }

    // ── verify_venue_transaction ──────────────────────────────────────────────────────────────

    const VENUE: u8 = 60;

    fn b64(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn venue_program() -> String {
        encode_32(&key(VENUE))
    }

    fn allowed() -> Vec<String> {
        let mut allowed = vec![venue_program()];
        allowed.extend(COMMON_PROGRAMS.iter().map(|p| p.to_string()));
        allowed
    }

    fn verify(ixs: &[Instruction]) -> Result<(), CompileError> {
        let bytes = compile_unsigned_v0(key(PAYER), ixs, BLOCKHASH).unwrap();
        let allowed = allowed();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        verify_venue_transaction(&b64(&bytes), &encode_32(&key(PAYER)), &allowed)
    }

    fn venue_ix() -> Instruction {
        Instruction {
            program_id: key(VENUE),
            accounts: vec![meta(PAYER, true, true), meta(10, false, true), meta(11, false, false)],
            data: vec![7, 7, 7],
        }
    }

    fn ix_for(program: &str, data: Vec<u8>, accounts: Vec<AccountMeta>) -> Instruction {
        Instruction { program_id: decode_32(program).unwrap(), accounts, data }
    }

    #[test]
    fn a_trade_by_the_wallet_through_allowed_programs_passes() {
        let set_limit = ix_for(COMPUTE_BUDGET_PROGRAM, vec![2, 1, 0, 0, 0], vec![]);
        assert_eq!(verify(&[set_limit, venue_ix()]), Ok(()));
    }

    #[test]
    fn inspection_reads_back_what_the_compiler_wrote() {
        let bytes = compile_unsigned_v0(key(PAYER), &sample(), BLOCKHASH).unwrap();
        let tx = inspect_transaction(&bytes).unwrap();
        assert_eq!(tx.num_required_signatures, 1);
        assert_eq!(tx.static_keys[0], key(PAYER));
        assert_eq!(tx.instructions.len(), 2);
        assert_eq!(tx.instructions[0].data, vec![1, 2, 3]);
        assert_eq!(tx.lookup_tables, 0);
    }

    #[test]
    fn a_transaction_for_a_different_wallet_is_refused() {
        let bytes = compile_unsigned_v0(key(PAYER), &[venue_ix()], BLOCKHASH).unwrap();
        let allowed = allowed();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        let other = encode_32(&key(2));
        assert!(matches!(verify_venue_transaction(&b64(&bytes), &other, &allowed), Err(CompileError::WrongSigner(_))));
    }

    #[test]
    fn an_unlisted_program_is_refused() {
        let rogue = Instruction { program_id: key(99), accounts: vec![meta(PAYER, true, true)], data: vec![] };
        assert_eq!(verify(&[venue_ix(), rogue]), Err(CompileError::ProgramNotAllowed(encode_32(&key(99)))));
    }

    #[test]
    fn a_direct_token_transfer_is_refused() {
        let transfer = ix_for(TOKEN_PROGRAM, vec![3, 1, 0, 0, 0, 0, 0, 0, 0], vec![meta(10, false, true), meta(11, false, true), meta(PAYER, true, false)]);
        assert!(matches!(verify(&[venue_ix(), transfer]), Err(CompileError::ForbiddenInstruction(_))));
        let checked = ix_for(TOKEN_2022_PROGRAM, vec![12, 1, 0, 0, 0, 0, 0, 0, 0, 6], vec![meta(10, false, true), meta(12, false, false), meta(11, false, true), meta(PAYER, true, false)]);
        assert!(matches!(verify(&[venue_ix(), checked]), Err(CompileError::ForbiddenInstruction(_))));
    }

    #[test]
    fn a_delegate_approval_and_an_authority_change_are_refused() {
        let approve = ix_for(TOKEN_PROGRAM, vec![4, 255, 255, 255, 255, 255, 255, 255, 255], vec![meta(10, false, true), meta(11, false, false), meta(PAYER, true, false)]);
        assert!(matches!(verify(&[venue_ix(), approve]), Err(CompileError::ForbiddenInstruction(_))));
        let set_authority = ix_for(TOKEN_PROGRAM, vec![6, 2, 1], vec![meta(10, false, true), meta(PAYER, true, false)]);
        assert!(matches!(verify(&[venue_ix(), set_authority]), Err(CompileError::ForbiddenInstruction(_))));
    }

    #[test]
    fn a_large_system_transfer_or_an_assign_is_refused_but_rent_is_fine() {
        let send = |lamports: u64| {
            let mut data = vec![2, 0, 0, 0];
            data.extend_from_slice(&lamports.to_le_bytes());
            ix_for(SYSTEM_PROGRAM, data, vec![meta(PAYER, true, true), meta(10, false, true)])
        };
        assert_eq!(verify(&[venue_ix(), send(2_039_280)]), Ok(()), "a token account's rent");
        assert_eq!(verify(&[venue_ix(), send(MAX_SYSTEM_LAMPORTS)]), Ok(()));
        assert!(matches!(verify(&[venue_ix(), send(MAX_SYSTEM_LAMPORTS + 1)]), Err(CompileError::ForbiddenInstruction(_))));
        assert!(matches!(verify(&[venue_ix(), send(1_000_000_000)]), Err(CompileError::ForbiddenInstruction(_))));
        let truncated = ix_for(SYSTEM_PROGRAM, vec![2, 0, 0, 0, 1], vec![meta(PAYER, true, true), meta(10, false, true)]);
        assert!(matches!(verify(&[venue_ix(), truncated]), Err(CompileError::ForbiddenInstruction(_))), "an amount that cannot be read is refused");
        let mut create = vec![0, 0, 0, 0];
        create.extend_from_slice(&900_000_000u64.to_le_bytes());
        create.extend_from_slice(&[0u8; 40]);
        let create = ix_for(SYSTEM_PROGRAM, create, vec![meta(PAYER, true, true), meta(11, false, true)]);
        assert!(matches!(verify(&[venue_ix(), create]), Err(CompileError::ForbiddenInstruction(_))), "an oversized account creation");
        let mut assign = vec![1, 0, 0, 0];
        assign.extend_from_slice(&key(99));
        let assign = ix_for(SYSTEM_PROGRAM, assign, vec![meta(PAYER, true, true)]);
        assert!(matches!(verify(&[venue_ix(), assign]), Err(CompileError::ForbiddenInstruction(_))));
    }

    #[test]
    fn closing_a_token_account_is_allowed_only_back_to_the_wallet() {
        let close_to_wallet = ix_for(TOKEN_PROGRAM, vec![9], vec![meta(10, false, true), meta(PAYER, true, true), meta(PAYER, true, false)]);
        assert_eq!(verify(&[venue_ix(), close_to_wallet]), Ok(()));
        let close_elsewhere = ix_for(TOKEN_PROGRAM, vec![9], vec![meta(10, false, true), meta(11, false, true), meta(PAYER, true, false)]);
        assert!(matches!(verify(&[venue_ix(), close_elsewhere]), Err(CompileError::ForbiddenInstruction(_))));
    }

    /// A transaction that needs a second signature, written by the official crate so the test does
    /// not depend on our own compiler (which refuses to produce one).
    fn official_two_signer_transaction() -> Vec<u8> {
        use solana_hash::Hash;
        use solana_instruction::{AccountMeta as SolMeta, Instruction as SolIx};
        use solana_message::{VersionedMessage, v0};
        use solana_pubkey::Pubkey as SolKey;

        let ix = SolIx {
            program_id: SolKey::new_from_array(key(VENUE)),
            accounts: vec![
                SolMeta { pubkey: SolKey::new_from_array(key(PAYER)), is_signer: true, is_writable: true },
                SolMeta { pubkey: SolKey::new_from_array(key(99)), is_signer: true, is_writable: false },
            ],
            data: vec![1],
        };
        let message = v0::Message::try_compile(&SolKey::new_from_array(key(PAYER)), &[ix], &[], Hash::new_from_array(BLOCKHASH)).unwrap();
        let mut bytes = vec![2u8];
        bytes.extend(std::iter::repeat_n(0u8, 128));
        bytes.extend(VersionedMessage::V0(message).serialize());
        bytes
    }

    #[test]
    fn a_second_required_signer_is_refused() {
        let allowed = allowed();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        let result = verify_venue_transaction(&b64(&official_two_signer_transaction()), &encode_32(&key(PAYER)), &allowed);
        assert!(matches!(result, Err(CompileError::WrongSigner(_))), "{result:?}");
    }

    #[test]
    fn garbage_truncation_and_trailing_bytes_are_refused_without_panicking() {
        let bytes = compile_unsigned_v0(key(PAYER), &[venue_ix()], BLOCKHASH).unwrap();
        let allowed = allowed();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        let wallet = encode_32(&key(PAYER));
        assert!(matches!(verify_venue_transaction("not base64!!", &wallet, &allowed), Err(CompileError::Malformed(_))));
        for cut in [0, 1, 40, 65, 70, bytes.len() - 1] {
            assert!(verify_venue_transaction(&b64(&bytes[..cut]), &wallet, &allowed).is_err(), "cut at {cut}");
        }
        let mut padded = bytes.clone();
        padded.push(0);
        assert!(matches!(verify_venue_transaction(&b64(&padded), &wallet, &allowed), Err(CompileError::Malformed(_))));
        assert!(matches!(verify_venue_transaction(&b64(&vec![0u8; MAX_TRANSACTION_SIZE + 1]), &wallet, &allowed), Err(CompileError::TooLarge(_))));
    }

    #[test]
    fn a_program_index_outside_the_static_keys_is_refused() {
        // Point the only instruction's program index past the account list.
        let mut bytes = compile_unsigned_v0(key(PAYER), &[venue_ix()], BLOCKHASH).unwrap();
        let tx = inspect_transaction(&bytes).unwrap();
        // 1 + 64 sigs, 4 header bytes (prefix + 3), account count, keys, blockhash, instruction count.
        let program_index_at = 65 + 4 + 1 + 32 * tx.static_keys.len() + 32 + 1;
        bytes[program_index_at] = 200;
        let allowed = allowed();
        let allowed: Vec<&str> = allowed.iter().map(String::as_str).collect();
        assert!(matches!(
            verify_venue_transaction(&b64(&bytes), &encode_32(&key(PAYER)), &allowed),
            Err(CompileError::ProgramNotAllowed(_))
        ));
    }
}
