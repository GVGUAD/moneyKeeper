use std::fs;
use std::path::{Path, PathBuf};

const CONTEXTS: &[&str] = &[
    "ledger",
    "banking",
    "mail",
    "recurring",
    "reporting",
    "sharing",
    "loans",
    "portfolio",
    "reference_data",
    "classification",
    "preferences",
];

#[test]
fn every_context_has_a_public_boundary() {
    for context in CONTEXTS {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/contexts")
            .join(context);
        assert!(root.join("mod.rs").is_file(), "missing {context}/mod.rs");
        assert!(
            root.join("public.rs").is_file(),
            "missing {context}/public.rs"
        );
    }
}

#[test]
fn contexts_cannot_import_private_layers_or_query_foreign_schemas() {
    let contexts_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contexts");
    for context in CONTEXTS {
        let root = contexts_root.join(context);
        for file in rust_files(&root) {
            assert_context_source_isolated(context, &file, &fs::read_to_string(&file).unwrap());
        }
    }
}

#[test]
fn context_http_adapters_contain_no_sql_or_sqlx() {
    let contexts_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contexts");
    for context in CONTEXTS {
        let api_root = contexts_root.join(context).join("api");
        if !api_root.exists() {
            continue;
        }
        for file in rust_files(&api_root) {
            let source = fs::read_to_string(&file).unwrap();
            let sql_literal = rust_string_literals(&source).find(|sql| looks_like_sql(sql));
            assert!(
                !source.contains("sqlx::") && sql_literal.is_none(),
                "{} contains persistence logic: {sql_literal:?}",
                file.display(),
            );
        }
    }
}

#[test]
fn process_managers_use_only_public_context_contracts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/integration/process_managers");
    if !root.exists() {
        return;
    }
    for file in rust_files(&root) {
        let source = fs::read_to_string(&file).unwrap();
        if let Some(reason) = process_manager_violation(&source) {
            panic!(
                "{} violates process-manager isolation: {reason}",
                file.display()
            );
        }
    }
}

#[test]
fn checker_rejects_deliberate_context_and_process_manager_violations() {
    let context_violation = "use crate::contexts::ledger::infrastructure::PgLedger;\n\
                             const SQL: &str = \"SELECT * FROM banking.resources\";";
    assert!(context_source_violation("recurring", context_violation).is_some());
    let grouped_import = "use crate::contexts::ledger::{public::LedgerQueries, domain::Journal};";
    assert!(context_source_violation("recurring", grouped_import).is_some());
    let grouped_root = "use crate::contexts::{ledger::public::LedgerQueries, reporting};";
    assert!(context_source_violation("recurring", grouped_root).is_some());
    let aliased_root = "use crate::contexts::ledger as ledger_context;";
    assert!(context_source_violation("recurring", aliased_root).is_some());
    let leaked_root = "use crate::contexts::classification::PgCategoryCatalog;";
    assert!(context_source_violation("recurring", leaked_root).is_some());
    let multiline_sql = "const SQL: &str = \"SELECT * FROM\n reporting.balances\";";
    assert!(context_source_violation("recurring", multiline_sql).is_some());
    for sql in [
        "TRUNCATE reporting.balances",
        "SELECT * FROM ONLY reporting.balances",
        "SELECT reporting.rebuild_balances()",
        "SELECT * FROM \"reporting\".\"balances\"",
    ] {
        assert!(
            context_source_violation("recurring", &format!("const SQL: &str = r#\"{sql}\"#;"))
                .is_some()
        );
    }

    let process_violation = "use crate::contexts::ledger::domain::Journal;\n\
                             const SQL: &str = \"UPDATE ledger.journals SET x = 1\";";
    assert!(process_manager_violation(process_violation).is_some());
}

fn assert_context_source_isolated(context: &str, file: &Path, source: &str) {
    if let Some(reason) = context_source_violation(context, source) {
        panic!("{} violates context isolation: {reason}", file.display());
    }
}

