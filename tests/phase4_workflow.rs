use std::fs;
#[test]
fn phase_four_contexts_never_query_foreign_private_schemas() {
    for context in ["mail", "recurring", "reporting"] {
        for entry in walk(&format!("src/contexts/{context}")) {
            let source = fs::read_to_string(entry).unwrap();
            for foreign in ["ledger.", "banking.", "reference_data."] {
                assert!(!source.contains(&format!("FROM {foreign}")));
                assert!(!source.contains(&format!("JOIN {foreign}")));
            }
        }
    }
}
fn walk(root: &str) -> Vec<std::path::PathBuf> {
    let mut pending = vec![std::path::PathBuf::from(root)];
    let mut files = vec![];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path)
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path)
            }
        }
    }
    files
}
