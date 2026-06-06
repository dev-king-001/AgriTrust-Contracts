//! Integration tests for Donor Reputation System with Matching Pool
//! 
//! These tests verify that:
//! - High-reputation donors trigger larger matches from community pool
//! - Reputation-based influence scaling works in quadratic funding
//! - Self-optimizing matching rounds function correctly
//! - Reputation farming is structurally blocked

#![cfg(test)]

extern crate std;

use std::println;
use soroban_sdk::{token, Address, Env, vec, Vec, testutils::{Address as _, Ledger}};
use crate::donor_reputation::*;
use crate::matching_pool::*;
use crate::{GrantStatus, REPUTATION_SCALE, BASIS_POINTS, DEFAULT_MIN_FUNDING_THRESHOLD, MAX_REPUTATION_MULTIPLIER, FIXED_POINT_SCALE};

fn create_test_env() -> Env {
    let env = Env::default();
    env.register_contract(None, crate::GrantStreamContract);
    env.mock_all_auths();
    env
}

fn setup_reputation_and_matching(env: &Env) -> (Address, Address) {
    let admin = Address::generate(env);
    let token = env.register_stellar_asset_contract_v2(admin.clone());

    // Initialize reputation system
    DonorReputationContract::initialize(env.clone(), admin.clone()).unwrap();

    // Initialize matching pool
    MatchingPoolContract::initialize_pool(
        env.clone(),
        1, // pool_id
        admin.clone(),
        token.clone(),
        1_000_000_000, // 1000 tokens for matching pool
        86400, // 24 hour round
        false, // no SEP-12 required for testing
        10_000_000, // 1 USDC minimum donation
        100_000_000, // 10 USDC max donation per donor
    ).unwrap();

    (admin, token)
}

fn create_donor_with_reputation(env: &Env, success_rate: i128, project_count: u32) -> Address {
    let donor = Address::generate(env);
    
    for i in 0..project_count {
        let project_id = i + 1;
        
        // Fund project
        DonorReputationContract::record_project_funded(
            env.clone(),
            donor.clone(),
            project_id as u64,
            DEFAULT_MIN_FUNDING_THRESHOLD,
            2, // 2 milestones per project
        ).unwrap();

        // Complete milestones based on success rate
        let should_succeed = (i as i128) < (project_count as i128 * success_rate / BASIS_POINTS);
        if should_succeed {
            DonorReputationContract::record_milestone_completed(env.clone(), project_id as u64, 0, None).unwrap();
            DonorReputationContract::record_milestone_completed(env.clone(), project_id as u64, 1, None).unwrap();
        } else {
            DonorReputationContract::record_project_failed(env.clone(), project_id as u64).unwrap();
        }
    }

    donor
}

#[test]
fn test_high_reputation_donor_larger_match() {
    let env = create_test_env();
    let (admin, token) = setup_reputation_and_matching(&env);

    // Create donors with different reputation levels
    let perfect_donor = create_donor_with_reputation(&env, 100 * BASIS_POINTS / 100, 3); // 100% success
    let average_donor = create_donor_with_reputation(&env, 50 * BASIS_POINTS / 100, 3);  // 50% success
    let poor_donor = create_donor_with_reputation(&env, 0, 3); // 0% success

    // All donors donate same amount to same project
    let donation_amount = 50_000_000; // 5 USDC
    let project_id = 1;

    // Mint tokens to donors and make donations
    let token_admin = token::StellarAssetClient::new(&env, &token);
    token_admin.mint(&perfect_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, project_id, perfect_donor.clone(), donation_amount).unwrap();
    token_admin.mint(&average_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, project_id, average_donor.clone(), donation_amount).unwrap();
    token_admin.mint(&poor_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, project_id, poor_donor.clone(), donation_amount).unwrap();

    // Fast forward to end of round
    env.ledger().with_mut(|li| { li.timestamp = env.ledger().timestamp() + 86401; });

    // Calculate matching
    let mut projects = Vec::new(&env); projects.push_back(project_id);
    let total_matched = MatchingPoolContract::calculate_matching(env.clone(), 1, projects).unwrap();

    // Check project contributions to verify influence scaling
    let contributions = MatchingPoolContract::get_project_contributions(env.clone(), 1, project_id).unwrap();
    
    // Perfect donor should have 2x influence (100% success rate)
    let perfect_influence = DonorReputationContract::calculate_influence(env.clone(), perfect_donor.clone()).unwrap();
    assert_eq!(perfect_influence, MAX_REPUTATION_MULTIPLIER);

    // Average donor should have 1.5x influence (50% success rate)
    let average_influence = DonorReputationContract::calculate_influence(env.clone(), average_donor.clone()).unwrap();
    let expected_average = REPUTATION_SCALE + (50 * (MAX_REPUTATION_MULTIPLIER - REPUTATION_SCALE) / BASIS_POINTS);
    assert_eq!(average_influence, expected_average);

    // Poor donor should have 1x influence (0% success rate)
    let poor_influence = DonorReputationContract::calculate_influence(env.clone(), poor_donor.clone()).unwrap();
    assert_eq!(poor_influence, REPUTATION_SCALE);

    // Verify that total contributions reflect reputation influence
    // Perfect donor: 5 USDC * 2x = 10 USDC equivalent
    // Average donor: 5 USDC * 1.5x = 7.5 USDC equivalent
    // Poor donor: 5 USDC * 1x = 5 USDC equivalent
    // Total: 22.5 USDC equivalent
    
    let expected_total = donation_amount * 2 + // Perfect donor
        (donation_amount * 3 / 2) + // Average donor (1.5x)
        donation_amount; // Poor donor (1x)
    
    assert_eq!(contributions.total_contributions, expected_total);
    assert_eq!(contributions.unique_donors, 3);

    // Verify that matching was calculated correctly
    assert!(total_matched > 0, "Should have matched funds");
    assert!(total_matched <= 1_000_000_000, "Should not exceed pool limit");
}

