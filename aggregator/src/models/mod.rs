//! Pure data shapes, no behavior - domain models, request/response DTOs, and vendor wire-format
//! types. Every struct here is data only; errors live in `crate::errors`, behavior in
//! `crate::services`.

pub mod execution;
pub mod position;
pub mod venue;
