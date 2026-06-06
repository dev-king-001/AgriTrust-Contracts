#![cfg(test)]

use super::{GrantStreamContract, GrantStreamContractClient, GrantStatus, SCALING_FACTOR};
use crate::storage_keys::StorageKey;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, Address, Env, Bytes, xdr::ToXdr,
};

fn setup_test(env: &Env) -> (Address, Address, Address, Address, Address, GrantStreamContractClient) {
    let admin = Address::generate(env);
    let grant_token_addr = env.register_stellar_asset_contract_v2(admin.clone());
    let native_token_addr = env.register_stellar_asset_contract_v2(admin.clone());
    let treasury = Address::generate(env);
    let oracle = Address::generate(env);

    let contract_id = env.register(GrantStreamContract, ());
    let client = GrantStreamContractClient::new(env, &contract_id);

    client.initialize(&admin, &grant_token_addr.address(), &treasury, &oracle, &native_token_addr.address());

    (admin, grant_token_addr.address(), treasury, oracle, native_token_addr.address(), client)
}

fn set_timestamp(env: &Env, timestamp: u64) {
    env.ledger().with_mut(|li| {
        li.timestamp = timestamp;
    });
}

#[test]
fn test_prune_finalized_grant_success() {
    let env = Env::default();
    env.mock_all_auths();
    let (_admin, grant_token_addr, _treasury, _oracle, native_token_addr, client) = setup_test(&env);
    
    let recipient = Address::generate(&env);
    let relayer = Address::generate(&env);
    let grant_token_admin = token::StellarAssetClient::new(&env, &grant_token_addr);
    let native_token_admin = token::StellarAssetClient::new(&env, &native_token_addr);
    let native_token = token::Client::new(&env, &native_token_addr);

    // Initial setup
    let start_ts = 1_000_000;
    set_timestamp(&env, start_ts);
    
    let grant_id = 1;
    let total_amount = 100 * SCALING_FACTOR;
    let flow_rate = 1 * SCALING_FACTOR; // 1 token per second
    
    // Mint tokens
    grant_token_admin.mint(&client.address, &total_amount);
    native_token_admin.mint(&client.address, &1_000_000); // For bounty

    // 1. Create Grant
    client.create_grant(&grant_id, &recipient, &total_amount, &flow_rate, &0, &None, &None);
    
    // 2. Fast forward to completion
    set_timestamp(&env, start_ts + 100);
    
    // 3. Withdraw all funds to drain the grant
    client.withdraw(&grant_id, &total_amount);
    
    let grant = client.get_grant(&grant_id);
    assert_eq!(grant.status, GrantStatus::Completed);
    assert_eq!(grant.claimable, 0);

    // 4. Try to prune early (should fail)
    set_timestamp(&env, start_ts + 100 + (90 * 24 * 60 * 60)); // 90 days later
    let result = client.try_prune_finalized_grant(&grant_id, &relayer);
    assert!(result.is_err(), "Should not be able to prune before 180 days");

    // 5. Fast forward to 181 days
    let prune_ts = start_ts + 100 + (181 * 24 * 60 * 60);
    set_timestamp(&env, prune_ts);
    
    // Capture state for tombstone verification
    let grant_before_prune = client.get_grant(&grant_id);

    // 6. Prune
    client.prune_finalized_grant(&grant_id, &relayer);
    
    // 7. Verify Cleanup
    // Grant should be gone from instance storage
    let result = client.try_get_grant(&grant_id);
    assert!(result.is_err(), "Grant should be removed from storage");
    
    // Tombstone should exist in persistent storage
    let expected_hash = env.crypto().sha256(&grant_before_prune.to_xdr(&env));
    let contract_id = client.address.clone();
    let stored_tombstone: Bytes = env.as_contract(&contract_id, || {
        env.storage().persistent().get(&StorageKey::Tombstone(grant_id)).expect("Tombstone should exist")
    });
    assert_eq!(stored_tombstone, expected_hash.into(), "Tombstone hash should match");
    
    // Relayer should receive bounty
    assert_eq!(native_token.balance(&relayer), 200_000, "Relayer should receive 0.02 XLM bounty");
    
    // Grant ID should be removed from recipient's list
    assert!(!client.is_active_grantee(&recipient), "Recipient should no longer have active grants");
}

#[test]
fn test_prune_fails_for_active_grant() {
    let env = Env::default();
    env.mock_all_auths();
    let (_admin, grant_token_addr, _treasury, _oracle, _native, client) = setup_test(&env);
    
    let recipient = Address::generate(&env);
    let relayer = Address::generate(&env);
    let grant_token_admin = token::StellarAssetClient::new(&env, &grant_token_addr);

    set_timestamp(&env, 1000);
    let grant_id = 1;
    grant_token_admin.mint(&client.address, &1000000);
    client.create_grant(&grant_id, &recipient, &1000000, &100, &0, &None, &None);
    
    // Fast forward 200 days
    set_timestamp(&env, 1000 + (200 * 24 * 60 * 60));
    
    let result = client.try_prune_finalized_grant(&grant_id, &relayer);
    assert!(result.is_err(), "Should not prune an active grant");
}

#[test]
fn test_prune_fails_for_undrained_grant() {
    let env = Env::default();
    env.mock_all_auths();
    let (_admin, grant_token_addr, _treasury, _oracle, _native, client) = setup_test(&env);
    
    let recipient = Address::generate(&env);
    let relayer = Address::generate(&env);
    let grant_token_admin = token::StellarAssetClient::new(&env, &grant_token_addr);

    set_timestamp(&env, 1000);
    let grant_id = 1;
    grant_token_admin.mint(&client.address, &1000000);
    client.create_grant(&grant_id, &recipient, &1000000, &100, &0, &None, &None);
    
    // Settle to completion but don't withdraw
    set_timestamp(&env, 1000 + 10000);
    let grant = client.get_grant(&grant_id);
    assert_eq!(grant.status, GrantStatus::Completed);
    assert!(grant.claimable > 0);
    
    // Fast forward 200 days
    set_timestamp(&env, 1000 + 10000 + (200 * 24 * 60 * 60));
    
    let result = client.try_prune_finalized_grant(&grant_id, &relayer);
    assert!(result.is_err(), "Should not prune a grant with remaining funds");
}
