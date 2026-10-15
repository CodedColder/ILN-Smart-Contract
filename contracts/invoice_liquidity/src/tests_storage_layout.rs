#[cfg(test)]
mod tests {
    use crate::constants::CURRENT_STORAGE_VERSION;
    use crate::invoice::{Invoice, InvoiceCore, InvoiceMetadata, InvoiceStatus, ReferralCode};
    use crate::storage::{load_invoice, save_invoice, try_load_invoice_core, DataKey};
    use crate::{InvoiceLiquidityContract, InvoiceLiquidityContractClient};
    use soroban_sdk::testutils::{Address as TestAddress, MockAuth, MockAuthInvoke};
    use soroban_sdk::{Address, Env, IntoVal};

    #[test]
    fn test_invoice_to_core_split() {
        let env = Env::default();
        // Create a full invoice
        let invoice = Invoice {
            id: 123,
            freelancer: Address::generate(&env),
            payer: Address::generate(&env),
            token: Address::generate(&env),
            amount: 1_000_000,
            due_date: 1234567890,
            discount_rate: 300,
            status: InvoiceStatus::Pending,
            funder: Some(Address::generate(&env)),
            funded_at: Some(1234567800),
            amount_funded: 0,
            amount_paid: 0,
            referral_code: ReferralCode::None,
            submitter_reputation: 50,
        };

        // Extract core
        let core = invoice.to_core();
        assert_eq!(core.id, 123);
        assert_eq!(core.amount, 1_000_000);
        assert_eq!(core.due_date, 1234567890);
        assert_eq!(core.discount_rate, 300);
        assert_eq!(core.status, InvoiceStatus::Pending);
        assert_eq!(core.amount_funded, 0);
        assert_eq!(core.amount_paid, 0);

        // Extract metadata
        let metadata = invoice.to_metadata();
        assert_eq!(metadata.funded_at, Some(1234567800));
        assert_eq!(metadata.submitter_reputation, 50);
        assert_eq!(metadata.referral_code, ReferralCode::None);
    }

    #[test]
    fn test_invoice_core_with_metadata_roundtrip() {
        let env = Env::default();
        // Create core and metadata
        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let token = Address::generate(&env);
        let funder = Address::generate(&env);

        let core = InvoiceCore {
            id: 456,
            freelancer: freelancer.clone(),
            payer: payer.clone(),
            token: token.clone(),
            amount: 2_000_000,
            due_date: 1234567890,
            discount_rate: 500,
            status: InvoiceStatus::Funded,
            amount_funded: 2_000_000,
            amount_paid: 0,
        };

        let metadata = InvoiceMetadata {
            funder: Some(funder.clone()),
            funded_at: Some(1234567850),
            referral_code: ReferralCode::None,
            submitter_reputation: 75,
        };

        // Combine into full invoice
        let invoice = core.clone().with_metadata(metadata.clone());

        // Verify all fields match
        assert_eq!(invoice.id, 456);
        assert_eq!(invoice.freelancer, freelancer);
        assert_eq!(invoice.payer, payer);
        assert_eq!(invoice.token, token);
        assert_eq!(invoice.amount, 2_000_000);
        assert_eq!(invoice.due_date, 1234567890);
        assert_eq!(invoice.discount_rate, 500);
        assert_eq!(invoice.status, InvoiceStatus::Funded);
        assert_eq!(invoice.amount_funded, 2_000_000);
        assert_eq!(invoice.amount_paid, 0);
        assert_eq!(invoice.funder, Some(funder));
        assert_eq!(invoice.funded_at, Some(1234567850));
        assert_eq!(invoice.submitter_reputation, 75);

        // Round-trip back to core and metadata
        assert_eq!(invoice.to_core(), core);
        assert_eq!(invoice.to_metadata(), metadata);
    }

    #[test]
    fn test_invoice_hot_cold_separation_consistency() {
        let env = Env::default();
        // Test that extracting hot/cold and recombining gives same result
        let invoice = Invoice {
            id: 789,
            freelancer: Address::generate(&env),
            payer: Address::generate(&env),
            token: Address::generate(&env),
            amount: 5_000_000,
            due_date: 987654321,
            discount_rate: 100,
            status: InvoiceStatus::PartiallyFunded,
            funder: None,
            funded_at: None,
            amount_funded: 2_500_000,
            amount_paid: 1_500_000,
            referral_code: ReferralCode::None,
            submitter_reputation: 25,
        };

        let core = invoice.to_core();
        let metadata = invoice.to_metadata();
        let reconstructed = core.with_metadata(metadata);

        // Should be identical
        assert_eq!(invoice.id, reconstructed.id);
        assert_eq!(invoice.amount, reconstructed.amount);
        assert_eq!(invoice.amount_funded, reconstructed.amount_funded);
        assert_eq!(invoice.amount_paid, reconstructed.amount_paid);
        assert_eq!(invoice.status, reconstructed.status);
        assert_eq!(invoice.funder, reconstructed.funder);
        assert_eq!(invoice.funded_at, reconstructed.funded_at);
        assert_eq!(
            invoice.submitter_reputation,
            reconstructed.submitter_reputation
        );
    }

    #[test]
    fn test_upgrade_migration_sequencing_full_lifecycle() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let usdc = Address::generate(&env);
        let eurc = Address::generate(&env);
        let xlm = Address::generate(&env);

