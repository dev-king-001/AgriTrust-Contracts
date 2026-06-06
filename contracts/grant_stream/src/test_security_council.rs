#![cfg(test)]

use crate::security_council::*;
use soroban_sdk::testutils::Ledger;
use soroban_sdk::{testutils::Address as _, Address, Env, Vec};

/// Test: Initialize Security Council with 5 members
#[test]
fn test_initialize_council() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
    
        let result = SecurityCouncil::initialize_council(env.clone(), members.clone());
        assert!(result.is_ok());

        let retrieved = SecurityCouncil::get_council_members(env.clone()).unwrap();
        assert_eq!(retrieved.len(), 5);
    });
}

/// Test: Cannot initialize with wrong number of members
#[test]
fn test_initialize_wrong_size() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let mut members = Vec::new(&env);
        members.push_back(Address::generate(&env));
        members.push_back(Address::generate(&env));
        members.push_back(Address::generate(&env));

        let result = SecurityCouncil::initialize_council(env.clone(), members);
        assert_eq!(result, Err(SecurityCouncilError::InvalidCouncilSize));
    });
}

/// Test: Create pending action with 48-hour timelock
#[test]
fn test_create_pending_action() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator.clone(),
            params,
        )
        .unwrap();

        assert_eq!(action_id, 1);

        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Pending);
        assert_eq!(action.action_type, ActionType::Clawback);
        assert_eq!(action.executable_at, action.created_at + 48 * 60 * 60);
    });
}

/// Test: Security Council can veto an action with 3 signatures
#[test]
fn test_veto_action_with_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members.clone()).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator,
            params,
        )
        .unwrap();

        // First signature
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(0).unwrap()).unwrap();
        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Pending);
        assert_eq!(SecurityCouncil::get_veto_count(env.clone(), action_id), 1);

        // Second signature
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(1).unwrap()).unwrap();
        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Pending);
        assert_eq!(SecurityCouncil::get_veto_count(env.clone(), action_id), 2);

        // Third signature - should trigger veto
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(2).unwrap()).unwrap();
        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Vetoed);
        assert_eq!(SecurityCouncil::get_veto_count(env.clone(), action_id), 3);
    });
}

/// Test: Cannot execute vetoed action
#[test]
fn test_cannot_execute_vetoed_action() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members.clone()).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator,
            params,
        )
        .unwrap();

        // Veto with 3 signatures
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(0).unwrap()).unwrap();
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(1).unwrap()).unwrap();
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(2).unwrap()).unwrap();

        // Try to execute - should fail
        let result = SecurityCouncil::execute_action(env.clone(), action_id);
        assert_eq!(result, Err(SecurityCouncilError::ActionAlreadyVetoed));
    });
}

/// Test: Cannot execute before timelock expires
#[test]
fn test_cannot_execute_before_timelock() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator,
            params,
        )
        .unwrap();

        // Try to execute immediately - should fail
        let result = SecurityCouncil::execute_action(env.clone(), action_id);
        assert_eq!(result, Err(SecurityCouncilError::TimelockNotExpired));
    });
}

/// Test: Can execute after timelock if not vetoed
#[test]
fn test_execute_after_timelock() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator,
            params,
        )
        .unwrap();

        // Advance time by 48 hours
        env.ledger().with_mut(|li| {
            li.timestamp += 48 * 60 * 60;
        });

        // Should be able to execute now
        let can_execute = SecurityCouncil::can_execute_action(env.clone(), action_id).unwrap();
        assert!(can_execute);

        let result = SecurityCouncil::execute_action(env.clone(), action_id);
        assert!(result.is_ok());

        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Executed);
    });
}

/// Test: Rogue DAO attack scenario - malicious clawback is blocked
#[test]
fn test_rogue_dao_attack_blocked() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members.clone()).unwrap();

        // Simulate rogue DAO trying to claw back all grants
        let rogue_dao = Address::generate(&env);
        let params = Vec::new(&env);

        // Create malicious clawback action
        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            rogue_dao.clone(),
            params,
        )
        .unwrap();

        // Security Council detects the attack and vetoes within 48 hours
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(0).unwrap()).unwrap();
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(1).unwrap()).unwrap();
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(3).unwrap()).unwrap();

        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Vetoed);

        // Even after timelock, action cannot be executed
        env.ledger().with_mut(|li| {
            li.timestamp += 48 * 60 * 60;
        });

        let result = SecurityCouncil::execute_action(env.clone(), action_id);
        assert_eq!(result, Err(SecurityCouncilError::ActionAlreadyVetoed));
    });
}

