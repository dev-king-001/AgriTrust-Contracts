use soroban_sdk::{
    contracterror, contracttype, symbol_short, Address, Bytes, BytesN,
    Env, Symbol, Vec, String,
};
#[cfg(test)]
use soroban_sdk::testutils::Ledger;
use soroban_sdk::xdr::ToXdr;

pub struct GrantContract;

// Bitwise status flags for grant optimization
// Each flag represents 1 bit in a u32 status mask
pub const STATUS_ACTIVE: u32 = 0b000000001;     // Grant is currently active
pub const STATUS_PAUSED: u32 = 0b000000010;     // Grant is paused
pub const STATUS_COMPLETED: u32 = 0b000000100;  // Grant is completed
pub const STATUS_CANCELLED: u32 = 0b000001000;  // Grant is cancelled
pub const STATUS_REVOCABLE: u32 = 0b000010000;  // Grant can be revoked
pub const STATUS_MILESTONE_BASED: u32 = 0b000100000; // Grant uses milestone-based releases
pub const STATUS_AUTO_RENEW: u32 = 0b001000000; // Grant auto-renews
pub const STATUS_EMERGENCY_PAUSE: u32 = 0b010000000; // Grant is emergency paused
pub const STATUS_RAGE_QUIT: u32 = 0b100000000; // Grantee rage quit; grant permanently closed (issue #39)

// Helper functions for bitwise operations
pub fn has_status(status_mask: u32, flag: u32) -> bool {
    (status_mask & flag) != 0
}

pub fn set_status(status_mask: u32, flag: u32) -> u32 {
    status_mask | flag
}

pub fn clear_status(status_mask: u32, flag: u32) -> u32 {
    status_mask & !flag
}

pub fn toggle_status(status_mask: u32, flag: u32) -> u32 {
    status_mask ^ flag
}

#[derive(Clone)]
#[contracttype]
pub struct Grant {
    pub recipient: Address,
    pub total_amount: i128,
    pub withdrawn: i128,
    pub claimable: i128,
    pub flow_rate: i128,
    pub last_update_ts: u64,
    pub rate_updated_at: u64,
    pub status_mask: u32, // Replaces multiple boolean fields with single u32
    // Milestone support (issue #40)
    pub milestone_deadline: u64, // unix timestamp after which clawback is allowed
    pub milestone_met: bool,     // whether milestone was approved/met by DAO
    pub milestone_threshold: u32,
    pub milestone_dispute_window_secs: u64,
    pub milestone_evidence_hash: Option<BytesN<32>>,
    pub ms_evidence_submitted_at: u64,
    pub milestone_dispute_window_end: u64,
    pub milestone_oracles: Vec<BytesN<32>>,
    pub milestone_approvers: Vec<BytesN<32>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[contracttype]
pub struct MilestoneConsensusState {
    pub evidence_hash: Option<BytesN<32>>,
    pub threshold: u32,
    pub dispute_window_secs: u64,
    pub dispute_window_end: u64,
    pub oracles: Vec<BytesN<32>>,
    pub approvers: Vec<BytesN<32>>,
    pub is_completed: bool,
}

#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    Grant(u64),
    MilestoneEvidence(u64),
}

#[contracterror]
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
#[repr(u32)]
pub enum Error {
    NotInitialized = 1,
    AlreadyInitialized = 2,
    NotAuthorized = 3,
    GrantNotFound = 4,
    GrantAlreadyExists = 5,
    InvalidRate = 6,
    InvalidAmount = 7,
    InvalidState = 8,
    MathOverflow = 9,
    InvalidStatusTransition = 10, // New error for invalid status transitions
    SoftPaused = 11,
    OracleFrozen = 12,
    InvalidMilestoneConfig = 13,
    MilestoneEvidenceMissing = 14,
    MilestoneAlreadyCompleted = 15,
    MilestoneWindowClosed = 16,
    OracleNotWhitelisted = 17,
    DuplicateOracleApproval = 18,
    EvidenceRequired = 19,
}

pub fn read_admin(env: &Env) -> Result<Address, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(Error::NotInitialized)
}

pub fn require_admin_auth(env: &Env) -> Result<(), Error> {
    let admin = read_admin(env)?;
    admin.require_auth();
    Ok(())
}

pub fn read_grant(env: &Env, grant_id: u64) -> Result<Grant, Error> {
    env.storage()
        .instance()
        .get(&DataKey::Grant(grant_id))
        .ok_or(Error::GrantNotFound)
}

