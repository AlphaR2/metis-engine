use pinocchio::error::ProgramError;

/// Program-specific errors. These values are returned to clients as
/// `ProgramError::Custom(code)`.
#[repr(u32)]
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
