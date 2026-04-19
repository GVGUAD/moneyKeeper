# OpenAPI Spec Design

**Date:** 2026-03-12
**Goal:** Add OpenAPI 3.0 documentation with Swagger UI to the MoneyKeeper API.

## Approach

Use `utoipa` with derive macros. Annotations live alongside the code, so the spec stays in sync as handlers and DTOs evolve. `utoipa-swagger-ui` serves Swagger UI directly from the app at `/swagger-ui`.

## Dependencies

Add to `Cargo.toml`:

```toml
utoipa = { version = "5", features = ["axum_extras", "chrono", "uuid", "decimal"] }
utoipa-swagger-ui = { version = "8", features = ["axum"] }
```

## New File: `src/api/openapi.rs`

Central registry that collects all paths and schemas:

```rust
use utoipa::{Modify, OpenApi, openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme}};
use crate::api::handlers;
use crate::api::dto::*;

#[derive(OpenApi)]
#[openapi(
    paths(
        handlers::accounts::create_account,
        handlers::accounts::list_accounts,
        handlers::accounts::get_account,
        handlers::accounts::update_account,
        handlers::accounts::delete_account,
        handlers::accounts::get_balance,
        handlers::transactions::create_transaction,
        handlers::transactions::list_transactions,
        handlers::transactions::get_transaction,
        handlers::transactions::delete_transaction,
        handlers::transactions::list_all_transactions,
        handlers::categories::create_category,
        handlers::categories::list_categories,
        handlers::categories::update_category,
        handlers::categories::delete_category,
        handlers::monobank::get_client_info,
        handlers::monobank::connect,
        handlers::monobank::list_connections,
        handlers::monobank::delete_connection,
        handlers::monobank::webhook,
    ),
    components(schemas(
        // Accounts
        CreateAccountRequest, UpdateAccountRequest, AccountDetailsDto,
        AccountResponse, BalanceResponse,
        // Transactions
        CreateTransactionRequest, TransactionDetailsDto, TransactionResponse,
        // Categories
        CreateCategoryRequest, UpdateCategoryRequest, CategoryResponse,
        // Monobank
        ConnectMonobankRequest, MonobankConnectionResponse,
        MonoAccountResponse, MonobankWebhookPayload, MonobankWebhookData,
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

## Changes to `src/api/dto.rs`

All DTOs are defined in a single file `src/api/dto.rs`. Changes:

Add a `use` import at the top:
```rust
use utoipa::{IntoParams, ToSchema};
```

Then add `#[derive(ToSchema)]` to every request and response struct/enum, and `#[derive(IntoParams)]` (instead of `ToSchema`) to `TxListQuery`.

`MonobankWebhookData` contains a field typed as `crate::domain::monobank::MonoStatementItem`, which is a domain struct that must not gain API-layer dependencies. To avoid adding `ToSchema` to the domain layer, annotate that field with `#[schema(value_type = Object)]`:

```rust
#[derive(Deserialize, ToSchema)]
pub struct MonobankWebhookData {
    pub account: String,
    #[serde(rename = "statementItem")]
    #[schema(value_type = Object)]
    pub statement_item: crate::domain::monobank::MonoStatementItem,
}
```

No logic changes.

## Handler files that need changes

The following four files each need `#[utoipa::path(...)]` annotations added to their public handler functions:

- `src/api/handlers/accounts.rs`
- `src/api/handlers/transactions.rs`
- `src/api/handlers/categories.rs`
- `src/api/handlers/monobank.rs`

Each public handler function gets a `#[utoipa::path(...)]` attribute. Pattern:

```rust
#[utoipa::path(
    post,
    path = "/accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 201, body = AccountResponse),
        (status = 400, description = "invalid input"),
        (status = 401, description = "unauthorized"),
    ),
    security(("bearer_auth" = []))
)]
pub async fn create_account(...) { ... }
```

**Handler → path mapping** (use these exact `path =` values in each annotation):