pub fn write_grant(env: &Env, grant_id: u64, grant: &Grant) {
    env.storage().instance().set(&DataKey::Grant(grant_id), grant);
}

fn read_milestone_evidence(env: &Env, grant_id: u64) -> Option<String> {
    env.storage().instance().get(&DataKey::MilestoneEvidence(grant_id))
}

fn write_milestone_evidence(env: &Env, grant_id: u64, anchor: &String) {
    env.storage()
        .instance()
        .set(&DataKey::MilestoneEvidence(grant_id), anchor);
}

fn clear_milestone_evidence(env: &Env, grant_id: u64) {
    env.storage().instance().remove(&DataKey::MilestoneEvidence(grant_id));
}

fn maybe_auto_cancel_milestone_expiry(
    env: &Env,
    grant_id: u64,
    grant: &mut Grant,
) -> Result<bool, Error> {
    if !has_status(grant.status_mask, STATUS_MILESTONE_BASED) {
        return Ok(false);
    }
    if has_status(grant.status_mask, STATUS_CANCELLED)
        || has_status(grant.status_mask, STATUS_COMPLETED)
        || has_status(grant.status_mask, STATUS_RAGE_QUIT)
    {
        return Ok(false);
    }

    let now = env.ledger().timestamp();
    if grant.milestone_deadline == 0 || now <= grant.milestone_deadline || grant.milestone_met {
        return Ok(false);
    }

    let cancel_settle_ts = if grant.last_update_ts < grant.milestone_deadline {
        grant.milestone_deadline
    } else {
        grant.last_update_ts
    };
    settle_grant(grant, cancel_settle_ts)?;
    grant.status_mask = set_status(grant.status_mask, STATUS_CANCELLED);
    grant.status_mask = clear_status(grant.status_mask, STATUS_ACTIVE);
    grant.status_mask = clear_status(grant.status_mask, STATUS_PAUSED);
    grant.flow_rate = 0;
    write_grant(env, grant_id, grant);
    env.events().publish(
        (symbol_short!("milexpir"), grant_id),
        (grant.milestone_deadline, cancel_settle_ts),
    );
    Ok(true)
}

pub fn validate_status_transition(current_mask: u32, new_mask: u32) -> Result<(), Error> {
    if has_status(current_mask, STATUS_COMPLETED) || has_status(current_mask, STATUS_CANCELLED) {
        return Err(Error::InvalidStatusTransition);
    }
    
    // Validate specific transitions
    match (current_mask, new_mask) {
        // From any state to cancelled
        (_, new) if has_status(new, STATUS_CANCELLED) => Ok(()),
        
        // From active to paused
        (current, new) if has_status(current, STATUS_ACTIVE) && has_status(new, STATUS_PAUSED) 
            && !has_status(new, STATUS_ACTIVE) => Ok(()),
        
        // From paused to active
        (current, new) if has_status(current, STATUS_PAUSED) && has_status(new, STATUS_ACTIVE) 
            && !has_status(new, STATUS_PAUSED) => Ok(()),
        
        // From active/paused to completed
        (current, new) if (has_status(current, STATUS_ACTIVE) || has_status(current, STATUS_PAUSED)) 
            && has_status(new, STATUS_COMPLETED) 
            && !has_status(new, STATUS_ACTIVE) && !has_status(new, STATUS_PAUSED) => Ok(()),
        
        // Initial creation (must be active)
        (0, new) if has_status(new, STATUS_ACTIVE) 
            && !has_status(new, STATUS_PAUSED) && !has_status(new, STATUS_COMPLETED) && !has_status(new, STATUS_CANCELLED) => Ok(()),
        
        // Invalid transitions
        _ => Err(Error::InvalidStatusTransition),
    }
}

pub fn settle_grant(grant: &mut Grant, now: u64) -> Result<(), Error> {
    if now < grant.last_update_ts {
        return Err(Error::InvalidState);
    }

    let elapsed = now - grant.last_update_ts;
    grant.last_update_ts = now;

    // Only accrue if grant is active (not paused, completed, or cancelled)
    if !has_status(grant.status_mask, STATUS_ACTIVE) || elapsed == 0 || grant.flow_rate == 0 {
        return Ok(());
    }

    if grant.flow_rate < 0 {
        return Err(Error::InvalidRate);
    }

    let elapsed_i128 = i128::from(elapsed);
    let accrued = grant
        .flow_rate
        .checked_mul(elapsed_i128)
        .ok_or(Error::MathOverflow)?;

    let accounted = grant
        .withdrawn
        .checked_add(grant.claimable)
        .ok_or(Error::MathOverflow)?;

    if accounted > grant.total_amount {
        return Err(Error::InvalidState);
    }

    let remaining = grant
        .total_amount
        .checked_sub(accounted)
        .ok_or(Error::MathOverflow)?;

    let delta = if accrued > remaining {
        remaining
    } else {
        accrued
    };

    grant.claimable = grant
        .claimable
        .checked_add(delta)
        .ok_or(Error::MathOverflow)?;

    let new_accounted = grant
        .withdrawn
        .checked_add(grant.claimable)
        .ok_or(Error::MathOverflow)?;

    if new_accounted == grant.total_amount {
        // Mark as completed
        grant.status_mask = set_status(grant.status_mask, STATUS_COMPLETED);
        grant.status_mask = clear_status(grant.status_mask, STATUS_ACTIVE);
    }

    Ok(())
}

