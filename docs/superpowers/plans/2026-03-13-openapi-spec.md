# OpenAPI Spec Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OpenAPI 3.1.0 documentation with Swagger UI to the MoneyKeeper API using `utoipa` derive macros.

**Architecture:** Add `#[derive(ToSchema)]` to all DTOs in `dto.rs`, `#[utoipa::path(...)]` annotations to each handler function, a central `ApiDoc` registry in a new `openapi.rs`, and two new public routes (`/api-doc/openapi.json`, `/swagger-ui`) on the outer router.

**Tech Stack:** Rust, Axum 0.8, `utoipa` v5, `utoipa-swagger-ui` v8

---

## File Map

| File | Change |
|---|---|
| `Cargo.toml` | Add `utoipa` and `utoipa-swagger-ui` dependencies |
| `src/api/dto.rs` | Add `ToSchema`/`IntoParams` derives + `schema` annotation on `MonobankWebhookData` |
| `src/api/handlers/accounts.rs` | Add `#[utoipa::path]` to 6 handlers |
| `src/api/handlers/transactions.rs` | Add `#[utoipa::path]` to 5 handlers |
| `src/api/handlers/categories.rs` | Add `#[utoipa::path]` to 4 handlers |
| `src/api/handlers/monobank.rs` | Add `#[utoipa::path]` to 5 handlers |
| `src/api/openapi.rs` | **Create** — `ApiDoc` struct + `SecurityAddon` (created after handlers are annotated) |
| `src/api/mod.rs` | Add `pub mod openapi;` |
| `src/api/routes.rs` | Add imports + two new public routes |

> **Ordering note:** `openapi.rs` is created in Task 7 (after all handler annotations), because the `#[openapi(paths(...))]` macro requires each listed function to already carry a `#[utoipa::path]` attribute. Creating it earlier would break compilation.

---

## Chunk 1: Foundations (deps + DTOs)

### Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add utoipa crates to Cargo.toml**

In `Cargo.toml`, add after the existing `axum` dependency:

```toml
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid", "decimal"] }
utoipa-swagger-ui = { version = "8", features = ["axum"] }
```

- [ ] **Step 2: Fetch and verify compilation**

```bash
cargo build 2>&1 | head -30
```

Expected: new crates download and compile successfully. If version conflicts appear, run `cargo tree` to inspect and adjust.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add utoipa and utoipa-swagger-ui dependencies"
```

---

### Task 2: Annotate DTOs

**Files:**
- Modify: `src/api/dto.rs`

- [ ] **Step 1: Add utoipa import**

At the top of `src/api/dto.rs`, add after the existing `use` statements:

```rust
use utoipa::{IntoParams, ToSchema};
```

- [ ] **Step 2: Add ToSchema to auth DTOs**

> Note: auth endpoints are served by Supabase externally and are not exposed through this API's router. Adding `ToSchema` here is harmless and keeps the derives consistent across all DTOs, but these schemas will not appear in `ApiDoc`.

```rust
#[derive(Deserialize, ToSchema)]
pub struct RegisterRequest { ... }

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest { ... }

#[derive(Deserialize, ToSchema)]
pub struct RefreshRequest { ... }

#[derive(Deserialize, ToSchema)]
pub struct LogoutRequest { ... }

#[derive(Serialize, ToSchema)]
pub struct AuthResponse { ... }
```

- [ ] **Step 3: Add ToSchema to account DTOs**

```rust
#[derive(Deserialize, ToSchema)]
pub struct CreateAccountRequest { ... }

#[derive(Deserialize, ToSchema)]
pub struct UpdateAccountRequest { ... }

#[derive(Serialize, Deserialize, Clone, Debug, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AccountDetailsDto { ... }

#[derive(Serialize, ToSchema)]
pub struct AccountResponse { ... }

#[derive(Serialize, ToSchema)]
pub struct BalanceResponse { ... }
```

- [ ] **Step 4: Add ToSchema to transaction DTOs**

```rust
#[derive(Deserialize, ToSchema)]
pub struct CreateTransactionRequest { ... }

#[derive(Serialize, Deserialize, Clone, Debug, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TransactionDetailsDto { ... }

