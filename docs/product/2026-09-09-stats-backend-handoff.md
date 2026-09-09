# Stats backend implementation and Android handoff

Implemented on `feature/stats-backend-analytics`, based on `cc2ae44`. Release revision `b42a0bb` was deployed to Fly on 2026-09-09. The source implementation plan is `/Users/volodymyr/AndroidStudioProjects/MoneyKeeper/docs/product/2026-09-08-stats-backend-plan.md`.

## Contract and fixtures

- Authoritative contract: [`static/openapi.json`](../../static/openapi.json).
- Complete August UAH aggregate: [`analytics-acceptance.json`](analytics-acceptance.json).
- Corresponding expense list: [`analytics-transactions-acceptance.json`](analytics-transactions-acceptance.json).
- Authenticated routes: `GET /reports/analytics` and `GET /reports/analytics/transactions`.
- Existing seven projection report routes, Activity queries, and workers retain their existing behavior.

The fixtures use UTC calendar boundaries and synthetic identifiers. August income is 1200, expenses 145, net 1055, purchases 165 and expense credits 20. The expense list has four contributing journals and a signed contribution of -145. July expenses are 100. Each response has complete metadata, exact decimal strings, and zero-filled calendar buckets.

The All transaction list retains a journal with equal nonzero income and expense components. Spending breakdown rows include only expense-bearing journals; mixed journals retain both components in those rows. Breakdown expense totals and counts partition the root expenses, purchases, credits and expense count.

## Implementation

Ledger owns one reusable effective-entry SQL builder for grouped aggregates and keyset-paged contributing journals. It excludes originals with any reversal or replacement regardless of the later occurrence date, retains surviving ordinary correction-source replacements, groups postings per journal in the requested currency, and preserves signed income and expenses. Explicit category selection distinguishes an empty ID set from All.

Reporting obtains currency definitions including disabled currencies, freezes the current Classification taxonomy, resolves subtree/direct/Uncategorized filters, composes breakdowns, and rechecks taxonomy consistency. A changed version or missing assigned category discards the full read and retries once; a second inconsistency returns 409. List consistency checks include categories in the full matching summary, even beyond the returned page.

Calendar boundaries use bound PostgreSQL timezone lookups and local calendar arithmetic. Aggregate intervals share one read-only repeatable-read snapshot. Page and range-wide summary share another such snapshot. `as_of` is a calculation timestamp, not a durable paging snapshot token.

No new dependency, migration, projection reset, or backfill is required. Analytics routing is composed from existing facades in the authenticated API; projection-worker construction remains independent.

## Query-plan review

PostgreSQL 16 testcontainer, 10,000 synthetic journals / 20,000 postings spread over 365 dates, 867 journals in the requested month, `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`:

| Query shape | Execution | Shared buffer hits | Finding |
| --- | ---: | ---: | --- |
| Combined reversal/replacement OR, range EXISTS | 548.881 ms | 12,850 | Nested anti join rejected 8,670,000 pairs. |
| Separate reversal/replacement anti joins, explicit bounded range union | 4.045 ms | 568 | Existing `journal_user_activity_order` bitmap scan and hash anti joins. |

These are individual synthetic local measurements, not production latency promises. The rewrite removes the observed quadratic relation check. A replacement-side scan and posting scan remain in this representative plan; neither justified an added index at this volume. The same bounded fact builder serves aggregate, summary and list queries. Recheck production diagnostics as history grows.

Reproduce the plan and snapshot isolation check with:

```sh
SQLX_OFFLINE=true cargo test --lib analytics_query_plan_and_snapshot_consistency -- --nocapture
```

## Verification

Tests use isolated disposable PostgreSQL 16 containers through the repository's test database helpers, never production. Coverage includes the acceptance fixture, signed components and decimal precision, mixed journals, late reversals and repeated replacements, empty category sets, overlapping intervals, 205-row summaries and cursor ties, archive/reassignment/direct-parent history, comparison-only and zero-net groups, foreign/missing categories, taxonomy retry/conflict, invalid query parameters, currency history, DST and calendar thresholds, report/list mutation parity, and preserved audit entries.

```sh
SQLX_OFFLINE=true cargo test --test analytics_queries --test analytics_api \
  --test ledger_api --test context_boundaries --test openapi --test reporting_api --test api
SQLX_OFFLINE=true cargo test --lib analytics
cargo fmt --check
git diff --check
```

Verification passed: 38 integration/contract tests across the listed targets, two analytics module tests, `cargo fmt --check`, and `git diff --check`. The final analytics-only rerun passed all ten integration tests.

The taxonomy retry state machine is unit-tested deterministically; missing-assignment retry/conflict is exercised through both real HTTP endpoints. Snapshot repeatability is tested across a committed concurrent database update using the same transaction helper as the live queries.

## Deployment status

Deployed to `https://moneykeeper.fly.dev` on 2026-09-09 after explicit user authorization. The release includes the analytics implementation and the banking/runtime/documentation changes the user subsequently requested to commit together.

Release evidence:

- Source revision: `b42a0bb` (includes the Clippy cleanup following `41bbe4b`).
- Image: `registry.fly.io/moneykeeper@sha256:4fb37855ffd33f8a6da95107d29a91bb600f48d25cedb2f4368d9c374aded608`.
- Fly machine: `683d526f391238`, version 32, started in Frankfurt.
- Full `cargo test --all-targets`, Clippy with warnings denied, formatting, and retired-SQL guards passed before rollout.
- Fly rolling deployment, machine smoke checks, and DNS verification passed.
- Public `/health/live` and `/health/ready` returned 200 with `live` and `ready` respectively.
- Both analytics routes returned the expected JSON 401 envelope without authentication.
- Authenticated production report reads were not performed because no user access token was supplied. Native-currency data checks remain required before Android release.

Remaining Android release checks:

1. Use deployed revision `b42a0bb` and the base URL above for integration.
2. With an authorized test user, smoke-test both analytics endpoints in UAH and another registered currency, plus known empty ranges, invalid requests and unauthenticated requests.
3. Verify `/transactions/summary` category-subtree behavior independently; Activity semantics remain distinct from Stats.
4. Record route latency, sanitized error rate and query diagnostics through existing observability. Do not log descriptions, amounts, tokens or complete query strings.
5. Give Android the deployed revision, base URL and this contract/fixtures. Enable the Android release only after those smoke tests pass. Missing endpoints after rollback must surface as analytics unavailable.
