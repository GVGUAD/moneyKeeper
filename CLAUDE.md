# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`moneykeeper` is a Rust project (edition 2024) — a personal finance / money-keeping application.

## Commands

```bash
cargo build          # compile
cargo run            # run
cargo test           # run all tests
cargo test <name>    # run a single test by name
cargo clippy         # lint
cargo fmt            # format
```

## Architecture

Single-crate project (no workspace) following Domain-Driven Design (DDD).

```
src/
  domain/         # Entities, value objects, aggregates, repository traits, domain events
  application/    # Use cases that orchestrate domain logic
  infrastructure/ # Repository implementations, DB clients, external services
  main.rs         # Entry point — wires layers together
```

**Dependency rule:** `domain` has no dependencies on other layers. `application` depends only on `domain`. `infrastructure` depends on both.

Typical patterns per layer:
- `domain/` — structs for entities/value objects, `trait` for repositories, `enum` for domain events
- `application/` — one struct per use case (e.g. `TransferFundsUseCase`), takes repository traits as constructor arguments
- `infrastructure/` — concrete repository structs that implement domain traits

## Error Handling

Use `thiserror` for domain/library errors and `anyhow` for application-level propagation:

```rust
// Domain errors — use thiserror
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("insufficient funds: needed {needed}, available {available}")]
    InsufficientFunds { needed: f64, available: f64 },
}

// Application code — use anyhow
use anyhow::Result;

fn run() -> Result<()> {
    // ? operator works on any error type
    Ok(())
}
```

## Async Runtime

Tokio. Entry point pattern:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    Ok(())
}
```

## Memory & Ownership

Follow this priority order — start simple and move up only when the compiler requires it:

1. `&T` / `&mut T` — plain references; default choice
2. `Arc<T>` — shared ownership across threads (equivalent to Java shared objects in concurrent code)
3. `Rc<T>` — single-threaded shared ownership; rarely needed

## Testing

Unit tests live in the same file as the code they test (in a `#[cfg(test)] mod tests` block). Integration tests go in `tests/`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() { }

    #[tokio::test]
    async fn test_async_something() { }
}
```
