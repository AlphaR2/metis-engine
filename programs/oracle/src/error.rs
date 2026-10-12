use pinocchio::error::ProgramError;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OracleError {
    InvalidFootballData = 6_000,
    CreatorMustSign = 6_001,
    OracleMustBeSystemOwned = 6_002,
    OracleAlreadyInitialized = 6_003,
    InvalidOraclePda = 6_004,
    RentCalculationFailed = 6_005,
    OracleShouldNotBeEmpty = 6_006,
    InvalidAccountLength = 6_007,
    InvalidGenericOracleData = 6_008,
    InvalidStateTransition = 6_009,
    InvalidUpdateKind = 6_010,
    DuplicateAuthority = 6_011,
    AttestationComplete = 6_012,
    InvalidAuthority = 6_013,
    UnauthorizedAuthority = 6_014,
    InvalidAuthorityPda = 6_015,
    MissingRootAttestation = 6_016,
    /// The program is paused by a root signer.
    Paused = 6_017,
    UnpauseSignaturesMissing = 6_018,
    /// Kickoff isn't far enough ahead (or isn't in the future at all).
    KickoffTooSoon = 6_019,
    /// A result other than "Scheduled" was reported before kickoff.
    ResultBeforeKickoff = 6_020,
    /// A report claims something impossible (score before the match starts, penalties outside
    /// a shoot-out, ...).
    InvalidReport = 6_021,
    /// Every reporter slot on this fixture is taken by other reporters.
    TooManyReporters = 6_022,
    /// The result is final; only a two-root override within the override window can change it.
    OracleFinalized = 6_023,
    /// The result was voided; markets on it cancel.
    OracleVoided = 6_024,
    /// Override or void attempted after the override window closed.
    OverrideWindowClosed = 6_025,
    /// Close attempted before the grace period after finalising/voiding/kickoff.
    CloseTooEarly = 6_026,
    /// The instruction needs two distinct root signers and didn't get them.
    TwoRootSignersRequired = 6_027,
}

impl From<OracleError> for ProgramError {
    fn from(error: OracleError) -> Self {
        ProgramError::Custom(error as u32)
    }
}

impl metis_authority::errors::AuthorityErrors for OracleError {
    fn must_sign() -> Self {
        OracleError::CreatorMustSign
    }
    fn unauthorized_authority() -> Self {
        OracleError::UnauthorizedAuthority
    }
    fn already_initialized() -> Self {
        OracleError::OracleAlreadyInitialized
    }
    fn invalid_authority_pda() -> Self {
        OracleError::InvalidAuthorityPda
    }
    fn invalid_authority() -> Self {
        OracleError::InvalidAuthority
    }
    fn rent_calculation_failed() -> Self {
        OracleError::RentCalculationFailed
    }
    fn duplicate_authority() -> Self {
        OracleError::DuplicateAuthority
    }
    fn attestation_complete() -> Self {
        OracleError::AttestationComplete
    }
    fn missing_root_attestation() -> Self {
        OracleError::MissingRootAttestation
    }
    fn paused() -> Self {
        OracleError::Paused
    }
    fn unpause_signatures_missing() -> Self {
        OracleError::UnpauseSignaturesMissing
    }
}