fn contains_oracle(oracles: &Vec<BytesN<32>>, oracle: &BytesN<32>) -> bool {
    for i in 0..oracles.len() {
        if oracles.get(i).unwrap() == *oracle {
            return true;
        }
    }
    false
}

fn validate_oracle_set(oracles: &Vec<BytesN<32>>, threshold: u32) -> Result<(), Error> {
    if oracles.is_empty() || threshold == 0 || threshold as u32 > oracles.len() {
        return Err(Error::InvalidMilestoneConfig);
    }

    for i in 0..oracles.len() {
        let oracle = oracles.get(i).unwrap();
        for j in (i + 1)..oracles.len() {
            if oracle == oracles.get(j).unwrap() {
                return Err(Error::InvalidMilestoneConfig);
            }
        }
    }

    Ok(())
}

fn build_milestone_approval_payload(
    env: &Env,
    grant_id: u64,
    evidence_hash: &BytesN<32>,
    dispute_window_end: u64,
) -> Bytes {
    let mut payload = Bytes::from_array(env, b"milestone-approval-v1");
    payload.append(&env.current_contract_address().to_xdr(env));
    payload.append(&Bytes::from_array(env, &grant_id.to_be_bytes()));
    payload.append(&Bytes::from_array(env, &dispute_window_end.to_be_bytes()));
    let b: soroban_sdk::Bytes = evidence_hash.clone().into();
    payload.append(&b);
    payload
}

fn milestone_state(grant: &Grant) -> MilestoneConsensusState {
    MilestoneConsensusState {
        evidence_hash: grant.milestone_evidence_hash.clone(),
        threshold: grant.milestone_threshold,
        dispute_window_secs: grant.milestone_dispute_window_secs,
        dispute_window_end: grant.milestone_dispute_window_end,
        oracles: grant.milestone_oracles.clone(),
        approvers: grant.milestone_approvers.clone(),
        is_completed: grant.milestone_met,
    }
}

fn preview_grant_at_now(env: &Env, grant: &Grant) -> Result<Grant, Error> {
    let mut preview = grant.clone();
    settle_grant(&mut preview, env.ledger().timestamp())?;
    Ok(preview)
}

fn emit_grant_snapshot(env: &Env, grant_id: u64, grant: &Grant) {
    env.events().publish(
        (symbol_short!("snapshot"), grant_id),
        (grant.flow_rate, grant.claimable, grant.status_mask, grant.last_update_ts),
    );
}

