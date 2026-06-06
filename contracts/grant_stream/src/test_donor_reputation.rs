//! Comprehensive tests for Donor Reputation System
//! 
//! These tests verify:
//! - Linear influence scaling across donor tiers
//! - Reputation farming prevention mechanisms
//! - Fair math calculations
//! - Event emissions
//! - Security measures

#![cfg(test)]

extern crate std;

use std::vec;
use soroban_sdk::{Address, Env, testutils::Address as _};
use crate::donor_reputation::*;
use crate::{GrantStatus, REPUTATION_SCALE, BASIS_POINTS, DEFAULT_MIN_FUNDING_THRESHOLD, MAX_REPUTATION_MULTIPLIER};

fn create_test_env() -> (Env, Address) {
    let env = Env::default();
    let admin = Address::generate(&env);
    (env, admin)
}

fn initialize_reputation_system(env: &Env, admin: &Address) {
    DonorReputationContract::initialize(env.clone(), admin.clone()).unwrap();
}

fn create_donor_with_projects(
    env: &Env,
    donor: &Address,
    project_count: u32,
    success_rate: i128, // in basis points
    milestones_per_project: u32,
) {
    let successful_projects = (project_count as i128 * success_rate) / BASIS_POINTS;
    
    for i in 0..project_count {
        let project_id = i + 1;
        
        // Fund project
        DonorReputationContract::record_project_funded(
            env.clone(),
            donor.clone(),
            project_id as u64,
            DEFAULT_MIN_FUNDING_THRESHOLD,
            milestones_per_project,
        ).unwrap();

        // Complete milestones based on success rate
        let should_succeed = (i as i128) < successful_projects;
        if should_succeed {
            for milestone in 0..milestones_per_project {
                DonorReputationContract::record_milestone_completed(
                    env.clone(),
                    project_id as u64,
                    milestone,
                    None,
                ).unwrap();
            }
        } else {
            // Mark project as failed
            DonorReputationContract::record_project_failed(env.clone(), project_id as u64).unwrap();
        }
    }
}

#[test]
fn test_linear_influence_scaling_tiers() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    // Test different donor tiers with linear scaling
    let test_cases = vec![
        (0,    0),      // 0% success rate -> 0x influence
        (25,   25),     // 25% success rate -> 1.25x influence  
        (50,   50),     // 50% success rate -> 1.5x influence
        (75,   75),     // 75% success rate -> 1.75x influence
        (100,  100),    // 100% success rate -> 2x influence (default max)
    ];

    for (success_rate_bps, expected_multiplier_bps) in test_cases {
        let donor = Address::generate(&env);
        
        // Create donor with specific success rate
        create_donor_with_projects(&env, &donor, 4, success_rate_bps, 3);
        
        let reputation = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
        let influence = DonorReputationContract::calculate_influence(env.clone(), donor.clone()).unwrap();
        
        // Verify success rate
        assert_eq!(reputation.success_rate, success_rate_bps, 
            "Success rate mismatch for {}%", success_rate_bps / 100);
        
        // Verify linear scaling: influence = 1x + (success_rate * (max-1x))
        let expected_influence = REPUTATION_SCALE + 
            (success_rate_bps * (MAX_REPUTATION_MULTIPLIER - REPUTATION_SCALE) / BASIS_POINTS);
        
        assert_eq!(influence, expected_influence, 
            "Influence scaling not linear for {}% success rate", success_rate_bps / 100);
        
        // Verify reputation score matches success rate
        let expected_reputation_score = success_rate_bps * REPUTATION_SCALE / BASIS_POINTS;
        assert_eq!(reputation.reputation_score, expected_reputation_score,
            "Reputation score incorrect for {}% success rate", success_rate_bps / 100);
    }
}