#[test]
fn test_self_optimizing_matching_rounds() {
    let env = create_test_env();
    let (admin, token) = setup_reputation_and_matching(&env);

    // Create two projects with different donor quality
    let high_quality_donor = create_donor_with_reputation(&env, 100 * BASIS_POINTS / 100, 5); // Excellent track record
    let low_quality_donor = create_donor_with_reputation(&env, 20 * BASIS_POINTS / 100, 5);  // Poor track record

    // Both projects get same number of donations from their respective donors
    let donation_amount = 30_000_000; // 3 USDC each
    let token_admin = token::StellarAssetClient::new(&env, &token);
    
    // High-quality project: 3 donations from high-reputation donor
    token_admin.mint(&high_quality_donor, &(donation_amount * 3));
    for i in 0..3 {
        MatchingPoolContract::donate(env.clone(), 1, 1, high_quality_donor.clone(), donation_amount).unwrap();
    }

    // Low-quality project: 3 donations from low-reputation donor  
    token_admin.mint(&low_quality_donor, &(donation_amount * 3));
    for i in 0..3 {
        MatchingPoolContract::donate(env.clone(), 1, 2, low_quality_donor.clone(), donation_amount).unwrap();
    }

    // Fast forward to end of round
    env.ledger().with_mut(|li| { li.timestamp = env.ledger().timestamp() + 86401; });

    // Calculate matching
    let projects = vec![&env, 1u64, 2u64];
    let total_matched = MatchingPoolContract::calculate_matching(env.clone(), 1, projects).unwrap();

    // Check contributions for both projects
    let high_quality_contrib = MatchingPoolContract::get_project_contributions(env.clone(), 1, 1).unwrap();
    let low_quality_contrib = MatchingPoolContract::get_project_contributions(env.clone(), 1, 2).unwrap();

    // High-quality project should have more effective contributions due to reputation
    assert!(high_quality_contrib.total_contributions > low_quality_contrib.total_contributions,
        "High-quality project should have more effective contributions");

    // Get matched amounts for each project
    let high_quality_matched = MatchingPoolContract::get_project_matched(env.clone(), 1, 1).unwrap();
    let low_quality_matched = MatchingPoolContract::get_project_matched(env.clone(), 1, 2).unwrap();

    // High-quality project should receive more matching funds
    assert!(high_quality_matched > low_quality_matched,
        "High-quality project should receive more matching funds");

    // This demonstrates self-optimizing behavior: reputable donors guide more capital to successful teams
    println!("High-quality project matched: {}", high_quality_matched);
    println!("Low-quality project matched: {}", low_quality_matched);
    println!("Ratio: {}", high_quality_matched as f64 / low_quality_matched as f64);
}