| Handler function | Method | `path` value |
|---|---|---|
| `accounts::create_account` | POST | `/accounts` |
| `accounts::list_accounts` | GET | `/accounts` |
| `accounts::get_account` | GET | `/accounts/{id}` |
| `accounts::update_account` | PUT | `/accounts/{id}` |
| `accounts::delete_account` | DELETE | `/accounts/{id}` |
| `accounts::get_balance` | GET | `/accounts/{id}/balance` |
| `transactions::create_transaction` | POST | `/accounts/{id}/transactions` |
| `transactions::list_transactions` | GET | `/accounts/{id}/transactions` |
| `transactions::get_transaction` | GET | `/transactions/{id}` |
| `transactions::delete_transaction` | DELETE | `/transactions/{id}` |
| `transactions::list_all_transactions` | GET | `/transactions` |
| `categories::create_category` | POST | `/categories` |
| `categories::list_categories` | GET | `/categories` |
| `categories::update_category` | PUT | `/categories/{id}` |
| `categories::delete_category` | DELETE | `/categories/{id}` |
| `monobank::get_client_info` | GET | `/monobank/client-info` |
| `monobank::connect` | POST | `/monobank/connect` |
| `monobank::list_connections` | GET | `/monobank/connections` |
| `monobank::delete_connection` | DELETE | `/monobank/connections/{id}` |
| `monobank::webhook` | POST | `/monobank/webhook` |

**Security rules:**
- All protected endpoints: `security(("bearer_auth" = []))`
- `GET /monobank/client-info`: also documents `X-Token` as a required header param
- `POST /monobank/webhook` and `GET /health`: no security

**Response status codes per handler:**

| Handler | Success | Error codes |
|---|---|---|
| `create_account` | 201 | 400, 401 |
| `list_accounts` | 200 | 401 |
| `get_account` | 200 | 401, 404 |
| `update_account` | 200 | 400, 401, 404 |
| `delete_account` | 204 | 401, 404 |
| `get_balance` | 200 | 401, 404 |
| `create_transaction` | 201 | 400, 401, 404 |
| `list_transactions` | 200 | 401, 404 |
| `get_transaction` | 200 | 401, 404 |
| `delete_transaction` | 204 | 401, 404 |
| `list_all_transactions` | 200 | 401 |
| `create_category` | 201 | 400, 401 |
| `list_categories` | 200 | 401 |
| `update_category` | 200 | 400, 401, 404 |
| `delete_category` | 204 | 401, 404 |
| `get_client_info` | 200 | 400, 401 |
| `connect` | 201 | 400, 401, 409 |
| `list_connections` | 200 | 401 |
| `delete_connection` | 204 | 401, 404 |
| `webhook` | 200 | 400 |

## Changes to `src/api/routes.rs`

Add imports at the top:

```rust
use axum::Json;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use crate::api::openapi::ApiDoc;
```

Add two public routes on the **outer** `Router::new()` chain (not the `protected` router), alongside `/health` and `/monobank/webhook`:

```rust
Router::new()
    .route("/health", get(|| async { (StatusCode::OK, "ok") }))
    .merge(protected)
    .route("/monobank/webhook", post(monobank::webhook))
    .route("/api-doc/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))  // new
    .merge(SwaggerUi::new("/swagger-ui").url("/api-doc/openapi.json", ApiDoc::openapi()))  // new
    .with_state(state)
```

## Changes to `src/api/mod.rs`

The current file declares: `dto`, `error`, `handlers`, `jwt`, `middleware`, `routes`, `state`. Add `openapi` to the list:

```rust
pub mod openapi;  // add alongside the existing module declarations
```

## Endpoint Inventory

### Public
- `GET /health`
- `POST /monobank/webhook`
- `GET /api-doc/openapi.json` *(new)*
- `GET /swagger-ui` *(new)*

### Protected (Bearer JWT)
- `POST /accounts` → 201 AccountResponse
- `GET /accounts` → 200 Vec\<AccountResponse\>
- `GET /accounts/{id}` → 200 AccountResponse
- `PUT /accounts/{id}` → 200 AccountResponse
- `DELETE /accounts/{id}` → 204
- `GET /accounts/{id}/balance` → 200 BalanceResponse
- `POST /accounts/{id}/transactions` → 201 TransactionResponse
- `GET /accounts/{id}/transactions` → 200 Vec\<TransactionResponse\>
- `GET /transactions` → 200 Vec\<TransactionResponse\>
- `GET /transactions/{id}` → 200 TransactionResponse
- `DELETE /transactions/{id}` → 204
- `POST /categories` → 201 CategoryResponse
- `GET /categories` → 200 Vec\<CategoryResponse\>
- `PUT /categories/{id}` → 200 CategoryResponse
- `DELETE /categories/{id}` → 204
- `GET /monobank/client-info` (+ X-Token header) → 200 Vec\<MonoAccountResponse\>
- `POST /monobank/connect` → 201 MonobankConnectionResponse
- `GET /monobank/connections` → 200 Vec\<MonobankConnectionResponse\>
- `DELETE /monobank/connections/{id}` → 204