#[test]
fn test_minimum_funding_threshold_prevents_farming() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let donor = Address::generate(&env);
    let below_threshold = DEFAULT_MIN_FUNDING_THRESHOLD - 1;
    let above_threshold = DEFAULT_MIN_FUNDING_THRESHOLD;

    // Fund project below threshold - should not accrue reputation
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        below_threshold,
        3,
    ).unwrap();

    // Complete all milestones
    for i in 0..3 {
        DonorReputationContract::record_milestone_completed(env.clone(), 1, i, None).unwrap();
    }

    // Should not have reputation due to below-threshold funding
    let result = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone());
    assert!(result.is_err(), "Reputation should not accrue for below-threshold funding");

    // Fund another project above threshold
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        2,
        above_threshold,
        2,
    ).unwrap();

    // Complete milestones for above-threshold project
    for i in 0..2 {
        DonorReputationContract::record_milestone_completed(env.clone(), 2, i, None).unwrap();
    }

    // Should now have reputation based only on above-threshold project
    let reputation = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
    assert_eq!(reputation.qualifying_projects, 1, "Only qualifying projects should count");
    assert_eq!(reputation.success_rate, BASIS_POINTS, "Should have 100% success rate");
    assert_eq!(reputation.total_funded, above_threshold, "Should only count qualifying funding");
}

#[test]
fn test_reputation_farming_resistance() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let farmer = Address::generate(&env);
    
    // Attempt reputation farming with many small projects
    let micro_grant_amount = DEFAULT_MIN_FUNDING_THRESHOLD;
    let project_count = 10;
    
    for i in 0..project_count {
        let project_id = i + 1;
        
        // Fund project at minimum threshold
        DonorReputationContract::record_project_funded(
            env.clone(),
            farmer.clone(),
            project_id,
            micro_grant_amount,
            1, // Single milestone for quick completion
        ).unwrap();

        // Complete the single milestone
        DonorReputationContract::record_milestone_completed(env.clone(), project_id, 0, None).unwrap();
    }

    let reputation = DonorReputationContract::get_donor_reputation(env.clone(), farmer.clone()).unwrap();
    
    // Verify all projects counted (they meet minimum threshold)
    assert_eq!(reputation.qualifying_projects, project_count as u32);
    assert_eq!(reputation.successful_projects, project_count as u32);
    assert_eq!(reputation.success_rate, BASIS_POINTS); // 100% success rate
    
    // But influence is capped at maximum
    let influence = DonorReputationContract::calculate_influence(env.clone(), farmer.clone()).unwrap();
    assert_eq!(influence, MAX_REPUTATION_MULTIPLIER, "Influence should be capped at maximum");
    
    // Verify total funding is tracked correctly
    assert_eq!(reputation.total_funded, micro_grant_amount * project_count as i128);
}

#[test]
fn test_partial_milestone_completion() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let donor = Address::generate(&env);
    
    // Fund project with 5 milestones
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        5,
    ).unwrap();

    // Complete only 3 out of 5 milestones
    for i in 0..3 {
        DonorReputationContract::record_milestone_completed(env.clone(), 1, i, None).unwrap();
    }

    let metrics = DonorReputationContract::get_project_metrics(env.clone(), 1).unwrap();
    assert_eq!(metrics.completed_milestones, 3);
    assert_eq!(metrics.project_status, GrantStatus::Active); // Not completed yet
    
    // Mark project as failed (simulating abandoned project)
    DonorReputationContract::record_project_failed(env.clone(), 1).unwrap();
    
    let updated_metrics = DonorReputationContract::get_project_metrics(env.clone(), 1).unwrap();
    assert_eq!(updated_metrics.project_status, GrantStatus::Cancelled);
    
    // Donor should have 0% success rate for this project
    let reputation = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
    assert_eq!(reputation.success_rate, 0, "Incomplete project should result in 0% success rate");
}