fn context_source_violation(context: &str, source: &str) -> Option<String> {
    for other in CONTEXTS.iter().copied().filter(|other| *other != context) {
        if let Some(private) = private_context_import(source, other) {
            return Some(format!("imports crate::contexts::{other}::{private}"));
        }
        if contains_schema_qualified_sql(source, other) {
            return Some(format!("contains SQL for schema {other}"));
        }
    }
    None
}

fn process_manager_violation(source: &str) -> Option<String> {
    for context in CONTEXTS {
        if let Some(private) = private_context_import(source, context) {
            return Some(format!("imports crate::contexts::{context}::{private}"));
        }
        if contains_schema_qualified_sql(source, context) {
            return Some(format!("contains SQL for schema {context}"));
        }
    }
    if source.contains("downcast_ref") || source.contains("downcast_mut") {
        return Some("downcasts to an implementation detail".to_owned());
    }
    None
}

fn private_context_import(source: &str, context: &str) -> Option<&'static str> {
    let prefix = format!("crate::contexts::{context}");
    for statement in source.split(';') {
        if statement.contains("crate::contexts::{")
            || statement.contains("use crate::contexts as ")
            || statement.contains("use crate::contexts\n")
        {
            return Some("non-public grouped or aliased root");
        }
        let mut remainder = statement;
        while let Some((_, rest)) = remainder.split_once(&prefix) {
            let rest = rest.trim_start();
            if !rest.starts_with("::public::")
                && !rest.starts_with("::public::{")
                && !rest.starts_with("::public as ")
                && rest != "::public"
            {
                return Some("non-public root");
            }
            remainder = &rest["::public".len()..];
        }
    }
    None
}

fn contains_schema_qualified_sql(source: &str, schema: &str) -> bool {
    // Context/process-manager Rust has no foreign-SQL exception. At the schema
    // level, the sole allowed dependency is a one-way composite foreign key to
    // immutable shared/reference identifiers; it belongs in migrations and
    // never grants a context permission to query or write the foreign schema.
    let qualified = format!("{schema}.");
    let quoted = format!("\"{schema}\".");
    rust_string_literals(source).any(|literal| {
        let literal = literal.to_ascii_lowercase();
        looks_like_sql(&literal) && (literal.contains(&qualified) || literal.contains(&quoted))
    })
}

fn looks_like_sql(literal: &str) -> bool {
    literal
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| {
            [
                "select",
                "insert",
                "update",
                "delete",
                "from",
                "join",
                "truncate",
                "create",
                "alter",
                "drop",
                "references",
                "with",
            ]
            .iter()
            .any(|keyword| word.eq_ignore_ascii_case(keyword))
        })
}

fn rust_string_literals(source: &str) -> impl Iterator<Item = String> + '_ {
    let bytes = source.as_bytes();
    let mut index = 0;
    std::iter::from_fn(move || {
        while index < bytes.len() {
            if bytes[index] == b'r' {
                let start = index;
                index += 1;
                let mut hashes = 0;
                while index < bytes.len() && bytes[index] == b'#' {
                    hashes += 1;
                    index += 1;
                }
                if index < bytes.len() && bytes[index] == b'"' {
                    index += 1;
                    let content_start = index;
                    while index < bytes.len() {
                        if bytes[index] == b'"'
                            && bytes
                                .get(index + 1..index + 1 + hashes)
                                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
                        {
                            let value = source[content_start..index].to_owned();
                            index += 1 + hashes;
                            return Some(value);
                        }
                        index += 1;
                    }
                }
                index = start + 1;
            } else if bytes[index] == b'"' {
                index += 1;
                let content_start = index;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == b'"' {
                        let value = source[content_start..index].to_owned();
                        index += 1;
                        return Some(value);
                    } else {
                        index += 1;
                    }
                }
            } else {
                index += 1;
            }
        }
        None
    })
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
}
