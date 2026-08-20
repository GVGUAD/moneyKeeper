//! Monobank wire anti-corruption layer.

mod client;
mod dto;
mod normalizer;

pub use client::MonobankClient;
pub use normalizer::{MonobankAdapter, NormalizedResource, NormalizedSnapshot};