#[test]
fn test_influence_math_fairness() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    // Test mathematical fairness across different scenarios
    let test_scenarios = vec![
        // (projects, success_rate, expected_influence_multiplier)
        (1,  100, 200),   // 1 project, 100% success -> 2x influence
        (2,  100, 200),   // 2 projects, 100% success -> 2x influence (same max)
        (5,  100, 200),   // 5 projects, 100% success -> 2x influence (same max)
        (10, 50,  150),   // 10 projects, 50% success -> 1.5x influence
        (3,  25,  125),   // 3 projects, 25% success -> 1.25x influence
        (1,  0,   100),   // 1 project, 0% success -> 1x influence (baseline)
    ];

    for (project_count, success_rate_bps, expected_influence_bps) in test_scenarios {
        let donor = Address::generate(&env);
        
        create_donor_with_projects(&env, &donor, project_count, success_rate_bps, 2);
        
        let reputation = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
        let influence = DonorReputationContract::calculate_influence(env.clone(), donor.clone()).unwrap();
        
        let expected_influence = expected_influence_bps * REPUTATION_SCALE / 100;
        assert_eq!(influence, expected_influence,
            "Influence mismatch for {} projects with {}% success rate", 
            project_count, success_rate_bps / 100);
        
        // Verify fairness: same success rate should give same influence regardless of project count
        let expected_success_rate = success_rate_bps;
        assert_eq!(reputation.success_rate, expected_success_rate,
            "Success rate should be consistent regardless of project count");
    }
}

#[test]
fn test_reputation_update_history() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let donor = Address::generate(&env);
    
    // Fund first project
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        2,
    ).unwrap();

    // Complete first milestone
    DonorReputationContract::record_milestone_completed(env.clone(), 1, 0, None).unwrap();
    
    // Complete second milestone (project finished)
    DonorReputationContract::record_milestone_completed(env.clone(), 1, 1, None).unwrap();

    // Fund second project
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        2,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        1,
    ).unwrap();

    // Check update history
    let mut update_count = 0;
    for i in 1..=10 { // Check first 10 updates
        if let Ok(update) = DonorReputationContract::get_reputation_update(env.clone(), i) {
            assert_eq!(update.donor, donor);
            assert!(update.update_id > 0);
            update_count += 1;
        } else {
            break;
        }
    }

    assert!(update_count >= 3, "Should have at least 3 reputation updates");
}

#[test]
fn test_configuration_updates() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    // Update configuration
    let new_threshold = DEFAULT_MIN_FUNDING_THRESHOLD * 2;
    let new_max_multiplier = REPUTATION_SCALE * 4; // 4x max influence
    
    DonorReputationContract::update_config(
        env.clone(),
        admin.clone(),
        Some(new_threshold),
        Some(new_max_multiplier),
        None,
        None,
    ).unwrap();

    let config = DonorReputationContract::get_reputation_config(env.clone());
    assert_eq!(config.min_funding_threshold, new_threshold);
    assert_eq!(config.max_multiplier, new_max_multiplier);

    // Test new configuration with a donor
    let donor = Address::generate(&env);
    
    // Fund with old threshold (should not qualify now)
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        DEFAULT_MIN_FUNDING_THRESHOLD, // Below new threshold
        2,
    ).unwrap();

    for i in 0..2 {
        DonorReputationContract::record_milestone_completed(env.clone(), 1, i, None).unwrap();
    }

    // Should not have reputation due to increased threshold
    let result = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone());
    assert!(result.is_err(), "Should not have reputation with increased threshold");

    // Fund with new threshold
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        2,
        new_threshold,
        2,
    ).unwrap();

    for i in 0..2 {
        DonorReputationContract::record_milestone_completed(env.clone(), 2, i, None).unwrap();
    }

    let reputation = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
    let influence = DonorReputationContract::calculate_influence(env.clone(), donor.clone()).unwrap();
    
    assert_eq!(influence, new_max_multiplier, "Should use new max multiplier");
}

#[test]
fn test_edge_cases_and_error_handling() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let donor = Address::generate(&env);

    // Test invalid project funding
    let result = DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        0, // Invalid amount
        2,
    );
    assert!(result.is_err());

    let result = DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        0, // Invalid milestone count
    );
    assert!(result.is_err());

    // Test invalid milestone completion
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        2,
    ).unwrap();

    // Try to complete non-existent milestone
    let result = DonorReputationContract::record_milestone_completed(env.clone(), 1, 5, None);
    assert!(result.is_err());

    // Try to complete same milestone twice
    DonorReputationContract::record_milestone_completed(env.clone(), 1, 0, None).unwrap();
    let result = DonorReputationContract::record_milestone_completed(env.clone(), 1, 0, None);
    assert!(result.is_err());

    // Test non-existent donor reputation
    let unknown_donor = Address::generate(&env);
    let result = DonorReputationContract::get_donor_reputation(env.clone(), unknown_donor.clone());
    assert!(result.is_err());

    let result = DonorReputationContract::calculate_influence(env.clone(), unknown_donor);
    assert!(result.is_err());
}

