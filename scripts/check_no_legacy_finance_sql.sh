#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repository_root"

legacy_tables='accounts|transactions|transfer_links|bank_connections|subscription_charges|email_connections|subscriptions|fx_rates|user_settings'
unqualified_sql="\\b(?:FROM|JOIN|INTO|UPDATE|DELETE[[:space:]]+FROM|ALTER[[:space:]]+TABLE|CREATE[[:space:]]+TABLE|REFERENCES)[[:space:]]+(?:\"?public\"?\\.)?\"?(?:${legacy_tables})\"?\\b"

if rg --pcre2 --line-number --ignore-case "$unqualified_sql" src --glob '*.rs' \
    || rg --pcre2 --line-number --ignore-case "$unqualified_sql" \
        src/infrastructure/migrations --glob '*.sql'; then
    echo "error: executable code or active migration SQL contains an unqualified retired finance table" >&2
    exit 1
fi

if rg --pcre2 --line-number \
    'contexts::[a-z_]+::(?:domain|application|infrastructure|api|build)(?:::|\\b)' \
    src/integration; then
    echo "error: integration code imports a context private layer" >&2
    exit 1
fi

echo "retired finance SQL and private-context import guards passed"