#[test]
fn test_reputation_farming_structural_block() {
    let env = create_test_env();
    let (admin, token) = setup_reputation_and_matching(&env);

    let farmer = Address::generate(&env);
    
    // Attempt reputation farming by creating many micro-projects at minimum threshold
    let micro_projects = 20;
    for i in 0..micro_projects {
        let project_id = i + 1;
        
        // Fund at minimum threshold
        DonorReputationContract::record_project_funded(
            env.clone(),
            farmer.clone(),
            project_id,
            DEFAULT_MIN_FUNDING_THRESHOLD,
            1, // Single milestone for quick completion
        ).unwrap();

        // Complete milestone
        DonorReputationContract::record_milestone_completed(env.clone(), project_id as u64, 0, None).unwrap();
    }

    // Farmer should have maximum reputation (100% success rate)
    let farmer_reputation = DonorReputationContract::get_donor_reputation(env.clone(), farmer.clone()).unwrap();
    assert_eq!(farmer_reputation.success_rate, BASIS_POINTS);
    assert_eq!(farmer_reputation.qualifying_projects, micro_projects as u32);

    // But influence is capped, preventing excessive advantage
    let farmer_influence = DonorReputationContract::calculate_influence(env.clone(), farmer.clone()).unwrap();
    assert_eq!(farmer_influence, MAX_REPUTATION_MULTIPLIER); // Capped at 3x

    // Now test in matching pool - even with max reputation, can't get unlimited advantage
    let donation_amount = 100_000_000; // 10 USDC
    let token_admin = token::StellarAssetClient::new(&env, &token);
    token_admin.mint(&farmer, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, 1, farmer.clone(), donation_amount).unwrap();

    // Create a legitimate high-quality donor for comparison
    let legitimate_donor = create_donor_with_reputation(&env, 100 * BASIS_POINTS / 100, 3);
    token_admin.mint(&legitimate_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, 2, legitimate_donor.clone(), donation_amount).unwrap();

    // Fast forward and calculate matching
    env.ledger().with_mut(|li| { li.timestamp = env.ledger().timestamp() + 86401; });
    let projects = vec![&env, 1u64, 2u64];
    MatchingPoolContract::calculate_matching(env.clone(), 1, projects).unwrap();

    // Both should have same influence multiplier despite different project counts
    let farmer_contrib = MatchingPoolContract::get_project_contributions(env.clone(), 1, 1).unwrap();
    let legitimate_contrib = MatchingPoolContract::get_project_contributions(env.clone(), 1, 2).unwrap();

    // Both should have same effective contribution (same donation amount * same max multiplier)
    assert_eq!(farmer_contrib.total_contributions, legitimate_contrib.total_contributions);

    // This shows the structural block: no matter how many micro-projects, influence is capped
}

#[test]
fn test_financial_barriers_to_reputation_farming() {
    let env = create_test_env();
    let (admin, token) = setup_reputation_and_matching(&env);

    // Update configuration to increase financial barriers
    let high_threshold = DEFAULT_MIN_FUNDING_THRESHOLD * 10; // 10x higher threshold
    DonorReputationContract::update_config(
        env.clone(),
        admin.clone(),
        Some(high_threshold),
        None,
        None,
        None,
    ).unwrap();

    let poor_attacker = Address::generate(&env);
    let rich_donor = Address::generate(&env);

    // Poor attacker attempts to farm reputation with many small projects
    let small_projects = 10;
    for i in 0..small_projects {
        let project_id = i + 1;
        
        // Try to fund below new threshold
        let result = DonorReputationContract::record_project_funded(
            env.clone(),
            poor_attacker.clone(),
            project_id,
            DEFAULT_MIN_FUNDING_THRESHOLD, // Below new threshold
            1,
        );
        
        // Should succeed in funding but not accrue reputation
        assert!(result.is_ok());
        
        // Complete milestone
        DonorReputationContract::record_milestone_completed(env.clone(), project_id as u64, 0, None).unwrap();
    }

    // Poor attacker should have no reputation despite completing projects
    let result = DonorReputationContract::get_donor_reputation(env.clone(), poor_attacker.clone());
    assert!(result.is_err(), "Poor attacker should not have reputation");

    // Rich donor funds one project above threshold
    DonorReputationContract::record_project_funded(
        env.clone(),
        rich_donor.clone(),
        101,
        high_threshold,
        3,
    ).unwrap();

    for i in 0..3 {
        DonorReputationContract::record_milestone_completed(env.clone(), 101, i, None).unwrap();
    }

    // Rich donor should have high reputation
    let rich_reputation = DonorReputationContract::get_donor_reputation(env.clone(), rich_donor.clone()).unwrap();
    assert_eq!(rich_reputation.success_rate, BASIS_POINTS);
    assert_eq!(rich_reputation.qualifying_projects, 1);

    // Test in matching pool
    let donation_amount = 50_000_000;
    let token_admin = token::StellarAssetClient::new(&env, &token);
    
    // Poor attacker's donation should have baseline influence
    token_admin.mint(&poor_attacker, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, 1, poor_attacker.clone(), donation_amount).unwrap();
    
    // Rich donor's donation should have maximum influence
    token_admin.mint(&rich_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, 2, rich_donor.clone(), donation_amount).unwrap();

    env.ledger().with_mut(|li| { li.timestamp = env.ledger().timestamp() + 86401; });
    let projects = vec![&env, 1u64, 2u64];
    MatchingPoolContract::calculate_matching(env.clone(), 1, projects).unwrap();

    let poor_contrib = MatchingPoolContract::get_project_contributions(env.clone(), 1, 1).unwrap();
    let rich_contrib = MatchingPoolContract::get_project_contributions(env.clone(), 1, 2).unwrap();

    // Rich donor's project should have significantly more effective contributions
    assert!(rich_contrib.total_contributions > poor_contrib.total_contributions * 2,
        "Rich donor should have at least 2x the influence");

    // This demonstrates financial barriers prevent reputation farming
}