#[derive(Serialize, ToSchema)]
pub struct TransactionResponse { ... }
```

- [ ] **Step 5: Add ToSchema/IntoParams to category and pagination DTOs**

```rust
#[derive(Deserialize, ToSchema)]
pub struct CreateCategoryRequest { ... }

#[derive(Deserialize, ToSchema)]
pub struct UpdateCategoryRequest { ... }
```

`UpdateCategoryRequest` has a field `color: Option<Option<String>>` (used to distinguish a missing field from an explicit `null`). `ToSchema` cannot represent `Option<Option<T>>` in JSON Schema. Add `#[schema(value_type = Option<String>)]` on that field:

```rust
#[derive(Deserialize, ToSchema)]
pub struct UpdateCategoryRequest {
    pub name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_field")]
    #[schema(value_type = Option<String>)]
    pub color: Option<Option<String>>,
}
```

Continue with:

```rust
#[derive(Serialize, ToSchema)]
pub struct CategoryResponse { ... }

// TxListQuery gets IntoParams (it's a query param struct, not a body schema)
#[derive(Deserialize, IntoParams)]
pub struct TxListQuery { ... }
```

- [ ] **Step 6: Add ToSchema to monobank DTOs, with domain type workaround**

`MonobankWebhookData.statement_item` is typed as `crate::domain::monobank::MonoStatementItem`, a domain struct that must not gain API-layer dependencies (`ToSchema` is an API concern). Use `#[schema(value_type = Object)]` on that field to tell utoipa to treat it as a generic JSON object in the spec:

```rust
#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct ConnectMonobankRequest { ... }

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct MonobankConnectionResponse { ... }

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct MonoAccountResponse { ... }

#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct MonobankWebhookPayload { ... }

#[derive(Debug, serde::Deserialize, ToSchema)]
pub struct MonobankWebhookData {
    pub account: String,
    #[serde(rename = "statementItem")]
    #[schema(value_type = Object)]
    pub statement_item: crate::domain::monobank::MonoStatementItem,
}
```

- [ ] **Step 7: Verify compilation**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors. Three potential issues:
- **`rust_decimal::Decimal` fields:** The project uses `rust_decimal` with the `serde-with-str` feature. If `utoipa`'s `decimal` feature conflicts, annotate individual `Decimal` fields with `#[schema(value_type = String)]` as a fallback.
- **`Option<Option<String>>` in `UpdateCategoryRequest`:** Handled above in Step 5 with `#[schema(value_type = Option<String>)]`.
- **`TxListQuery` serde defaults and `IntoParams`:** `TxListQuery.limit` uses `#[serde(default = "default_limit")]` (a function path). If utoipa's `IntoParams` macro rejects this form, add an explicit `#[param(default = 50)]` attribute on the `limit` field and `#[param(default = 0)]` on `offset`.

- [ ] **Step 8: Commit**

```bash
git add src/api/dto.rs
git commit -m "feat: add ToSchema/IntoParams derives to all DTOs"
```

---

## Chunk 2: Handler Annotations

### Task 3: Annotate accounts handlers

**Files:**
- Modify: `src/api/handlers/accounts.rs`

Add the `#[utoipa::path(...)]` attribute immediately before each `pub async fn`.

- [ ] **Step 1: Annotate `create_account`**

```rust
#[utoipa::path(
    post,
    path = "/accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 201, description = "account created", body = AccountResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_account(
```

- [ ] **Step 2: Annotate `list_accounts`**

```rust
#[utoipa::path(
    get,
    path = "/accounts",
    responses(
        (status = 200, description = "list of accounts", body = Vec<AccountResponse>),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_accounts(
```

- [ ] **Step 3: Annotate `get_account`**

```rust
#[utoipa::path(
    get,
    path = "/accounts/{id}",
    params(
        ("id" = Uuid, Path, description = "Account ID")
    ),
    responses(
        (status = 200, description = "account found", body = AccountResponse),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "account not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_account(
```

- [ ] **Step 4: Annotate `update_account`**

```rust
#[utoipa::path(
    put,
    path = "/accounts/{id}",
    params(
        ("id" = Uuid, Path, description = "Account ID")
    ),
    request_body = UpdateAccountRequest,
    responses(
        (status = 200, description = "account updated", body = AccountResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "account not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_account(
```

