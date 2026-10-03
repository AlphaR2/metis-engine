//! Storage and retrieval, nothing else - the only place that owns how state is kept. In-memory
//! (`Mutex`/`HashMap`) for MVP; this is the exact seam a future real database would slot into
//! without touching `services/`.

pub mod health_repository;
pub mod market_repository;
pub mod market_store_db;
pub mod subjects_db;
pub mod positions_cache;
