//! The provisioning steps.
//!
//! Each step is a probe and an install. `probe` answers whether the machine
//! already satisfies the step; `install` does the work and is idempotent, so a
//! second call on a satisfied machine changes nothing.

pub mod aegis;
pub mod cli;
pub mod codebase_memory;
pub mod donsetch;
pub mod mastery;
pub mod proxy_provider;
pub mod swarm_agents;