- [ ] **Step 5: Annotate `delete_account`**

```rust
#[utoipa::path(
    delete,
    path = "/accounts/{id}",
    params(
        ("id" = Uuid, Path, description = "Account ID")
    ),
    responses(
        (status = 204, description = "account deleted"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "account not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_account(
```

- [ ] **Step 6: Annotate `get_balance`**

```rust
#[utoipa::path(
    get,
    path = "/accounts/{id}/balance",
    params(
        ("id" = Uuid, Path, description = "Account ID")
    ),
    responses(
        (status = 200, description = "current balance", body = BalanceResponse),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "account not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_balance(
```

- [ ] **Step 7: Verify compilation**

```bash
cargo build 2>&1 | grep -E "^error"
```

- [ ] **Step 8: Commit**

```bash
git add src/api/handlers/accounts.rs
git commit -m "feat: add utoipa path annotations to accounts handlers"
```

---

### Task 4: Annotate transactions handlers

**Files:**
- Modify: `src/api/handlers/transactions.rs`

- [ ] **Step 1: Annotate `create_transaction`**

```rust
#[utoipa::path(
    post,
    path = "/accounts/{id}/transactions",
    params(
        ("id" = Uuid, Path, description = "Account ID")
    ),
    request_body = CreateTransactionRequest,
    responses(
        (status = 201, description = "transaction created", body = TransactionResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "account not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_transaction(
```

- [ ] **Step 2: Annotate `list_transactions`**

`TxListQuery` implements `IntoParams` (not `ToSchema`), so it is referenced via `params(TxListQuery)` rather than as a `request_body`:

```rust
#[utoipa::path(
    get,
    path = "/accounts/{id}/transactions",
    params(
        ("id" = Uuid, Path, description = "Account ID"),
        TxListQuery,
    ),
    responses(
        (status = 200, description = "list of transactions", body = Vec<TransactionResponse>),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "account not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_transactions(
```

- [ ] **Step 3: Annotate `get_transaction`**

```rust
#[utoipa::path(
    get,
    path = "/transactions/{id}",
    params(
        ("id" = Uuid, Path, description = "Transaction ID")
    ),
    responses(
        (status = 200, description = "transaction found", body = TransactionResponse),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "transaction not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_transaction(
```

- [ ] **Step 4: Annotate `delete_transaction`**

```rust
#[utoipa::path(
    delete,
    path = "/transactions/{id}",
    params(
        ("id" = Uuid, Path, description = "Transaction ID")
    ),
    responses(
        (status = 204, description = "transaction deleted"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "transaction not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_transaction(
```

- [ ] **Step 5: Annotate `list_all_transactions`**

> Note: This handler accepts `TxListQuery` but only uses `limit` and `offset` — it silently ignores `kind` and `category_id`. To avoid advertising non-functional filters in the spec, document only the two fields that actually work using inline params instead of `params(TxListQuery)`:

```rust
#[utoipa::path(
    get,
    path = "/transactions",
    params(
        ("limit" = i64, Query, description = "Maximum results to return (default: 50)"),
        ("offset" = i64, Query, description = "Number of results to skip (default: 0)"),
    ),
    responses(
        (status = 200, description = "list of all transactions", body = Vec<TransactionResponse>),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_all_transactions(
```

- [ ] **Step 6: Verify compilation**

```bash
cargo build 2>&1 | grep -E "^error"
```

- [ ] **Step 7: Commit**

```bash
git add src/api/handlers/transactions.rs
git commit -m "feat: add utoipa path annotations to transactions handlers"
```

---

### Task 5: Annotate categories handlers

**Files:**
- Modify: `src/api/handlers/categories.rs`

- [ ] **Step 1: Annotate `create_category`**

```rust
#[utoipa::path(
    post,
    path = "/categories",
    request_body = CreateCategoryRequest,
    responses(
        (status = 201, description = "category created", body = CategoryResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_category(
```

- [ ] **Step 2: Annotate `list_categories`**

```rust
#[utoipa::path(
    get,
    path = "/categories",
    responses(
        (status = 200, description = "list of categories", body = Vec<CategoryResponse>),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_categories(
```

- [ ] **Step 3: Annotate `update_category`**

