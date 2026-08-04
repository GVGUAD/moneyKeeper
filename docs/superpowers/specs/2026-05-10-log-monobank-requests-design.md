# Log Monobank API requests

## Context

The Monobank integration ([src/infrastructure/monobank_client.rs](../../../src/infrastructure/monobank_client.rs))
currently makes three HTTP calls (`get_accounts`, `get_statement`, `set_webhook`) and
emits no per-request logs. Higher-level orchestration in
[src/application/monobank.rs](../../../src/application/monobank.rs) logs sync start/end
and individual transaction outcomes, but if a Monobank call fails or returns an
unexpected payload, there is no record of the actual response.

Goal: after each Monobank HTTP call completes, emit a single
`tracing::info!` event with endpoint, status, latency, and full response body to aid
debugging and observability. Bearer token must never be logged.

## Scope

- Affects only the real client `ReqwestMonobankClient` in
  [src/infrastructure/monobank_client.rs](../../../src/infrastructure/monobank_client.rs).
- Mock clients (in tests) are unaffected.
- Three methods are instrumented: `get_accounts`, `get_statement`, `set_webhook`.

## Design

For each method, replace the current chained
`.send().await?.error_for_status()?.json().await?` with an explicit sequence:

1. Capture `std::time::Instant::now()` before sending.
2. `send().await` → keep `Response`.
3. Read `status()`.
4. `resp.text().await` → owned `String` body.
5. Compute `elapsed_ms`.
6. Emit:
   ```rust
   tracing::info!(
       endpoint = "<path>",
       %status,
       elapsed_ms,
       body = %body,
       "monobank request done"
   );
   ```
7. If `!status.is_success()`, return an `anyhow!` error (preserves prior
   non-2xx behaviour; the response body is now captured in the log).
8. For methods that parse JSON (`get_accounts`, `get_statement`):
   `serde_json::from_str(&body)` with `.context("... parse failed")`.
9. `set_webhook` ignores the body after logging and returns `Ok(())`.

### Endpoint labels

| Method | `endpoint` field |
| --- | --- |
| `get_accounts` | `/personal/client-info` |
| `get_statement` | `/personal/statement` |
| `set_webhook` | `/personal/webhook` |

Path params (account id, from/to timestamps) are intentionally excluded from the
label so events group cleanly; they remain visible via the response body.

### Token safety

No code path serializes the `X-Token` header into the log event. The `body`
field carries only the response, and `endpoint` is a static string per method.

## Files modified

- [src/infrastructure/monobank_client.rs](../../../src/infrastructure/monobank_client.rs)

No other files change. No new dependencies.

## Verification

- `cargo build` and `cargo clippy` succeed.
- `cargo test` succeeds (existing tests use mock clients, so no behaviour
  change from their perspective).
- Manual: trigger a sync against a real Monobank token (or any reachable
  endpoint that returns a body) and confirm a `monobank request done` event
  appears with `endpoint`, `status`, `elapsed_ms`, and `body` fields, and that
  no `X-Token` value is present in the log output.
