#![cfg(test)]

//! Execution cost benchmarks for the governance contract (Issue #522).
//! Emits machine-readable `BENCHMARK` lines for CI regression checks,
//! mirroring the pattern established in `invoice_liquidity/tests_benchmarks.rs`.

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    token::StellarAssetClient,
    Address, BytesN, Env,
};

#[contract]
struct MockIlnBench;

#[contractimpl]
impl MockIlnBench {
    pub fn update_fee_rate(_env: Env, _rate: u32) {}
    pub fn add_token(_env: Env, _token: Address, _decimals: u32) {}
    pub fn remove_token(_env: Env, _token: Address) {}
    pub fn update_max_discount(_env: Env, _rate: u32) {}
}

struct BaseBenchEnv {
    env: Env,
    contract: GovContractClient<'static>,
    proposer: Address,
    voter: Address,
}

fn setup_benchmark_env() -> BaseBenchEnv {
    let env = Env::default();
    env.mock_all_auths();
    env.budget().reset_unlimited();

    let mut ledger = env.ledger().get();
    ledger.timestamp = 1_700_000_000;
    env.ledger().set(ledger);

    let token_admin = Address::generate(&env);
    let token_id = env.register_stellar_asset_contract_v2(token_admin);
    let token_addr = token_id.address();
    let token_admin_client = StellarAssetClient::new(&env, &token_addr);

    let proposer = Address::generate(&env);
    let voter = Address::generate(&env);
    token_admin_client.mint(&proposer, &1_000_000);
    token_admin_client.mint(&voter, &1_000_000);

    let iln_contract = env.register_contract(None, MockIlnBench);
    let dist_contract = env.register_contract(None, MockIlnBench);
    let rep_contract = env.register_contract(None, MockIlnBench);
    let admin = Address::generate(&env);

    let contract_id = env.register_contract(None, GovContract);
    let contract = GovContractClient::new(&env, &contract_id);
    contract.initialize(
        &iln_contract,
        &dist_contract,
        &rep_contract,
        &token_addr,
        &admin,
        &10_000,
    );

    // Issue #805: checkpoint + age both actors so benchmarked votes and
    // delegations exercise the eligible path.
    contract.checkpoint_balance(&proposer);
    contract.checkpoint_balance(&voter);
    let mut ledger = env.ledger().get();
    ledger.sequence_number += MIN_VOTE_HOLD_LEDGERS + 1;
    env.ledger().set(ledger);

    BaseBenchEnv {
        env,
        contract,
        proposer,
        voter,
    }
}

fn emit_benchmark(name: &str, cpu: u64, mem: u64) {
    std::println!("BENCHMARK {name} cpu={cpu} mem={mem}");
}

fn measure<F: FnOnce()>(env: &Env, name: &str, action: F) -> (u64, u64) {
    env.budget().reset_unlimited();
    action();
    let cpu = env.budget().cpu_instruction_cost();
    let mem = env.budget().memory_bytes_cost();
    emit_benchmark(name, cpu, mem);
    (cpu, mem)
}

#[test]
fn benchmark_create_proposal() {
    let bench = setup_benchmark_env();
    let hash = BytesN::from_array(&bench.env, &[7u8; 32]);

    measure(&bench.env, "create_proposal", || {
        bench.contract.create_proposal(
            &bench.proposer,
            &ProposalAction::UpdateFeeRate(500),
            &hash,
            &500,
        );
    });
}

#[test]
fn benchmark_cast_vote() {
    let bench = setup_benchmark_env();
    let hash = BytesN::from_array(&bench.env, &[7u8; 32]);
    let id = bench.contract.create_proposal(
        &bench.proposer,
        &ProposalAction::UpdateFeeRate(500),
        &hash,
        &500,
    );

    measure(&bench.env, "cast_vote", || {
        bench.contract.cast_vote(&bench.voter, &id, &true);
    });
}

#[test]
fn benchmark_delegate_votes() {
    let bench = setup_benchmark_env();

    measure(&bench.env, "delegate_votes", || {
        bench.contract.delegate_votes(&bench.voter, &bench.proposer);
    });
}

#[test]
fn benchmark_all_functions_summary() {
    // Uses "_summary"-suffixed BENCHMARK names so these lines never collide
    // with the isolated per-function benchmarks above when CI's regression
    // script parses combined test output (tests run concurrently, so line
    // order between this test and the isolated ones is not guaranteed).
    let mut results = std::vec::Vec::new();

    let bench = setup_benchmark_env();
    let hash = BytesN::from_array(&bench.env, &[7u8; 32]);

    results.push(measure(&bench.env, "create_proposal_summary", || {
        bench.contract.create_proposal(
            &bench.proposer,
            &ProposalAction::UpdateFeeRate(500),
            &hash,
            &500,
        );
    }));

    let id = bench.contract.create_proposal(
        &bench.proposer,
        &ProposalAction::UpdateFeeRate(500),
        &hash,
        &500,
    );
    results.push(measure(&bench.env, "cast_vote_summary", || {
        bench.contract.cast_vote(&bench.voter, &id, &true);
    }));
    results.push(measure(&bench.env, "delegate_votes_summary", || {
        bench.contract.delegate_votes(&bench.voter, &bench.proposer);
    }));

    std::println!("\n| Function        | CPU Instructions | Memory (bytes) |");
    std::println!("| --------------- | ----------------- | -------------- |");
    for (name, (cpu, mem)) in [
        ("create_proposal", results[0]),
        ("cast_vote", results[1]),
        ("delegate_votes", results[2]),
    ] {
        std::println!("| {name:<15} | {cpu:>17} | {mem:>14} |");
    }
}