```rust
#[utoipa::path(
    put,
    path = "/categories/{id}",
    params(
        ("id" = Uuid, Path, description = "Category ID")
    ),
    request_body = UpdateCategoryRequest,
    responses(
        (status = 200, description = "category updated", body = CategoryResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "category not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn update_category(
```

- [ ] **Step 4: Annotate `delete_category`**

```rust
#[utoipa::path(
    delete,
    path = "/categories/{id}",
    params(
        ("id" = Uuid, Path, description = "Category ID")
    ),
    responses(
        (status = 204, description = "category deleted"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "category not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_category(
```

- [ ] **Step 5: Verify compilation**

```bash
cargo build 2>&1 | grep -E "^error"
```

- [ ] **Step 6: Commit**

```bash
git add src/api/handlers/categories.rs
git commit -m "feat: add utoipa path annotations to categories handlers"
```

---

### Task 6: Annotate monobank handlers

**Files:**
- Modify: `src/api/handlers/monobank.rs`

- [ ] **Step 1: Annotate `get_client_info`**

This endpoint requires an `X-Token` header in addition to Bearer auth:

```rust
#[utoipa::path(
    get,
    path = "/monobank/client-info",
    params(
        ("x-token" = String, Header, description = "Monobank API token")
    ),
    responses(
        (status = 200, description = "list of monobank accounts", body = Vec<MonoAccountResponse>),
        (status = 400, description = "missing X-Token header"),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn get_client_info(
```

- [ ] **Step 2: Annotate `connect`**

```rust
#[utoipa::path(
    post,
    path = "/monobank/connect",
    request_body = ConnectMonobankRequest,
    responses(
        (status = 201, description = "connection created", body = MonobankConnectionResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
        (status = 409, description = "connection already exists"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn connect(
```

- [ ] **Step 3: Annotate `list_connections`**

```rust
#[utoipa::path(
    get,
    path = "/monobank/connections",
    responses(
        (status = 200, description = "list of monobank connections", body = Vec<MonobankConnectionResponse>),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn list_connections(
```

- [ ] **Step 4: Annotate `delete_connection`**

```rust
#[utoipa::path(
    delete,
    path = "/monobank/connections/{id}",
    params(
        ("id" = Uuid, Path, description = "Connection ID")
    ),
    responses(
        (status = 204, description = "connection deleted"),
        (status = 401, description = "unauthorized"),
        (status = 404, description = "connection not found"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn delete_connection(
```

- [ ] **Step 5: Annotate `webhook`**

This endpoint is public — no security requirement:

```rust
#[utoipa::path(
    post,
    path = "/monobank/webhook",
    request_body = MonobankWebhookPayload,
    responses(
        (status = 200, description = "webhook processed"),
        (status = 400, description = "invalid payload"),
    ),
)]
pub async fn webhook(
```

- [ ] **Step 6: Verify compilation**

```bash
cargo build 2>&1 | grep -E "^error"
```

- [ ] **Step 7: Commit**

```bash
git add src/api/handlers/monobank.rs
git commit -m "feat: add utoipa path annotations to monobank handlers"
```

---

## Chunk 3: ApiDoc + Route Wiring

### Task 7: Create `src/api/openapi.rs` and register the module

> All handler functions now carry `#[utoipa::path]` annotations, so `openapi.rs` can reference them without causing compile errors.

**Files:**
- Create: `src/api/openapi.rs`
- Modify: `src/api/mod.rs`

- [ ] **Step 1: Create `src/api/openapi.rs`**

> `TxListQuery` is not imported here — it implements `IntoParams` and is referenced directly within each handler's annotation; `openapi.rs` only needs the handler module paths.

