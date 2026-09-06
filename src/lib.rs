//! Drove evaluates repository-owned workspace declarations and reconciles a
//! backend (Herdr, Radiator) on demand.

pub mod backend;
pub mod cli;
pub mod dsl;
pub mod ir;
pub mod model;
pub mod planner;
pub mod readiness;
pub mod state;
