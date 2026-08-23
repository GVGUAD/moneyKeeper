use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

const FROZEN_LEGACY_MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_accounts.sql",
        "bd93fd323360cd7e25ddda516c0260942b1d8f8887354bca45b2c025bd64435a",
    ),
    (
        "0002_transactions.sql",
        "e63e0e0f3a121f31e0d76f9b2ebae40febaf2312674d9ea3c948fe56b9b38c82",
    ),
    (
        "0003_monobank.sql",
        "859227f0e2f3d525aa73dd88fb2ee1762f7361cedd6466fd3ac884ab2985d71b",
    ),
    (
        "0004_bank_connections.sql",
        "d70bc314015e9683b2ba2b6a00e9680faf93885b8b9be3d106792146af16fa70",
    ),
    (
        "0005_account_balance.sql",
        "81f17ed96ad04d0b586c085c1ef702f5b62266bb1b47ef928f09eacf7d92fe7e",
    ),
    (
        "0006_fx_rates.sql",
        "009c75b63d4d7ed5e018e22cc92a7418c16fb0cf3dbc186a5c8a98b294adea24",
    ),
    (
        "0007_user_settings.sql",
        "95404b5106c1d7766a86a49d74872d72adee7432d0b5ad205b2536da47b2486e",
    ),
    (
        "0008_transaction_external_balance.sql",
        "a537c04b200b9fdccec3b6989ecc0b4bd22bdcbc30961c1e94cad908072a3b5f",
    ),
    (
        "0009_email_connections.sql",
        "5d5c955c7a19e898970eec87464ebf6cdf90d76e4b570439c05a2b0365f93de9",
    ),
    (
        "0010_subscriptions.sql",
        "2687da8c2779fd53ba2c3fbf7a513e5ba8ecddef0d86256f12ef135ddc943b36",
    ),
    (
        "0011_subscription_charges.sql",
        "264ccda3e8d9e15b19f9e52f2d6da524402226aaab8238771845bb497d9d47cb",
    ),
    (
        "0012_subscription_integrity.sql",
        "32dd6f95baf73280080c2beeb1f4507939c13cd98d4b8ec9afc13a0151dd5bfe",
    ),
    (
        "0013_credentials_oauth_security.sql",
        "6c41bfb8547e90024b0b512d9a9b6721d608dd7fe493ea169484721f79c51b23",
    ),
    (
        "0014_email_sync_reliability.sql",
        "1886dd9f26946fb61bc941d384f50696d1345331c5ad48ba7bdf8858c2c2bff5",
    ),
    (
        "0015_email_credential_fencing.sql",
        "6ac72b46761ddd6b564b8802c9791a45c7e13369ac4018095443c05be9f6e41d",
    ),
    (
        "0016_subscription_integrity_backfill.sql",
        "ae867a668dec2c21b1c62ec289ec0fb88d8b794f9bd87de98cb15edc07188907",
    ),
    (
        "0017_subscription_integrity_validate.sql",
        "e95c7f98c1b789f8584923fb342fce2045737eabf1185874af471bb14d85acfd",
    ),
    (
        "0018_subscription_transaction_unique_index.sql",
        "7f0545c0bdbe9609b5d46c75b71140a9ca7d6f676cc618fa4402f6ab9368b779",
    ),
    (
        "0019_subscription_source_key_unique_index.sql",
        "1fae8336d665917f84d80d75d61b2feb471326de77a1bc5de33be5663ea45611",
    ),
    (
        "0020_subscription_gmail_message_unique_index.sql",
        "34b2502cdcb7ace25fe127fb3e119f64c8450ab63f57eac56cf56fabfef17986",
    ),
    (
        "0021_subscription_rfc_message_index.sql",
        "b5ebd16fa0defa50173d7e58130e42c7c4213677fe02debb9c98f19489705b8e",
    ),
    (
        "0022_subscription_pending_age_index.sql",
        "3238a0e6af6e7f226cd5e138b06d9b6acfa3e0537f48e34e8068dbb1d8356d5a",
    ),
    (
        "0023_email_connection_identity_backfill.sql",
        "4e0a3e6065daffdd7abe83e52af3f5b115f5d9da2acc8ce4d7dfb590c5044b0d",
    ),
    (
        "0024_email_connection_identity_validate.sql",
        "f11ea80bc67bc538f42b7b833038e21b2e144a1a3043e6176231d30316921d5f",
    ),
    (
        "0025_email_connection_identity_unique_index.sql",
        "ea0d84e08c7a1604d76db0e561e0b7d3dbeac7352f0952df79c2417702c45d56",
    ),
];

#[test]
fn legacy_migration_lineage_is_byte_for_byte_frozen() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/infrastructure/migrations");
    let mut actual_names = fs::read_dir(&root)
        .expect("read frozen legacy migration directory")
        .map(|entry| {
            entry
                .expect("read legacy migration entry")
                .file_name()
                .into_string()
                .expect("legacy migration filenames are UTF-8")
        })
        .collect::<Vec<_>>();
    actual_names.sort();

    let expected_names = FROZEN_LEGACY_MIGRATIONS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<Vec<_>>();
    assert_eq!(actual_names, expected_names, "legacy migration set changed");

    for (name, expected_checksum) in FROZEN_LEGACY_MIGRATIONS {
        let bytes = fs::read(root.join(name)).expect("read frozen legacy migration");
        let actual_checksum = format!("{:x}", Sha256::digest(bytes));
        assert_eq!(
            actual_checksum, *expected_checksum,
            "legacy migration {name} changed"
        );
    }
}