```rust
use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

use crate::api::dto::{
    AccountDetailsDto, AccountResponse, BalanceResponse,
    CategoryResponse, ConnectMonobankRequest, CreateAccountRequest,
    CreateCategoryRequest, CreateTransactionRequest, MonoAccountResponse,
    MonobankConnectionResponse, MonobankWebhookData, MonobankWebhookPayload,
    TransactionDetailsDto, TransactionResponse, UpdateAccountRequest,
    UpdateCategoryRequest,
};
use crate::api::handlers::{accounts, categories, monobank, transactions};

#[derive(OpenApi)]
#[openapi(
    paths(
        accounts::create_account,
        accounts::list_accounts,
        accounts::get_account,
        accounts::update_account,
        accounts::delete_account,
        accounts::get_balance,
        transactions::create_transaction,
        transactions::list_transactions,
        transactions::get_transaction,
        transactions::delete_transaction,
        transactions::list_all_transactions,
        categories::create_category,
        categories::list_categories,
        categories::update_category,
        categories::delete_category,
        monobank::get_client_info,
        monobank::connect,
        monobank::list_connections,
        monobank::delete_connection,
        monobank::webhook,
    ),
    components(schemas(
        CreateAccountRequest,
        UpdateAccountRequest,
        AccountDetailsDto,
        AccountResponse,
        BalanceResponse,
        CreateTransactionRequest,
        TransactionDetailsDto,
        TransactionResponse,
        CreateCategoryRequest,
        UpdateCategoryRequest,
        CategoryResponse,
        ConnectMonobankRequest,
        MonobankConnectionResponse,
        MonoAccountResponse,
        MonobankWebhookPayload,
        MonobankWebhookData,
    )),
    modifiers(&SecurityAddon),
    info(title = "MoneyKeeper API", version = "0.1.0"),
)]
pub struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer_auth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}
```

- [ ] **Step 2: Register the module in `src/api/mod.rs`**

The current file contains 7 `pub mod` declarations. Add `openapi` in alphabetical order between `middleware` and `routes`:

```rust
pub mod dto;
pub mod error;
pub mod handlers;
pub mod jwt;
pub mod middleware;
pub mod openapi;   // add this line
pub mod routes;
pub mod state;
```

- [ ] **Step 3: Verify compilation**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/api/openapi.rs src/api/mod.rs
git commit -m "feat: add ApiDoc registry and openapi module"
```

---

### Task 8: Wire Swagger UI into routes

**Files:**
- Modify: `src/api/routes.rs`

- [ ] **Step 1: Add imports to `src/api/routes.rs`**

Add after the existing `use` statements at the top of the file:

```rust
use axum::Json;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use crate::api::openapi::ApiDoc;
```

- [ ] **Step 2: Add two new public routes to the outer router**

The outer `Router::new()` block currently ends with:

```rust
Router::new()
    .route("/health", get(|| async { (StatusCode::OK, "ok") }))
    .merge(protected)
    .route("/monobank/webhook", post(monobank::webhook))
    .with_state(state)
```

Update it to:

```rust
Router::new()
    .route("/health", get(|| async { (StatusCode::OK, "ok") }))
    .merge(protected)
    .route("/monobank/webhook", post(monobank::webhook))
    .route("/api-doc/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
    .merge(SwaggerUi::new("/swagger-ui").url("/api-doc/openapi.json", ApiDoc::openapi()))
    .with_state(state)
```

- [ ] **Step 3: Verify full build is clean**

```bash
cargo build 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 4: Run clippy**

```bash
cargo clippy 2>&1 | grep -E "^error"
```

Expected: no errors.

- [ ] **Step 5: Commit**

```bash
git add src/api/routes.rs
git commit -m "feat: serve OpenAPI JSON and Swagger UI at /api-doc/openapi.json and /swagger-ui"
```

---

### Task 9: Smoke test

- [ ] **Step 1: Run the server**

```bash
cargo run
```

Expected: server starts. Check `.env` for the port (typically `PORT=3000` or `8080`).

- [ ] **Step 2: Verify OpenAPI JSON**

```bash
curl -s http://localhost:<PORT>/api-doc/openapi.json | python3 -m json.tool | head -30
```

Expected: valid JSON starting with `{"openapi":"3.1.0","info":{"title":"MoneyKeeper API",...}}` and a `paths` object listing endpoints.

- [ ] **Step 3: Open Swagger UI**

Navigate to `http://localhost:<PORT>/swagger-ui/` in a browser.

Expected: Swagger UI loads showing all 20 endpoints. Protected endpoints show a padlock icon. Schemas for request/response bodies are visible in the Models section.

- [ ] **Step 4: Run tests to confirm no regressions**

```bash
cargo test 2>&1 | tail -20
```

Expected: all existing tests pass. The handler annotation changes are purely additive (no logic changes), so no test should be affected.