impl GrantContract {
    pub fn initialize(env: Env, admin: Address) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Ok(())
    }

    pub fn create_grant(
        env: Env,
        grant_id: u64,
        recipient: Address,
        total_amount: i128,
        flow_rate: i128,
        initial_status_mask: u32, // Allow setting initial flags
    ) -> Result<(), Error> {
        require_admin_auth(&env)?;

        if total_amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        if flow_rate < 0 {
            return Err(Error::InvalidRate);
        }

        // Validate initial status
        validate_status_transition(0, initial_status_mask)?;

        let key = DataKey::Grant(grant_id);
        if env.storage().instance().has(&key) {
            return Err(Error::GrantAlreadyExists);
        }

        let now = env.ledger().timestamp();
        let grant = Grant {
            recipient,
            total_amount,
            withdrawn: 0,
            claimable: 0,
            flow_rate,
            last_update_ts: now,
            rate_updated_at: now,
            status_mask: initial_status_mask,
            milestone_deadline: 0,
            milestone_met: false,
            milestone_threshold: 0,
            milestone_dispute_window_secs: 0,
            milestone_evidence_hash: None,
            ms_evidence_submitted_at: 0,
            milestone_dispute_window_end: 0,
            milestone_oracles: Vec::new(&env),
            milestone_approvers: Vec::new(&env),
        };

        env.storage().instance().set(&key, &grant);
        emit_grant_snapshot(&env, grant_id, &grant);
        Ok(())
    }

    pub fn cancel_grant_bitmask(env: Env, grant_id: u64) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;

        let current_mask = grant.status_mask;
        let new_mask = set_status(current_mask, STATUS_CANCELLED);
        
        // Validate transition
        validate_status_transition(current_mask, new_mask)?;

        settle_grant(&mut grant, env.ledger().timestamp())?;
        grant.status_mask = new_mask;
        grant.flow_rate = 0; // Stop flow rate

        write_grant(&env, grant_id, &grant);
        emit_grant_snapshot(&env, grant_id, &grant);
        Ok(())
    }

    pub fn pause_grant(env: Env, grant_id: u64) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;

        let current_mask = grant.status_mask;
        
        // Can only pause active grants
        if !has_status(current_mask, STATUS_ACTIVE) {
            return Err(Error::InvalidState);
        }

        let mut new_mask = set_status(current_mask, STATUS_PAUSED);
        new_mask = clear_status(new_mask, STATUS_ACTIVE);

        settle_grant(&mut grant, env.ledger().timestamp())?;
        grant.status_mask = new_mask;

        write_grant(&env, grant_id, &grant);
        emit_grant_snapshot(&env, grant_id, &grant);
        Ok(())
    }

    pub fn resume_grant(env: Env, grant_id: u64) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;

        let current_mask = grant.status_mask;
        
        // Can only resume paused grants
        if !has_status(current_mask, STATUS_PAUSED) {
            return Err(Error::InvalidState);
        }

        let mut new_mask = set_status(current_mask, STATUS_ACTIVE);
        new_mask = clear_status(new_mask, STATUS_PAUSED);

        settle_grant(&mut grant, env.ledger().timestamp())?;
        grant.status_mask = new_mask;

        write_grant(&env, grant_id, &grant);
        emit_grant_snapshot(&env, grant_id, &grant);
        Ok(())
    }

    pub fn set_grant_flags(
        env: Env, 
        grant_id: u64, 
        flags_to_set: u32, 
        flags_to_clear: u32
    ) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;

        let current_mask = grant.status_mask;
        let new_mask = (current_mask | flags_to_set) & !flags_to_clear;
        
        // Validate that we're not making invalid transitions
        validate_status_transition(current_mask, new_mask)?;

        settle_grant(&mut grant, env.ledger().timestamp())?;
        grant.status_mask = new_mask;

        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    pub fn get_grant(env: Env, grant_id: u64) -> Result<Grant, Error> {
        let mut grant = read_grant(&env, grant_id)?;
        let _ = maybe_auto_cancel_milestone_expiry(&env, grant_id, &mut grant)?;
        preview_grant_at_now(&env, &grant)
    }

    pub fn get_grant_status(env: Env, grant_id: u64) -> Result<u32, Error> {
        let grant = read_grant(&env, grant_id)?;
        Ok(grant.status_mask)
    }

    pub fn is_grant_active(env: Env, grant_id: u64) -> Result<bool, Error> {
        let grant = read_grant(&env, grant_id)?;
        preview_grant_at_now(&env, &grant)?;
        Ok(has_status(grant.status_mask, STATUS_ACTIVE))
    }

    pub fn is_grant_paused(env: Env, grant_id: u64) -> Result<bool, Error> {
        let grant = read_grant(&env, grant_id)?;
        preview_grant_at_now(&env, &grant)?;
        Ok(has_status(grant.status_mask, STATUS_PAUSED))
    }

    pub fn is_grant_completed(env: Env, grant_id: u64) -> Result<bool, Error> {
        let grant = read_grant(&env, grant_id)?;
        preview_grant_at_now(&env, &grant)?;
        Ok(has_status(grant.status_mask, STATUS_COMPLETED))
    }

    pub fn is_grant_cancelled(env: Env, grant_id: u64) -> Result<bool, Error> {
        let grant = read_grant(&env, grant_id)?;
        preview_grant_at_now(&env, &grant)?;
        Ok(has_status(grant.status_mask, STATUS_CANCELLED))
    }

    pub fn claimable_bitmask(env: Env, grant_id: u64) -> Result<i128, Error> {
        let mut grant = read_grant(&env, grant_id)?;
        let _ = maybe_auto_cancel_milestone_expiry(&env, grant_id, &mut grant)?;
        let preview = preview_grant_at_now(&env, &grant)?;
        Ok(preview.claimable)
    }

    pub fn withdraw(env: Env, grant_id: u64, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let mut grant = read_grant(&env, grant_id)?;
        let _ = maybe_auto_cancel_milestone_expiry(&env, grant_id, &mut grant)?;

        // Can only withdraw from active grants
        if !has_status(grant.status_mask, STATUS_ACTIVE) {
            return Err(Error::InvalidState);
        }

        grant.recipient.require_auth();

        settle_grant(&mut grant, env.ledger().timestamp())?;

        if amount > grant.claimable {
            return Err(Error::InvalidAmount);
        }

        grant.claimable = grant
            .claimable
            .checked_sub(amount)
            .ok_or(Error::MathOverflow)?;
        grant.withdrawn = grant
            .withdrawn
            .checked_add(amount)
            .ok_or(Error::MathOverflow)?;

        let accounted = grant
            .withdrawn
            .checked_add(grant.claimable)
            .ok_or(Error::MathOverflow)?;

        if accounted == grant.total_amount {
            grant.status_mask = set_status(grant.status_mask, STATUS_COMPLETED);
            grant.status_mask = clear_status(grant.status_mask, STATUS_ACTIVE);
        }

        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    /// Auto-cancel milestone-based grants that missed deadline without approval.
    /// Returns `true` when cancellation was applied, `false` otherwise.
    pub fn process_milestone_expiry(env: Env, grant_id: u64) -> Result<bool, Error> {
        let mut grant = read_grant(&env, grant_id)?;
        maybe_auto_cancel_milestone_expiry(&env, grant_id, &mut grant)
    }

    pub fn update_rate(env: Env, grant_id: u64, new_rate: i128) -> Result<(), Error> {
        require_admin_auth(&env)?;

        if new_rate < 0 {
            return Err(Error::InvalidRate);
        }

        let mut grant = read_grant(&env, grant_id)?;
        
        // Can only update rate for active or paused grants
        if !has_status(grant.status_mask, STATUS_ACTIVE) && !has_status(grant.status_mask, STATUS_PAUSED) {
            return Err(Error::InvalidState);
        }

        let old_rate = grant.flow_rate;

        settle_grant(&mut grant, env.ledger().timestamp())?;
        
        if !has_status(grant.status_mask, STATUS_ACTIVE) && !has_status(grant.status_mask, STATUS_PAUSED) {
            write_grant(&env, grant_id, &grant);
            return Err(Error::InvalidState);
        }

        grant.flow_rate = new_rate;
        grant.rate_updated_at = grant.last_update_ts;

        write_grant(&env, grant_id, &grant);

        env.events().publish(
            (symbol_short!("rateupdt"), grant_id),
            (old_rate, new_rate, grant.rate_updated_at),
        );
        emit_grant_snapshot(&env, grant_id, &grant);

        Ok(())
    }

    /// Set the milestone deadline for a milestone-based grant. Deadline is a
    /// UNIX timestamp after which the DAO may claw back unwithdrawn funds if
    /// the milestone has not been met. Only the admin may call this, and the
    /// grant must have the `STATUS_MILESTONE_BASED` flag.
    pub fn set_milestone_deadline(env: Env, grant_id: u64, deadline: u64) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;
        if !has_status(grant.status_mask, STATUS_MILESTONE_BASED) {
            return Err(Error::InvalidState);
        }
        grant.milestone_deadline = deadline;
        grant.milestone_met = false;
        clear_milestone_evidence(&env, grant_id);
        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    /// Configure the oracle whitelist and approval threshold for a
    /// milestone-based grant.
    pub fn configure_milestone_oracles(
        env: Env,
        grant_id: u64,
        oracles: Vec<BytesN<32>>,
        threshold: u32,
        dispute_window_secs: u64,
    ) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;
        if !has_status(grant.status_mask, STATUS_MILESTONE_BASED) || dispute_window_secs == 0 {
            return Err(Error::InvalidMilestoneConfig);
        }
        validate_oracle_set(&oracles, threshold)?;

        grant.milestone_oracles = oracles;
        grant.milestone_threshold = threshold;
        grant.milestone_dispute_window_secs = dispute_window_secs;
        grant.milestone_evidence_hash = None;
        grant.ms_evidence_submitted_at = 0;
        grant.milestone_dispute_window_end = 0;
        grant.milestone_approvers = Vec::new(&env);
        grant.milestone_met = false;

        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    /// Store the evidence hash in persistent storage and start the dispute
    /// window for oracle approvals.
    pub fn submit_milestone_evidence(
        env: Env,
        grant_id: u64,
        evidence_hash: BytesN<32>,
    ) -> Result<(), Error> {
        let mut grant = read_grant(&env, grant_id)?;
        if !has_status(grant.status_mask, STATUS_MILESTONE_BASED)
            || grant.milestone_threshold == 0
            || grant.milestone_oracles.is_empty()
        {
            return Err(Error::InvalidMilestoneConfig);
        }

        grant.recipient.require_auth();

        let now = env.ledger().timestamp();
        grant.milestone_evidence_hash = Some(evidence_hash);
        grant.ms_evidence_submitted_at = now;
        grant.milestone_dispute_window_end = now + grant.milestone_dispute_window_secs;
        grant.milestone_approvers = Vec::new(&env);
        grant.milestone_met = false;

        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    /// Verify an oracle signature and record its approval. Consensus is reached
    /// once the threshold of distinct whitelisted oracles has signed.
    pub fn approve_milestone(
        env: Env,
        grant_id: u64,
        oracle_public_key: BytesN<32>,
        signature: BytesN<64>,
    ) -> Result<u32, Error> {
        let mut grant = read_grant(&env, grant_id)?;
        if !has_status(grant.status_mask, STATUS_MILESTONE_BASED) {
            return Err(Error::InvalidState);
        }
        if grant.milestone_met {
            return Err(Error::MilestoneAlreadyCompleted);
        }
        if !contains_oracle(&grant.milestone_oracles, &oracle_public_key) {
            return Err(Error::OracleNotWhitelisted);
        }
        if contains_oracle(&grant.milestone_approvers, &oracle_public_key) {
            return Err(Error::DuplicateOracleApproval);
        }

        let evidence_hash = grant
            .milestone_evidence_hash
            .clone()
            .ok_or(Error::MilestoneEvidenceMissing)?;
        let now = env.ledger().timestamp();
        if now > grant.milestone_dispute_window_end {
            return Err(Error::MilestoneWindowClosed);
        }

        let payload = build_milestone_approval_payload(
            &env,
            grant_id,
            &evidence_hash,
            grant.milestone_dispute_window_end,
        );
        env.crypto()
            .ed25519_verify(&oracle_public_key, &payload, &signature);

        grant.milestone_approvers.push_back(oracle_public_key.clone());
        let approvals = grant.milestone_approvers.len();

        if approvals >= grant.milestone_threshold {
            grant.milestone_met = true;
            if has_status(grant.status_mask, STATUS_PAUSED) {
                grant.status_mask = clear_status(grant.status_mask, STATUS_PAUSED);
                grant.status_mask = set_status(grant.status_mask, STATUS_ACTIVE);
            }
            env.events().publish(
                (
                    Symbol::new(&env, "MilestoneConsensusReached"),
                    grant_id,
                ),
                grant.milestone_approvers.clone(),
            );
        }

        write_grant(&env, grant_id, &grant);
        Ok(approvals)
    }

    pub fn get_milestone_consensus(env: Env, grant_id: u64) -> Result<MilestoneConsensusState, Error> {
        let grant = read_grant(&env, grant_id)?;
        Ok(milestone_state(&grant))
    }

    /// Mark a milestone as met. After this call the admin can no longer claw
    /// back the accrued balance based on deadline. Evidence anchoring is
    /// required before approval.
    pub fn mark_milestone_met(env: Env, grant_id: u64) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;
        if !has_status(grant.status_mask, STATUS_MILESTONE_BASED) {
            return Err(Error::InvalidState);
        }
        let evidence = read_milestone_evidence(&env, grant_id)
            .ok_or(Error::EvidenceRequired)?;
        if evidence.len() == 0 {
            return Err(Error::EvidenceRequired);
        }
        grant.milestone_met = true;
        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    /// Claw back the unwithdrawn balance of a milestone-based grant if the
    /// deadline has passed and the milestone has not been met. This leaves the
    /// grant in place but resets `claimable` to zero; already withdrawn funds
    /// are unaffected.
    pub fn clawback_milestone(env: Env, grant_id: u64) -> Result<(), Error> {
        require_admin_auth(&env)?;
        let mut grant = read_grant(&env, grant_id)?;
        if !has_status(grant.status_mask, STATUS_MILESTONE_BASED) {
            return Err(Error::InvalidState);
        }
        let now = env.ledger().timestamp();
        if grant.milestone_deadline == 0 || now <= grant.milestone_deadline {
            return Err(Error::InvalidState);
        }
        if grant.milestone_met {
            return Err(Error::InvalidState);
        }
        settle_grant(&mut grant, now)?;
        grant.claimable = 0;
        write_grant(&env, grant_id, &grant);
        Ok(())
    }

    /// Grantee "rage quits" a paused grant and claims 100% of accrued funds.
    /// This permanently closes the grant and prevents the admin from resuming it.
    pub fn rage_quit(env: Env, grant_id: u64) -> Result<(), Error> {
        let mut grant = read_grant(&env, grant_id)?;
        grant.recipient.require_auth();

        // Can only rage quit a paused grant
        if !has_status(grant.status_mask, STATUS_PAUSED) {
            return Err(Error::InvalidState);
        }

        // Settle accrual up to now
        settle_grant(&mut grant, env.ledger().timestamp())?;

        // Prevent any future operation: mark as completed and set rage quit flag
        grant.status_mask = set_status(grant.status_mask, STATUS_COMPLETED);
        grant.status_mask = set_status(grant.status_mask, STATUS_RAGE_QUIT);
        grant.status_mask = clear_status(grant.status_mask, STATUS_PAUSED);
        grant.status_mask = clear_status(grant.status_mask, STATUS_ACTIVE);
        grant.flow_rate = 0; // Stop all future accrual

        // Grantee immediately claims all claimable funds
        let claimable_amount = grant.claimable;
        grant.withdrawn = grant
            .withdrawn
            .checked_add(claimable_amount)
            .ok_or(Error::MathOverflow)?;
        grant.claimable = 0;

        write_grant(&env, grant_id, &grant);

        env.events().publish(
            (symbol_short!("ragequit"), grant_id),
            claimable_amount,
        );

        Ok(())
    }
}

#[cfg(test)]
mod milestone_oracle_tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use soroban_sdk::{testutils::Address as _, Address, Env};

    fn signing_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn bytesn32(env: &Env, bytes: [u8; 32]) -> BytesN<32> {
        BytesN::from_array(env, &bytes)
    }

    fn bytesn64(env: &Env, bytes: [u8; 64]) -> BytesN<64> {
        BytesN::from_array(env, &bytes)
    }

    fn sign_approval(
        env: &Env,
        grant_id: u64,
        evidence_hash: &BytesN<32>,
        dispute_window_end: u64,
        key: &SigningKey,
    ) -> BytesN<64> {
        let payload =
            build_milestone_approval_payload(env, grant_id, evidence_hash, dispute_window_end);
        let signature = key.sign(&payload.to_alloc_vec()).to_bytes();
        bytesn64(env, signature)
    }

    fn with_contract<F: FnOnce(&Env)>(env: &Env, contract_id: &Address, f: F) {
        env.as_contract(contract_id, || f(env));
    }

    fn setup_grant(env: &Env, contract_id: &Address) -> (Address, Address) {
        let admin = Address::generate(env);
        let recipient = Address::generate(env);

        env.as_contract(contract_id, || {
            GrantContract::initialize(env.clone(), admin.clone()).unwrap();
            GrantContract::create_grant(
                env.clone(),
                7,
                recipient.clone(),
                1_000_000,
                100,
                STATUS_ACTIVE | STATUS_MILESTONE_BASED,
            )
            .unwrap();
        });

        (admin, recipient)
    }

    #[test]
    fn milestone_reaches_consensus_at_two_of_three() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::GrantStreamContract);
        let (_admin, _recipient) = setup_grant(&env, &contract_id);
        with_contract(&env, &contract_id, |env| {

            let key1 = signing_key(1);
            let key2 = signing_key(2);
            let key3 = signing_key(3);
            let oracle_keys = Vec::from_array(
                &env,
                [
                    bytesn32(&env, key1.verifying_key().to_bytes()),
                    bytesn32(&env, key2.verifying_key().to_bytes()),
                    bytesn32(&env, key3.verifying_key().to_bytes()),
                ],
            );

            GrantContract::configure_milestone_oracles(
                env.clone(),
                7,
                oracle_keys,
                2,
                300,
            )
            .unwrap();

            let evidence_hash = bytesn32(&env, [9; 32]);
            GrantContract::submit_milestone_evidence(env.clone(), 7, evidence_hash.clone()).unwrap();

            let state = GrantContract::get_milestone_consensus(env.clone(), 7).unwrap();
            assert_eq!(state.approvers.len(), 0);
            assert!(!state.is_completed);

            let sig1 = sign_approval(&env, 7, &evidence_hash, state.dispute_window_end, &key1);
            let sig2 = sign_approval(&env, 7, &evidence_hash, state.dispute_window_end, &key2);

            assert_eq!(
                GrantContract::approve_milestone(
                    env.clone(),
                    7,
                    bytesn32(&env, key1.verifying_key().to_bytes()),
                    sig1,
                )
                .unwrap(),
                1
            );

            let interim = GrantContract::get_milestone_consensus(env.clone(), 7).unwrap();
            assert_eq!(interim.approvers.len(), 1);
            assert!(!interim.is_completed);

            assert_eq!(
                GrantContract::approve_milestone(
                    env.clone(),
                    7,
                    bytesn32(&env, key2.verifying_key().to_bytes()),
                    sig2,
                )
                .unwrap(),
                2
            );

            let finalized = GrantContract::get_milestone_consensus(env.clone(), 7).unwrap();
            assert_eq!(finalized.approvers.len(), 2);
            assert!(finalized.is_completed);

            let grant = GrantContract::get_grant(env.clone(), 7).unwrap();
            assert!(grant.milestone_met);

        });
    }

    #[test]
    fn duplicate_oracle_approval_is_rejected() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::GrantStreamContract);
        let (_admin, _recipient) = setup_grant(&env, &contract_id);
        with_contract(&env, &contract_id, |env| {

            let key1 = signing_key(11);
            let key2 = signing_key(12);
            let key3 = signing_key(13);
            GrantContract::configure_milestone_oracles(
                env.clone(),
                7,
                Vec::from_array(
                    &env,
                    [
                        bytesn32(&env, key1.verifying_key().to_bytes()),
                        bytesn32(&env, key2.verifying_key().to_bytes()),
                        bytesn32(&env, key3.verifying_key().to_bytes()),
                    ],
                ),
                2,
                300,
            )
            .unwrap();

            let evidence_hash = bytesn32(&env, [7; 32]);
            GrantContract::submit_milestone_evidence(env.clone(), 7, evidence_hash.clone()).unwrap();
            let state = GrantContract::get_milestone_consensus(env.clone(), 7).unwrap();
            let pk = bytesn32(&env, key1.verifying_key().to_bytes());
            let sig = sign_approval(&env, 7, &evidence_hash, state.dispute_window_end, &key1);

            GrantContract::approve_milestone(env.clone(), 7, pk.clone(), sig).unwrap();

            let duplicate_sig =
                sign_approval(&env, 7, &evidence_hash, state.dispute_window_end, &key1);
            let err =
                GrantContract::approve_milestone(env.clone(), 7, pk, duplicate_sig).unwrap_err();
            assert_eq!(err, Error::DuplicateOracleApproval);

        });
    }

    #[test]
    fn milestone_stays_locked_after_dispute_window() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, crate::GrantStreamContract);
        let (_admin, _recipient) = setup_grant(&env, &contract_id);
        with_contract(&env, &contract_id, |env| {

            let key1 = signing_key(21);
            let key2 = signing_key(22);
            let key3 = signing_key(23);
            GrantContract::configure_milestone_oracles(
                env.clone(),
                7,
                Vec::from_array(
                    &env,
                    [
                        bytesn32(&env, key1.verifying_key().to_bytes()),
                        bytesn32(&env, key2.verifying_key().to_bytes()),
                        bytesn32(&env, key3.verifying_key().to_bytes()),
                    ],
                ),
                2,
                10,
            )
            .unwrap();

            let evidence_hash = bytesn32(&env, [3; 32]);
            GrantContract::submit_milestone_evidence(env.clone(), 7, evidence_hash.clone()).unwrap();
            let state = GrantContract::get_milestone_consensus(env.clone(), 7).unwrap();
            let sig1 = sign_approval(&env, 7, &evidence_hash, state.dispute_window_end, &key1);

            GrantContract::approve_milestone(
                env.clone(),
                7,
                bytesn32(&env, key1.verifying_key().to_bytes()),
                sig1,
            )
            .unwrap();

            env.ledger().with_mut(|ledger| {
                ledger.timestamp = state.dispute_window_end + 1;
            });

            let sig2 = sign_approval(&env, 7, &evidence_hash, state.dispute_window_end, &key2);
            let err = GrantContract::approve_milestone(
                env.clone(),
                7,
                bytesn32(&env, key2.verifying_key().to_bytes()),
                sig2,
            )
            .unwrap_err();
            assert_eq!(err, Error::MilestoneWindowClosed);

            let final_state = GrantContract::get_milestone_consensus(env.clone(), 7).unwrap();
            assert_eq!(final_state.approvers.len(), 1);
            assert!(!final_state.is_completed);

        });
    }
}