        let contract_id = env.register(InvoiceLiquidityContract, ());
        let client = InvoiceLiquidityContractClient::new(&env, &contract_id);
        client.initialize(&admin, &usdc, &eurc, &xlm);

        // Pre-migration state: schema version defaults to 1
        assert_eq!(client.get_storage_version(), 1);

        // Seed a legacy monolithic invoice (v1 format: DataKey::Invoice(100))
        let freelancer = Address::generate(&env);
        let payer = Address::generate(&env);
        let legacy_invoice = Invoice {
            id: 100,
            freelancer: freelancer.clone(),
            payer: payer.clone(),
            token: usdc.clone(),
            amount: 1_000_000,
            due_date: 1_700_000_000,
            discount_rate: 300,
            status: InvoiceStatus::Pending,
            funder: None,
            funded_at: None,
            amount_funded: 0,
            amount_paid: 0,
            referral_code: ReferralCode::None,
            submitter_reputation: 50,
        };

        // Write directly using the legacy monolithic DataKey::Invoice(100)
        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::Invoice(100), &legacy_invoice);
        });

        // Verify backward-compatible loading of legacy record before migration
        env.as_contract(&contract_id, || {
            let loaded = load_invoice(&env, 100);
            assert_eq!(loaded.id, 100);
            assert_eq!(loaded.freelancer, freelancer);
            assert_eq!(loaded.amount, 1_000_000);

            let core = try_load_invoice_core(&env, 100).expect("legacy core should load");
            assert_eq!(core.id, 100);
            assert_eq!(core.status, InvoiceStatus::Pending);
        });

        // Upgrade step: execute migrate()
        let migrated_version = client.migrate();
        assert_eq!(migrated_version, CURRENT_STORAGE_VERSION);
        assert_eq!(client.get_storage_version(), CURRENT_STORAGE_VERSION);

        // Idempotency check: repeated migrate() call is safe and returns current version
        let idempotent_run = client.migrate();
        assert_eq!(idempotent_run, CURRENT_STORAGE_VERSION);
        assert_eq!(client.get_storage_version(), CURRENT_STORAGE_VERSION);

        // Verify backward-compatible loading continues to work after migration
        env.as_contract(&contract_id, || {
            let loaded_after = load_invoice(&env, 100);
            assert_eq!(loaded_after.id, 100);
            assert_eq!(loaded_after.freelancer, freelancer);

            // New invoice written post-migration uses split storage
            let new_invoice = Invoice {
                id: 101,
                freelancer: freelancer.clone(),
                payer: payer.clone(),
                token: usdc.clone(),
                amount: 2_000_000,
                due_date: 1_700_001_000,
                discount_rate: 250,
                status: InvoiceStatus::Pending,
                funder: None,
                funded_at: None,
                amount_funded: 0,
                amount_paid: 0,
                referral_code: ReferralCode::None,
                submitter_reputation: 60,
            };
            save_invoice(&env, &new_invoice);

            // Verify new invoice is stored in hot/cold split keys
            assert!(env
                .storage()
                .persistent()
                .has(&DataKey::InvoiceCore(101)));
            assert!(env
                .storage()
                .persistent()
                .has(&DataKey::InvoiceMetadata(101)));

            let loaded_new = load_invoice(&env, 101);
            assert_eq!(loaded_new.id, 101);
            assert_eq!(loaded_new.amount, 2_000_000);
        });
    }

    #[test]
    fn test_upgrade_migration_sequencing_authorization() {
        let env = Env::default();
        let admin = Address::generate(&env);
        let usdc = Address::generate(&env);
        let eurc = Address::generate(&env);
        let xlm = Address::generate(&env);

        let contract_id = env.register(InvoiceLiquidityContract, ());
        let client = InvoiceLiquidityContractClient::new(&env, &contract_id);
        client.initialize(&admin, &usdc, &eurc, &xlm);

        let imposter = Address::generate(&env);
        env.mock_auths(&[MockAuth {
            address: &imposter,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "migrate",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }]);

        let res = client.try_migrate();
        assert!(res.is_err(), "migrate should fail for non-admin caller");

        // Admin call succeeds
        env.mock_all_auths();
        let res_admin = client.try_migrate();
        assert!(res_admin.is_ok(), "migrate should succeed for admin caller");
        assert_eq!(res_admin.unwrap().unwrap(), CURRENT_STORAGE_VERSION);
    }

    #[test]
    fn test_upgrade_migration_sequencing_while_paused() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let usdc = Address::generate(&env);
        let eurc = Address::generate(&env);
        let xlm = Address::generate(&env);

        let contract_id = env.register(InvoiceLiquidityContract, ());
        let client = InvoiceLiquidityContractClient::new(&env, &contract_id);
        client.initialize(&admin, &usdc, &eurc, &xlm);

        // Protocol paused during upgrade
        client.pause();
        assert_eq!(client.is_paused(), true);

        // Storage migration succeeds while paused
        let res = client.migrate();
        assert_eq!(res, CURRENT_STORAGE_VERSION);
        assert_eq!(client.get_storage_version(), CURRENT_STORAGE_VERSION);

        // Unpause after migration verified
        client.unpause();
        assert_eq!(client.is_paused(), false);
    }
}
