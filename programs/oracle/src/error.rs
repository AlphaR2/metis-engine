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
}