#[test]
fn test_multiple_donors_independence() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let donor1 = Address::generate(&env);
    let donor2 = Address::generate(&env);
    let donor3 = Address::generate(&env);

    // Create donors with different performance
    create_donor_with_projects(&env, &donor1, 3, 100, 2); // Perfect
    create_donor_with_projects(&env, &donor2, 3, 50, 2);  // Average
    create_donor_with_projects(&env, &donor3, 3, 0, 2);   // Poor

    let rep1 = DonorReputationContract::get_donor_reputation(env.clone(), donor1.clone()).unwrap();
    let rep2 = DonorReputationContract::get_donor_reputation(env.clone(), donor2.clone()).unwrap();
    let rep3 = DonorReputationContract::get_donor_reputation(env.clone(), donor3.clone()).unwrap();

    // Verify independence and correct calculations
    assert_eq!(rep1.success_rate, BASIS_POINTS);
    assert_eq!(rep2.success_rate, 50 * BASIS_POINTS / 100);
    assert_eq!(rep3.success_rate, 0);

    let inf1 = DonorReputationContract::calculate_influence(env.clone(), donor1).unwrap();
    let inf2 = DonorReputationContract::calculate_influence(env.clone(), donor2).unwrap();
    let inf3 = DonorReputationContract::calculate_influence(env.clone(), donor3).unwrap();

    assert_eq!(inf1, MAX_REPUTATION_MULTIPLIER);
    assert_eq!(inf3, REPUTATION_SCALE); // Baseline influence
    assert!(inf2 > inf3 && inf2 < inf1, "Average donor should have influence between poor and perfect");
}

#[test] 
fn test_reputation_persistence_across_operations() {
    let (env, admin) = create_test_env();
    initialize_reputation_system(&env, &admin);

    let donor = Address::generate(&env);

    // Build reputation over multiple operations
    // Project 1: Success
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        1,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        2,
    ).unwrap();
    DonorReputationContract::record_milestone_completed(env.clone(), 1, 0, None).unwrap();
    DonorReputationContract::record_milestone_completed(env.clone(), 1, 1, None).unwrap();

    let rep_after_1 = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
    assert_eq!(rep_after_1.successful_projects, 1);
    assert_eq!(rep_after_1.qualifying_projects, 1);

    // Project 2: Failure
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        2,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        2,
    ).unwrap();
    DonorReputationContract::record_project_failed(env.clone(), 2).unwrap();

    let rep_after_2 = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
    assert_eq!(rep_after_2.successful_projects, 1);
    assert_eq!(rep_after_2.qualifying_projects, 2);
    assert_eq!(rep_after_2.success_rate, 50 * BASIS_POINTS / 100); // 1/2 = 50%

    // Project 3: Success
    DonorReputationContract::record_project_funded(
        env.clone(),
        donor.clone(),
        3,
        DEFAULT_MIN_FUNDING_THRESHOLD,
        1,
    ).unwrap();
    DonorReputationContract::record_milestone_completed(env.clone(), 3, 0, None).unwrap();

    let final_rep = DonorReputationContract::get_donor_reputation(env.clone(), donor.clone()).unwrap();
    assert_eq!(final_rep.successful_projects, 2);
    assert_eq!(final_rep.qualifying_projects, 3);
    assert_eq!(final_rep.success_rate, 67 * BASIS_POINTS / 100); // 2/3 ≈ 66.67%
    assert_eq!(final_rep.total_funded, DEFAULT_MIN_FUNDING_THRESHOLD * 3);
}
