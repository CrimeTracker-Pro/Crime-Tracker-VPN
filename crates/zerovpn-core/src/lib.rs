//! Domain types and config shared across all Crime Tracker VPN crates.
//!
//! This crate has zero dependencies on web/db/runtime crates so the domain
//! model stays pure and testable.

pub mod config;
pub mod geo;
pub mod models;