#[test]
fn test_time_based_barriers() {
    let env = create_test_env();
    let (admin, token) = setup_reputation_and_matching(&env);

    let quick_farmer = Address::generate(&env);
    
    // Try to build reputation quickly in short time window
    let start_time = env.ledger().timestamp();
    
    // Create multiple projects in quick succession
    for i in 0..5 {
        let project_id = i + 1;
        
        DonorReputationContract::record_project_funded(
            env.clone(),
            quick_farmer.clone(),
            project_id,
            DEFAULT_MIN_FUNDING_THRESHOLD,
            1,
        ).unwrap();

        DonorReputationContract::record_milestone_completed(env.clone(), project_id as u64, 0, None).unwrap();
        
        // Advance time slightly between projects (but still within window)
        env.ledger().set_timestamp(start_time + (i as u64 * 3600)); // 1 hour between projects
    }

    let quick_reputation = DonorReputationContract::get_donor_reputation(env.clone(), quick_farmer.clone()).unwrap();
    
    // Should have reputation (system doesn't currently implement time-based restrictions)
    // but this test shows where such restrictions could be added
    assert_eq!(quick_reputation.success_rate, BASIS_POINTS);
    assert_eq!(quick_reputation.qualifying_projects, 5);

    // The system could be enhanced with time-based barriers in future iterations
    // For now, financial barriers provide the main protection
}

#[test]
fn test_incentive_alignment() {
    let env = create_test_env();
    let (admin, token) = setup_reputation_and_matching(&env);

    // Create three donors representing different incentive scenarios
    let diligent_donor = create_donor_with_reputation(&env, 100 * BASIS_POINTS / 100, 5); // Always does due diligence
    let careless_donor = create_donor_with_reputation(&env, 30 * BASIS_POINTS / 100, 5);  // Poor due diligence
    let new_donor = Address::generate(&env); // No track record

    // All donate to the same promising project
    let project_id = 1;
    let donation_amount = 40_000_000; // 4 USDC each
    let token_admin = token::StellarAssetClient::new(&env, &token);

    token_admin.mint(&diligent_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, project_id, diligent_donor.clone(), donation_amount).unwrap();
    token_admin.mint(&careless_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, project_id, careless_donor.clone(), donation_amount).unwrap();
    token_admin.mint(&new_donor, &donation_amount);
    MatchingPoolContract::donate(env.clone(), 1, project_id, new_donor.clone(), donation_amount).unwrap();

    // Fast forward and calculate matching
    env.ledger().with_mut(|li| { li.timestamp = env.ledger().timestamp() + 86401; });
    let mut projects = Vec::new(&env); projects.push_back(project_id);
    let total_matched = MatchingPoolContract::calculate_matching(env.clone(), 1, projects).unwrap();

    let contributions = MatchingPoolContract::get_project_contributions(env.clone(), 1, project_id).unwrap();

    // Calculate expected contributions based on reputation influence
    let diligent_influence = DonorReputationContract::calculate_influence(env.clone(), diligent_donor.clone()).unwrap();
    let careless_influence = DonorReputationContract::calculate_influence(env.clone(), careless_donor.clone()).unwrap();
    let new_influence = DonorReputationContract::calculate_influence(env.clone(), new_donor.clone()).unwrap();

    // Diligent donor should have maximum influence (2x)
    assert_eq!(diligent_influence, MAX_REPUTATION_MULTIPLIER);
    
    // Careless donor should have reduced influence (around 1.3x for 30% success rate)
    let expected_careless = REPUTATION_SCALE + (30 * (MAX_REPUTATION_MULTIPLIER - REPUTATION_SCALE) / BASIS_POINTS);
    assert_eq!(careless_influence, expected_careless);
    
    // New donor should have baseline influence (1x)
    assert_eq!(new_influence, REPUTATION_SCALE);

    // This demonstrates proper incentive alignment:
    // - Diligent donors get more matching power for their donations
    // - Careless donors have reduced influence
    // - New donors start at baseline but can build reputation

    println!("Diligent donor influence: {}x", diligent_influence as f64 / REPUTATION_SCALE as f64);
    println!("Careless donor influence: {}x", careless_influence as f64 / REPUTATION_SCALE as f64);
    println!("New donor influence: {}x", new_influence as f64 / REPUTATION_SCALE as f64);
    println!("Total project contributions: {}", contributions.total_contributions);
    println!("Total matched: {}", total_matched);
}