/// Test: Multiple malicious actions blocked simultaneously
#[test]
fn test_multiple_attacks_blocked() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members.clone()).unwrap();

        let rogue_dao = Address::generate(&env);
        let params = Vec::new(&env);

        // Create multiple malicious actions
        let action1 = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            rogue_dao.clone(),
            params.clone(),
        )
        .unwrap();

        let action2 = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::TreasuryWithdraw,
            None,
            rogue_dao.clone(),
            params.clone(),
        )
        .unwrap();

        let action3 = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::AdminChange,
            None,
            rogue_dao.clone(),
            params,
        )
        .unwrap();

        // Council vetoes all three
        for action_id in [action1, action2, action3] {
            SecurityCouncil::sign_veto(env.clone(), action_id, members.get(0).unwrap()).unwrap();
            SecurityCouncil::sign_veto(env.clone(), action_id, members.get(1).unwrap()).unwrap();
            SecurityCouncil::sign_veto(env.clone(), action_id, members.get(2).unwrap()).unwrap();

            let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
            assert_eq!(action.status, ActionStatus::Vetoed);
        }
    });
}

/// Test: Council rotation with 7-day timelock
#[test]
fn test_council_rotation() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let old_members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), old_members).unwrap();

        let dao_admin = Address::generate(&env);
        let new_members = create_council_members(&env);

        // Propose rotation
        SecurityCouncil::propose_council_rotation(env.clone(), new_members.clone(), dao_admin).unwrap();

        // Cannot execute immediately
        let result = SecurityCouncil::execute_council_rotation(env.clone());
        assert_eq!(result, Err(SecurityCouncilError::TimelockNotExpired));

        // Advance time by 7 days
        env.ledger().with_mut(|li| {
            li.timestamp += 7 * 24 * 60 * 60;
        });

        // Execute rotation
        let result = SecurityCouncil::execute_council_rotation(env.clone());
        assert!(result.is_ok());

        // Verify new members
        let current_members = SecurityCouncil::get_council_members(env.clone()).unwrap();
        assert_eq!(current_members.len(), 5);
    });
}

/// Test: Check rotation due after 1 year
#[test]
fn test_rotation_due_check() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members).unwrap();

        // Initially not due
        assert!(!SecurityCouncil::is_rotation_due(env.clone()));

        // Advance time by 1 year
        env.ledger().with_mut(|li| {
            li.timestamp += 365 * 24 * 60 * 60;
        });

        // Now rotation is due
        assert!(SecurityCouncil::is_rotation_due(env.clone()));
    });
}

/// Test: Non-council member cannot sign veto
#[test]
fn test_non_member_cannot_veto() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator,
            params,
        )
        .unwrap();

        let non_member = Address::generate(&env);
        let result = SecurityCouncil::sign_veto(env.clone(), action_id, non_member);
        assert_eq!(result, Err(SecurityCouncilError::NotCouncilMember));
    });
}

/// Test: Cannot sign veto twice
#[test]
fn test_cannot_double_sign() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members.clone()).unwrap();

        let initiator = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::Clawback,
            Some(1),
            initiator,
            params,
        )
        .unwrap();

        let member = members.get(0).unwrap();
        SecurityCouncil::sign_veto(env.clone(), action_id, member.clone()).unwrap();

        // Try to sign again
        let result = SecurityCouncil::sign_veto(env.clone(), action_id, member);
        assert_eq!(result, Err(SecurityCouncilError::AlreadySigned));
    });
}

/// Test: Legitimate action can proceed if council doesn't veto
#[test]
fn test_legitimate_action_proceeds() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, crate::GrantStreamContract);
    env.as_contract(&contract_id, || {

        let members = create_council_members(&env);
        SecurityCouncil::initialize_council(env.clone(), members.clone()).unwrap();

        let legitimate_dao = Address::generate(&env);
        let params = Vec::new(&env);

        let action_id = SecurityCouncil::create_pending_action(
            env.clone(),
            ActionType::RateChange,
            Some(1),
            legitimate_dao,
            params,
        )
        .unwrap();

        // Only 2 council members sign (not enough to veto)
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(0).unwrap()).unwrap();
        SecurityCouncil::sign_veto(env.clone(), action_id, members.get(1).unwrap()).unwrap();

        let action = SecurityCouncil::get_pending_action(env.clone(), action_id).unwrap();
        assert_eq!(action.status, ActionStatus::Pending);

        // Advance time
        env.ledger().with_mut(|li| {
            li.timestamp += 48 * 60 * 60;
        });

        // Action can still be executed
        let result = SecurityCouncil::execute_action(env.clone(), action_id);
        assert!(result.is_ok());
    });
}

// Helper function to create 5 council members
fn create_council_members(env: &Env) -> Vec<Address> {
    let mut members = Vec::new(env);
    for _ in 0..5 {
        members.push_back(Address::generate(env));
    }
    members
}
