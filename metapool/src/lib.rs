//! A smart contract that allows diversified staking, providing the stNEAR LST NEP-141 Token
//! this contract include parts of core-contracts/lockup-contract & core-contracts/staking-pool

/********************************/
/* CONTRACT Self Identification */
/********************************/
// [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
// see also pub fn get_contract_info
const CONTRACT_NAME: &str = "Metapool";
const CONTRACT_VERSION: &str = "2.0.0";
const DEFAULT_WEB_APP_URL: &str = "https://metapool.app";
const DEFAULT_AUDITOR_ACCOUNT_ID: &str = "auditors.near";
const SOURCE_URL: &str = "github.com/Meta-Pool/liquid-staking-contract";

use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::collections::{LookupMap, UnorderedMap};
use near_sdk::{
    assert_one_yocto, env, ext_contract, log, near_bindgen, require, AccountId, PanicOnDefault,
    Promise,
};

pub mod gas;
pub mod types;
pub mod utils;
pub use crate::owner::*;
pub use crate::types::*;
pub use crate::utils::*;

pub mod account;
pub mod internal;
pub mod staking_pools;
pub use crate::account::*;
pub use crate::internal::*;
pub use crate::staking_pools::*;

pub mod distribute;
mod migrations;
pub mod owner;

pub mod reward_meter;
pub use reward_meter::*;

pub mod empty_nep_145;
pub mod events;
pub mod fungible_token_standard;

//self-callbacks
#[ext_contract(ext_self_owner)]
pub trait ExtMetaStakingPoolOwnerCallbacks {
    fn on_staking_pool_deposit(&mut self, amount: U128String) -> bool;

    fn on_retrieve_from_staking_pool(&mut self, inx: u16) -> bool;

    fn on_staking_pool_stake_maybe_deposit(
        &mut self,
        sp_inx: usize,
        amount: u128,
        included_deposit: bool,
    ) -> bool;

    fn on_staking_pool_unstake(
        &mut self,
        sp_inx: usize,
        amount_from_unstake_orders: U128String,
        amount_from_rebalance: U128String,
    ) -> bool;

    fn on_get_sp_total_balance(&mut self, sp_inx: usize, #[callback] total_balance: U128String);

    fn on_get_sp_unstaked_balance(
        &mut self,
        sp_inx: usize,
        #[callback] unstaked_balance: U128String,
    );

    fn after_minting_meta(self, account_id: AccountId, to_mint: U128String);
}

// #[ext_contract(meta_token_mint)]
// pub trait MetaToken {
//     fn mint(&mut self, account_id: AccountId, amount: U128String);
// }

//------------------------
//  Main Contract State --
//------------------------
// Note: Because this contract holds a large liquidity-pool, there are no `min_account_balance` required for accounts.
// Accounts are automatically removed (converted to default) where available & staked & shares & meta = 0. see: internal_update_account
#[near_bindgen]
#[derive(BorshDeserialize, BorshSerialize, PanicOnDefault)]
pub struct MetaPool {
    /// Owner's account ID (DAO)
    pub owner_account_id: AccountId,

    /// Avoid re-entry when async-calls are in-flight
    pub contract_busy: bool,

    /// no auto-staking. true while changing staking pools
    pub staking_paused: bool,

    /// What should be the contract_account_balance according to our internal accounting (if there's extra, it is 30% tx-fees)
    /// This amount increments with attachedNEAR calls (inflow) and decrements with deposit_and_stake calls (outflow)
    /// increments with retrieve_from_staking_pool (inflow) and decrements with user withdrawals from the contract (outflow)
    /// It should match env::balance()
    pub contract_account_balance: u128,

    /// Every time a user performs a delayed-unstake, stNEAR tokens are burned and the user gets a unstaked_claim that will
    /// be fulfilled 4 epochs from now. If there are someone else staking in the same epoch, both orders (stake & d-unstake) cancel each other
    /// (no need to go to the staking-pools) but the NEAR received for staking must be now reserved for the unstake-withdraw 4 epochs form now.
    /// This amount increments *during* end_of_epoch_clearing, *if* there are staking & unstaking orders that cancel each-other
    /// This amount also increments at retrieve_from_staking_pool, all retrieved NEAR after wait is considered at first reserved for unstake claims
    /// The funds here are *reserved* for the unstake-claims and can only be used to fulfill those claims
    /// This amount decrements at user's delayed-unstake-withdraw, when sending the NEAR to the user
    /// Related variables and Invariant:
    /// retrieved_for_unstake_claims = NEAR in the contract, retrieved in prev epochs (or result of clearing)
    /// unstaked_and_waiting = delay-unstaked in prev epochs, waiting, will become reserve
    /// epoch_unstake_orders = delay-unstaked in this epoch, may remain in the contract or start unstaking before EOE
    /// Invariant: retrieved_for_unstake_claims + unstaked_and_waiting + epoch_unstake_orders must be >= total_unstake_claims
    /// IF the sum is > (not ==), then it is implied that a rebalance is in progress, and the extra amount should be restaked
    /// NOTE: use always fn self.consider_retrieved_for_unstake_claims(amount) to increase this accumulator
    pub retrieved_for_unstake_claims: u128,

    /// This value is equivalent to sum(accounts.available)
    /// This amount increments with user's deposits_into_available and decrements when users stake_from_available
    /// increments with unstake_to_available and decrements with withdraw_from_available
    /// Note: in the current simplified UI user-flow of the meta-pool, only the NSLP & the treasury can have available balance
    /// the rest of the users move directly between their NEAR native accounts & the contract accounts, only briefly occupying acc.available
    pub total_available: u128,

    //-- ORDERS
    // this two amounts can cancel each other at end_of_epoch_clearing
    /// The total amount of "stake" orders in the current epoch, stNEAR has been minted, NEAR is in the contract, stake might be done before EOE
    /// at at end_of_epoch_clearing, (if there were a lot of unstake in the same epoch),
    /// it is possible that this amount remains in hte contract as reserve_for_unstake_claim
    pub epoch_stake_orders: u128,
    /// The total amount of "delayed-unstake" orders in the current epoch, stNEAR has been burned, unstake might be done before EOE
    /// at at end_of_epoch_clearing, (if there were also stake in the same epoch),
    /// it is possible that this amount remains in hte contract as reserve_for_unstake_claim
    pub epoch_unstake_orders: u128,
    /// Not used
    pub epoch_last_clearing: EpochHeight,

    /// The total amount of tokens selected for staking by the users
    /// not necessarily what's actually staked since staking can is done in batches
    /// Share price is computed using this number. share_price = total_for_staking/total_shares
    pub total_for_staking: u128,

    /// The total amount of tokens actually staked (the tokens are in the staking pools)
    // During distribute_staking(), If !staking_paused && total_for_staking<total_actually_staked, then the difference gets staked in the pools
    // During distribute_unstaking(), If total_actually_staked>total_for_staking, then the difference gets unstaked from the pools
    pub total_actually_staked: u128,

    /// how many "shares" were minted. Every time someone "stakes" he "buys pool shares" with the staked amount
    // the buy share price is computed so if she "sells" the shares on that moment she recovers the same near amount
    // staking produces rewards, rewards are added to total_for_staking so share_price will increase with rewards
    // share_price = total_for_staking/total_shares
    // when someone "unstakes" they "burns" X shares at current price to recoup Y near
    pub total_stake_shares: u128, //total stNEAR minted

    /// META (now mpDAO) is the governance token
    pub total_meta: u128, // deprecated

    /// The total amount of tokens actually unstaked and in the waiting-delay (the tokens are in the staking pools)
    /// equivalent to sum(sp.unstaked)
    pub total_unstaked_and_waiting: u128,

    /// Every time a user performs a delayed-unstake, stNEAR tokens are burned and the user gets a unstaked_claim
    /// equal to sum(accounts.unstake). Every time a user delayed-unstakes, this amount is incremented
    /// when the funds are withdrawn to the user account, the amount is decremented.
    /// Related variables and Invariant:
    /// retrieved_for_unstake_claims = NEAR in the contract, retrieved in prev epochs (or result of clearing)
    /// unstaked_and_waiting = delay-unstaked in prev epochs, waiting, will become reserve
    /// epoch_unstake_orders = delay-unstaked in this epoch, may remain in the contract or start unstaking before EOE
    /// Invariant: retrieved_for_unstake_claims + unstaked_and_waiting + epoch_unstake_orders must be >= total_unstake_claims
    /// IF the sum is > (not ==), then it is implied that a rebalance is in progress, and the extra amount should be restaked
    pub total_unstake_claims: u128,

    /// the staking pools will add rewards to the staked amount on each epoch
    /// here we store the accumulated amount only for stats purposes. This amount can only grow
    pub accumulated_staked_rewards: u128,

    //user's accounts
    pub accounts: UnorderedMap<AccountId, Account>,

    //list of pools to diversify in
    pub staking_pools: Vec<StakingPoolInfo>,

    // validator loan request
    // action on audit suggestions, this field is not used. No need for this to be on the main contract
    pub loan_requests: LookupMap<AccountId, VLoanRequest>,

    //The next 3 values define the Liq.Provider fee curve
    // NEAR/stNEAR Liquidity pool fee curve params
    // We assume this pool is always UNBALANCED, there should be more NEAR than stNEAR 99% of the time
    ///NEAR/stNEAR Liquidity target. If the Liquidity reach this amount, the fee reaches nslp_min_discount_basis_points
    pub nslp_liquidity_target: u128, // 150_000*NEAR initially
    ///NEAR/stNEAR Liquidity pool max fee
    pub nslp_max_discount_basis_points: u16, //5% initially
    ///NEAR/stNEAR Liquidity pool min fee
    pub nslp_min_discount_basis_points: u16, //0.5% initially

    // (deprecated) The next 3 values define meta rewards multipliers. (10 => 1x, 20 => 2x, ...)
    // for each stNEAR paid staking reward, reward stNEAR holders with META. default:5x. reward META = rewards * (mult_pct*10) / 100
    pub staker_meta_mult_pct: u16, // deprecated
    // for each stNEAR paid as discount, reward stNEAR sellers with META. default:1x. reward META = discounted * (mult_pct*10) / 100
    pub stnear_sell_meta_mult_pct: u16, // deprecated
    // for each stNEAR paid as discount, reward LP providers  with META. default:20x. reward META = fee * (mult_pct*10) / 100
    pub lp_provider_meta_mult_pct: u16, // deprecated

    /// min amount accepted as deposit or stake
    pub min_deposit_amount: u128,

    /// Operator account ID (who's in charge to call distribute_xx() on a periodic basis)
    pub operator_account_id: AccountId,
    /// operator_rewards_fee_basis_points. (0.2% default) 100 basis point => 1%. E.g.: owner_fee_basis_points=30 => 0.3% owner's fee
    pub operator_rewards_fee_basis_points: u16,
    /// owner's cut on Liquid Unstake fee (3% default)
    pub operator_swap_cut_basis_points: u16,
    /// Treasury account ID (it will be controlled by a DAO on phase II)
    pub treasury_account_id: AccountId,
    /// treasury cut on Liquid Unstake (25% from the fees by default)
    pub treasury_swap_cut_basis_points: u16,

    // Configurable info for [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
    pub web_app_url: Option<String>,
    pub auditor_account_id: Option<AccountId>,

    /// (deprecated) Where's the governance token contract
    pub meta_token_account_id: AccountId, // deprecated

    /// (deprecated) estimated & max meta rewards for each category
    pub est_meta_rewards_stakers: u128,
    pub est_meta_rewards_lu: u128, //liquid-unstakers
    pub est_meta_rewards_lp: u128, //liquidity-providers
    // max. when this amount is passed, corresponding multiplier is damped proportionally
    pub max_meta_rewards_stakers: u128,
    pub max_meta_rewards_lu: u128, //liquid-unstakers
    pub max_meta_rewards_lp: u128, //liquidity-providers

    /// up to 1% of the total pool can be unstaked for rebalance (no more than 1% to not affect APY)
    pub unstake_for_rebalance_cap_bp: u16, // default 100bp, meaning 1%
    /// when some unstake for rebalance is executed, this amount is increased
    /// when some extra is retrieved or recovered in EOE clearing, it is decremented
    /// represents the amount that's not staked because is in transit for rebalance.
    /// it could be in unstaked_and_waiting or in the contract & epoch_stake_orders
    pub unstaked_for_rebalance: u128,
}

#[near_bindgen]
impl MetaPool {
    /* NOTE
    This contract implements several traits

    1. core-contracts/staking-pool: this contract must be perceived as a staking-pool for the lockup-contract, wallets, and users.
        This means implementing: ping, deposit, deposit_and_stake, withdraw_all, withdraw, stake_all, stake, unstake_all, unstake
        and view methods: get_account_unstaked_balance, get_account_staked_balance, get_account_total_balance, is_account_unstaked_balance_available,
            get_total_staked_balance, get_owner_id, get_reward_fee_fraction, is_staking_paused, get_staking_key, get_account,
            get_number_of_accounts, get_accounts.
        Note: Some functions are not emulated like: get_staking_key, and other are condensed in fn deposit_and_stake

    2. meta-staking: these are the extensions to the standard staking pool (liquid stake/unstake)

    3. fungible token [NEP-141]: this contract is the NEP-141 contract for the stNEAR token

    */

    /// Initializes MetaPool contract.
    /// - `owner_account_id` - the account ID of the owner.  Only this account can call owner's methods on this contract.
    #[init]
    pub fn new(
        owner_account_id: AccountId,
        treasury_account_id: AccountId,
        operator_account_id: AccountId,
        meta_token_account_id: AccountId,
    ) -> Self {
        let result = Self {
            owner_account_id,
            contract_busy: false,
            operator_account_id,
            treasury_account_id,
            contract_account_balance: 0,
            web_app_url: Some(String::from(DEFAULT_WEB_APP_URL)),
            auditor_account_id: Some(AccountId::new_unchecked(DEFAULT_AUDITOR_ACCOUNT_ID.into())),
            operator_rewards_fee_basis_points: DEFAULT_OPERATOR_REWARDS_FEE_BASIS_POINTS,
            operator_swap_cut_basis_points: DEFAULT_OPERATOR_SWAP_CUT_BASIS_POINTS,
            treasury_swap_cut_basis_points: DEFAULT_TREASURY_SWAP_CUT_BASIS_POINTS,
            staking_paused: false,
            total_available: 0,
            total_for_staking: 0,
            total_actually_staked: 0,
            total_unstaked_and_waiting: 0,
            retrieved_for_unstake_claims: 0,
            total_unstake_claims: 0,
            epoch_stake_orders: 0,
            epoch_unstake_orders: 0,
            epoch_last_clearing: 0,
            accumulated_staked_rewards: 0,
            total_stake_shares: 0,
            total_meta: 0,
            accounts: UnorderedMap::new(b"A".to_vec()),
            loan_requests: LookupMap::new(b"L".to_vec()),
            nslp_liquidity_target: 10_000 * NEAR,
            nslp_max_discount_basis_points: 180, //1.8%
            nslp_min_discount_basis_points: 25,  //0.25%
            min_deposit_amount: ONE_NEAR,        // 1 NEAR
            // (deprecated) for each stNEAR paid as discount, reward stNEAR sellers with META. initial 5x, default:1x. reward META = discounted * mult_pct / 100
            stnear_sell_meta_mult_pct: 50, //5x (deprecated)
            // (deprecated) for each stNEAR paid staking reward, reward stNEAR holders with META. initial 10x, default:5x. reward META = rewards * mult_pct / 100
            staker_meta_mult_pct: 5000, //500x (deprecated)
            // for each stNEAR paid as discount, reward LPs with META. initial 50x, default:20x. reward META = fee * mult_pct / 100
            lp_provider_meta_mult_pct: 200, //20x (deprecated)
            staking_pools: Vec::new(),
            meta_token_account_id,
            est_meta_rewards_stakers: 0, // (deprecated)
            est_meta_rewards_lu: 0,      // (deprecated)
            est_meta_rewards_lp: 0,      // (deprecated)
            max_meta_rewards_stakers: 1_000_000 * ONE_NEAR, // (deprecated)
            max_meta_rewards_lu: 50_000 * ONE_NEAR, // (deprecated)
            max_meta_rewards_lp: 100_000 * ONE_NEAR, // (deprecated)
            unstaked_for_rebalance: 0,
            unstake_for_rebalance_cap_bp: 100,
        };
        //all key accounts must be different
        result.require_key_accounts_are_different();
        return result;
    }

    fn require_key_accounts_are_different(&self) {
        //all accounts must be different
        require!(self.owner_account_id != self.operator_account_id);
        require!(self.owner_account_id.to_string() != DEVELOPERS_ACCOUNT_ID);
        require!(self.owner_account_id != self.treasury_account_id);
        require!(self.operator_account_id.to_string() != DEVELOPERS_ACCOUNT_ID);
        require!(self.operator_account_id != self.treasury_account_id);
        require!(self.treasury_account_id.to_string() != DEVELOPERS_ACCOUNT_ID);
    }

    //------------------------------------
    // core-contracts/staking-pool trait
    //------------------------------------

    /// staking-pool's ping is moot here
    pub fn ping(&mut self) {}

    /// Deposits the attached amount into the inner account of the predecessor.
    // Note: pub fn deposit(&mut self)  is not implemented, use fn deposit_and_stake

    /// Withdraws a specific amount from "UNSTAKED" balance *TO MIMIC core-contracts/staking-pool*
    pub fn withdraw(&mut self, amount: U128String) -> Promise {
        require_not_lockup_account_calling();
        let account_id = env::predecessor_account_id();
        self.internal_withdraw_use_unstaked(&account_id, amount.0)
    }
    /// Withdraws ALL from from "UNSTAKED" balance *TO MIMIC core-contracts/staking-pool
    pub fn withdraw_all(&mut self) -> Promise {
        require_not_lockup_account_calling();
        let account_id = env::predecessor_account_id();
        let account = self.internal_get_account(&account_id);
        self.internal_withdraw_use_unstaked(&account_id, account.unstaked)
    }

    /// simplified flow, equivalent to withdraw_all, used by metapool.app frontend
    pub fn withdraw_unstaked(&mut self) -> Promise {
        self.withdraw_all()
    }

    /// Deposits the attached amount into the inner account of the predecessor and stakes it.
    #[payable]
    pub fn deposit_and_stake(&mut self) -> U128String {
        require_not_lockup_account_calling();
        let account_id = env::predecessor_account_id();
        let amount = self.internal_deposit(&account_id);
        let (amount, shares) = self.internal_stake_from_account(&account_id, amount);
        //----------
        // check if the liquidity pool needs liquidity, and then use this opportunity to liquidate stnear in the LP by internal-clearing
        // the amount just deposited, might be swapped in the liquid-unstake pool
        self.nslp_try_internal_clearing(amount);
        events::FtMint {
            owner_id: &account_id,
            amount: shares.into(),
            memo: None,
        }
        .emit();

        shares.into()
    }

    /// Stakes all "unstaked" balance from the inner account of the predecessor.
    /// we keep this to implement the staking-pool trait, but we don't support re-staking unstaked amounts
    // Note: pub fn stake_all(&mut self) is not implemented, use fn deposit_and_stake

    /// Stakes the given amount from the inner account of the predecessor.
    /// we keep this to implementing the staking-pool trait, but we don't support re-staking unstaked amounts
    // Note: pub fn stake(&mut self, amount: U128String) is not implemented, use fn deposit_and_stake

    /// Unstakes all staked balance from the inner account of the predecessor.
    /// The new total unstaked balance will be available for withdrawal in four epochs.
    pub fn unstake_all(&mut self) {
        require_not_lockup_account_calling();
        let account_id = env::predecessor_account_id();
        let mut account = self.internal_get_account(&account_id);
        let all_shares = account.stake_shares;
        self.internal_unstake_shares(&account_id, &mut account, all_shares);
    }

    /// Unstakes the given amount (in NEAR) from the inner account of the predecessor.
    /// The inner account should have enough staked balance.
    /// The new total unstaked balance will be available for withdrawal in four epochs.
    /// delayed_unstake, amount_requested is in yoctoNEARs
    pub fn unstake(&mut self, amount: U128String) {
        require_not_lockup_account_calling();
        let account_id = env::predecessor_account_id();
        self.internal_unstake(&account_id, amount.0);
    }

    /*******************/
    /* lockup accounts */
    /*******************/
    #[payable]
    pub fn stake_for_lockup(&mut self, lockup_account_id: &AccountId) -> U128String {
        require_lockup_contract_calling();
        let amount = self.internal_deposit(lockup_account_id);
        let (amount, shares) = self.internal_stake_from_account(lockup_account_id, amount);
        //----------
        // check if the liquidity pool needs liquidity, and then use this opportunity to liquidate stnear in the LP by internal-clearing
        // the amount just deposited, might be swapped in the liquid-unstake pool
        self.nslp_try_internal_clearing(amount);

        events::FtMint {
            owner_id: &lockup_account_id,
            amount: shares.into(),
            memo: None,
        }
        .emit();
        // return the shares minted
        shares.into()
    }
    /// Unstakes the exact amount of shares from a lockup account
    /// The new total unstaked balance will be available for withdrawal in x epochs.
    /// delayed_unstake, amount_requested is in stNEAR/shares
    /// return value is the unstaked nears and the epoch when the NEARS will be available for withdraw
    pub fn unstake_from_lockup_shares(
        &mut self,
        lockup_account_id: &AccountId,
        shares: U128String,
    ) -> (U128String, U64String) {
        require_lockup_contract_calling();
        let mut acc = self.internal_get_account(lockup_account_id);
        let (nears, epoch) = self.internal_unstake_shares(&lockup_account_id, &mut acc, shares.0);
        (nears.into(), epoch.into())
    }

    pub fn withdraw_to_lockup(
        &mut self,
        lockup_account_id: &AccountId,
        amount: U128String,
    ) -> Promise {
        require_lockup_contract_calling();
        self.internal_withdraw_use_unstaked(lockup_account_id, amount.0)
    }

    /*****************************/
    /* staking-pool View methods */
    /*****************************/

    /// Returns the unstaked balance of the given account.
    pub fn get_account_unstaked_balance(&self, account_id: AccountId) -> U128String {
        // note: get_account returns HumanReadableAccount - ok for unregistered accounts
        return self.get_account(account_id).unstaked_balance;
    }

    /// Returns the staked balance of the given account.
    /// NOTE: This is computed from the amount of "stake" shares the given account has and the
    /// current amount of total staked balance and total stake shares on the account.
    pub fn get_account_staked_balance(&self, account_id: AccountId) -> U128String {
        // note: get_account returns HumanReadableAccount - ok for unregistered accounts
        return self.get_account(account_id).staked_balance;
    }

    /// Returns the total balance of the given account (including staked and unstaked balances).
    pub fn get_account_total_balance(&self, account_id: AccountId) -> U128String {
        let acc = self.accounts.get(&account_id).unwrap_or_default();
        return (acc.available + self.amount_from_stake_shares(acc.stake_shares) + acc.unstaked)
            .into();
    }

    /// additional to staking-pool to satisfy generic deposit-NEP-standard
    /// returns the amount that can be withdrawn immediately
    pub fn get_account_available_balance(&self, account_id: AccountId) -> U128String {
        let acc = self.accounts.get(&account_id).unwrap_or_default();
        return acc.available.into();
    }

    /// Returns `true` if the given account can withdraw tokens in the current epoch.
    pub fn is_account_unstaked_balance_available(&self, account_id: AccountId) -> bool {
        // note: get_account returns HumanReadableAccount - ok for unregistered accounts
        return self.get_account(account_id).can_withdraw;
    }

    /// Returns account ID of the staking pool owner.
    pub fn get_owner_id(&self) -> AccountId {
        return self.owner_account_id.clone();
    }

    /// Returns the current reward fee as a fraction.
    pub fn get_reward_fee_fraction(&self) -> RewardFeeFraction {
        return RewardFeeFraction {
            numerator: (self.operator_rewards_fee_basis_points
                + DEVELOPERS_REWARDS_FEE_BASIS_POINTS)
                .into(),
            denominator: BP_100_PERCENT as u32,
        };
    }
    pub fn get_reward_fee_bp(&self) -> u16 {
        self.operator_rewards_fee_basis_points + DEVELOPERS_REWARDS_FEE_BASIS_POINTS
    }

    #[payable]
    pub fn set_reward_fee(&mut self, basis_points: u16) {
        self.require_owner_calling();
        assert_one_yocto();
        require!(basis_points <= 1000); // less than or equal 10%
        self.operator_rewards_fee_basis_points =
            basis_points.saturating_sub(DEVELOPERS_REWARDS_FEE_BASIS_POINTS);
    }

    /// Returns true if the staking is paused
    pub fn is_staking_paused(&self) -> bool {
        return self.staking_paused;
    }

    /// to implement the Staking-pool interface, get_account returns the same as the staking-pool returns
    /// full account info can be obtained by calling: pub fn get_account_info(&self, account_id: AccountId) -> GetAccountInfoResult
    /// Returns human readable representation of the account for the given account ID.
    // note: get_account returns HumanReadableAccount - ok for unregistered accounts
    pub fn get_account(&self, account_id: AccountId) -> HumanReadableAccount {
        let account = self.accounts.get(&account_id).unwrap_or_default();
        return HumanReadableAccount {
            account_id,
            unstaked_balance: account.unstaked.into(),
            staked_balance: self.amount_from_stake_shares(account.stake_shares).into(),
            can_withdraw: env::epoch_height() >= account.unstaked_requested_unlock_epoch,
        };
    }

    /// Returns the number of accounts that have positive balance on this staking pool.
    pub fn get_number_of_accounts(&self) -> u64 {
        return self.accounts.len();
    }

    /// Returns the list of accounts (staking-pool trait)
    // note: get_account returns HumanReadableAccount - ok for unregistered accounts
    pub fn get_accounts(&self, from_index: u64, limit: u64) -> Vec<HumanReadableAccount> {
        let keys = self.accounts.keys_as_vector();
        return (from_index..std::cmp::min(from_index + limit, keys.len()))
            .map(|index| self.get_account(keys.get(index).unwrap()))
            .collect();
    }

    //----------------------------------
    //----------------------------------
    // META-STAKING-POOL trait
    //----------------------------------
    //----------------------------------

    /// Returns the list of accounts with full data (div-pool trait)
    pub fn get_accounts_info(&self, from_index: u64, limit: u64) -> Vec<GetAccountInfoResult> {
        let keys = self.accounts.keys_as_vector();
        return (from_index..std::cmp::min(from_index + limit, keys.len()))
            .map(|index| self.get_account_info(keys.get(index).unwrap()))
            .collect();
    }

    //---------------------------
    // NSLP Methods
    //---------------------------

    /// user method - NEAR/stNEAR SWAP functions
    /// return how much NEAR you can get by selling x stNEAR
    pub fn get_near_amount_sell_stnear(&self, stnear_to_sell: U128String) -> U128String {
        let lp_account = self.internal_get_nslp_account();
        return self
            .internal_get_near_amount_sell_stnear(lp_account.available, stnear_to_sell.0)
            .into();
    }

    /// NEAR/stNEAR Liquidity Pool
    /// computes the discount_basis_points for NEAR/stNEAR Swap based on NSLP Balance
    /// If you want to sell x stNEAR
    pub fn nslp_get_discount_basis_points(&self, stnear_to_sell: U128String) -> u16 {
        let lp_account = self.internal_get_nslp_account();
        return self.internal_get_discount_basis_points(lp_account.available, stnear_to_sell.0);
    }

    /// user method
    /// swaps stNEAR->NEAR in the Liquidity Pool
    /// returns nears transferred
    //#[payable]
    pub fn liquid_unstake(
        &mut self,
        st_near_to_burn: U128String,
        min_expected_near: U128String,
    ) -> LiquidUnstakeResult {
        self.require_not_busy();
        self.require_not_paused();
        // Q: Why not? - R: liquid_unstake It's not as problematic as transfer, because it moves tokens between accounts of the same user
        // so let's remove the one_yocto_requirement, waiting for a better solution for the function-call keys NEP-141 problem
        //assert_one_yocto();

        let account_id = env::predecessor_account_id();
        let mut user_account = self.internal_get_account(&account_id);

        let stnear_owned = user_account.stake_shares;

        let st_near_to_sell:u128 =
        // if the amount is close to user's total, remove user's total
        // to: a) do not leave less than ONE_MILLI_NEAR in the account, b) Allow 10 yoctos of rounding, e.g. remove(100) removes 99.999993 without panicking
        if is_close(st_near_to_burn.0, stnear_owned) { // allow for rounding simplification
            stnear_owned
        }
        else  {
            st_near_to_burn.0
        };

        log!(
            "st_near owned:{}, to_sell:{}",
            user_account.stake_shares,
            st_near_to_sell
        );

        require!(
            stnear_owned >= st_near_to_sell,
            format!("Not enough stNEAR. You own {}", stnear_owned)
        );

        let mut nslp_account = self.internal_get_nslp_account();

        // compute how many nears are the st_near valued at
        let nears_out = self.amount_from_stake_shares(st_near_to_sell);
        let swap_fee_basis_points =
            self.internal_get_discount_basis_points(nslp_account.available, nears_out);
        let fee = apply_pct(swap_fee_basis_points, nears_out);

        let near_to_receive = nears_out - fee;
        require!(
            near_to_receive >= min_expected_near.0,
            format!(
                "Price changed, your min amount {} is not satisfied {}. Try again",
                min_expected_near.0, near_to_receive
            )
        );
        require!(
            nslp_account.available >= near_to_receive,
            "Not enough liquidity in the liquidity pool"
        );

        //the NEAR for the user comes from the LP
        nslp_account.available -= near_to_receive;
        user_account.available += near_to_receive;

        // compute how many shares the swap fee represent
        let fee_in_st_near = self.stake_shares_from_amount(fee);

        // involved accounts
        let treasury_account_id = self.treasury_account_id.clone();
        require!(
            &account_id != &treasury_account_id,
            "can't use treasury account"
        );
        let mut treasury_account = self.accounts.get(&treasury_account_id).unwrap_or_default();

        let operator_account_id = self.operator_account_id.clone();
        require!(
            &account_id != &operator_account_id,
            "can't use operator account"
        );
        let mut operator_account = self.accounts.get(&operator_account_id).unwrap_or_default();

        let developers_account_id = AccountId::new_unchecked(DEVELOPERS_ACCOUNT_ID.into());
        require!(
            &account_id != &developers_account_id,
            "can't use developers account"
        );
        let mut developers_account = self
            .accounts
            .get(&developers_account_id)
            .unwrap_or_default();

        // The treasury cut in stnear-shares (25% by default)
        let treasury_st_near_cut = apply_pct(self.treasury_swap_cut_basis_points, fee_in_st_near);
        treasury_account.add_st_near(treasury_st_near_cut, &self);

        // The cut that the contract owner (operator) takes. (3% of 1% normally)
        let operator_st_near_cut = apply_pct(self.operator_swap_cut_basis_points, fee_in_st_near);
        operator_account.add_st_near(operator_st_near_cut, &self);

        // The cut that the developers take. (2% of 1% normally)
        let developers_st_near_cut = apply_pct(DEVELOPERS_SWAP_CUT_BASIS_POINTS, fee_in_st_near);
        developers_account.add_st_near(developers_st_near_cut, &self);

        log!("treasury_st_near_cut:{} operator_st_near_cut:{} developers_st_near_cut:{} fee_in_st_near:{}",
            treasury_st_near_cut,operator_st_near_cut,developers_st_near_cut,fee_in_st_near);

        assert!(
            fee_in_st_near > treasury_st_near_cut + developers_st_near_cut + operator_st_near_cut
        );

        // The rest of the st_near sold goes into the liq-pool. Because it is a larger amount than NEARs removed, it will increase share value for all LP providers.
        // Adding value to the pool via adding more stNEAR value than the NEAR removed
        let st_near_to_liq_pool = st_near_to_sell
            - (treasury_st_near_cut + operator_st_near_cut + developers_st_near_cut);
        log!("nslp_account.add_st_near {}", st_near_to_liq_pool);
        // major part of stNEAR sold goes to the NSLP
        nslp_account.add_st_near(st_near_to_liq_pool, &self);

        //complete the transfer, remove stnear from the user (stnear was transferred to the LP & others)
        user_account.sub_st_near(st_near_to_sell, &self);

        // Save involved accounts
        self.internal_update_account(&treasury_account_id, &treasury_account);
        self.internal_update_account(&operator_account_id, &operator_account);
        self.internal_update_account(&developers_account_id, &developers_account);

        //Save nslp accounts
        self.internal_save_nslp_account(&nslp_account);

        //simplified user-flow
        //direct transfer to user (instead of leaving it in-contract as "available")
        let transfer_amount = user_account.take_from_available(&account_id, near_to_receive, self);
        self.native_transfer(&account_id, transfer_amount);

        //Save user account
        self.internal_update_account(&account_id, &user_account);

        log!(
            "@{} liquid-unstaked {} stNEAR, got {} NEAR",
            &account_id,
            st_near_to_sell,
            transfer_amount
        );
        event!(
            r#"{{"event":"LIQ.U","account_id":"{}","stnear":"{}","near":"{}"}}"#,
            &account_id,
            st_near_to_sell,
            transfer_amount
        );

        return LiquidUnstakeResult {
            near: transfer_amount.into(),
            fee: fee_in_st_near.into(),
            meta: 0.into(), // meta_to_seller.into(),
        };
    }

    /// add liquidity - payable
    #[payable]
    pub fn nslp_add_liquidity(&mut self) -> u16 {
        // TODO: Since this method doesn't guard the resulting liquidity, is it possible to put it
        //    into a front-run/end-run sandwich to capitalize on the transaction?
        let account_id = env::predecessor_account_id();
        let amount = self.internal_deposit(&account_id);
        return self.internal_nslp_add_liquidity(&account_id, amount);
    }

    /// remove liquidity from liquidity pool
    /// "amount" is the amount in NEAR to remove from the pool
    /// internally the function calculates the proportion of NEAR and stNEAR to remove
    /// and the amount of shares to burn
    //#[payable]
    pub fn nslp_remove_liquidity(&mut self, amount: U128String) -> RemoveLiquidityResult {
        self.require_not_busy();
        self.require_not_paused();
        //assert_one_yocto();

        let account_id = env::predecessor_account_id();
        let mut acc = self.internal_get_account(&account_id);
        let mut nslp_account = self.internal_get_nslp_account();

        //how much does this user owns
        let valued_actual_shares = acc.valued_nslp_shares(self, &nslp_account);

        let mut to_remove = amount.0;
        let nslp_shares_to_burn: u128;
        // if the amount is close to user's total, remove user's total
        // to: a) do not leave less than ONE_MILLI_NEAR in the account, b) Allow 10 yoctos of rounding, e.g. remove(100) removes 99.999993 without panicking
        if is_close(to_remove, valued_actual_shares) {
            // allow for rounding simplification
            to_remove = valued_actual_shares;
            nslp_shares_to_burn = acc.nslp_shares; // close enough to all shares, burn-it all (avoid leaving "dust")
        } else {
            require!(
                valued_actual_shares >= to_remove,
                format!(
                    "Not enough share value {} to remove the requested amount from the pool",
                    valued_actual_shares
                )
            );
            // Calculate the number of "nslp" shares that the account will burn based on the amount requested
            nslp_shares_to_burn = self.nslp_shares_from_amount(to_remove, &nslp_account);
        }

        assert!(nslp_shares_to_burn > 0);

        //register removed liquidity to compute rewards correctly
        acc.lp_meter.unstake(to_remove);

        //compute proportionals stNEAR/NEAR
        //1st: stNEAR how much stNEAR from the Liq-Pool represents the ratio: nslp_shares_to_burn relative to total nslp_shares
        let st_near_to_remove_from_pool = proportional(
            nslp_account.stake_shares,
            nslp_shares_to_burn,
            nslp_account.nslp_shares,
        );
        //2nd: NEAR, by difference
        let near_value_of_st_near = self.amount_from_stake_shares(st_near_to_remove_from_pool);
        assert!(
            to_remove >= near_value_of_st_near,
            "inconsistency NTR<STR+UTR"
        );
        let near_to_remove = to_remove - near_value_of_st_near;

        //update user account
        //remove first from stNEAR in the pool, proportional to shares being burned
        //NOTE: To simplify user-operations, the LIQ.POOL DO NOT carry "unstaked". The NSLP self-balances only by internal-clearing on `deposit_and_stake`
        acc.available += near_to_remove;
        acc.add_st_near(st_near_to_remove_from_pool, &self); //add stnear to user acc
        acc.nslp_shares -= nslp_shares_to_burn; //shares this user burns
                                                //update NSLP account
        nslp_account.available -= near_to_remove;
        nslp_account.sub_st_near(st_near_to_remove_from_pool, &self); //remove stnear from the pool
        nslp_account.nslp_shares -= nslp_shares_to_burn; //burn from total nslp shares

        //simplify user-flow
        //direct transfer to user (instead of leaving it in-contract as "available")
        let transfer_amount = acc.take_from_available(&account_id, near_to_remove, self);
        self.native_transfer(&account_id, transfer_amount);

        //--SAVE ACCOUNTS
        self.internal_update_account(&account_id, &acc);
        self.internal_save_nslp_account(&nslp_account);

        event!(
            r#"{{"event":"REM.L","account_id":"{}","near":"{}","stnear":"{}"}}"#,
            account_id,
            transfer_amount,
            st_near_to_remove_from_pool
        );

        return RemoveLiquidityResult {
            near: transfer_amount.into(),
            st_near: st_near_to_remove_from_pool.into(),
        };
    }

    //----------------------------------
    // Use part of the NSLP to stake. This is the inverse operation of nslp_try_internal_clearing
    // can be used by the operator to increase epoch_stake_orders
    // to later direct stake in validators that are about to lose the seat
    // ---------------------------------
    #[payable]
    pub fn stake_from_nslp(&mut self, near_amount: U128String) {
        assert_one_yocto();
        self.require_operator_or_owner();
        // check the amount
        let nslp_account = self.internal_get_nslp_account();
        let amount = near_amount.0;
        require!(nslp_account.available > amount, "too much");
        require!(
            nslp_account.available - amount > self.nslp_liquidity_target,
            "stake will leave NSLP below target"
        );
        // stake from nslp
        self.internal_stake_from_account(&Self::nslp_internal_account_id(), amount);
    }

    //---------------------------------------------------------------------------
    /// Sputnik DAO remote-upgrade receiver
    /// can be called by a remote-upgrade proposal
    ///
    #[cfg(target_arch = "wasm32")]
    pub fn upgrade(self) -> Promise {
        require!(env::predecessor_account_id() == self.owner_account_id);
        require!(
            env::prepaid_gas() > near_sdk::Gas(150 * TGAS),
            "set 200TGAS or more for this transaction"
        );

        // Calculate gas for migration - leave some buffer for the rest of this function
        const GAS_FOR_THE_REST_OF_THIS_FUNCTION: near_sdk::Gas = near_sdk::Gas(10 * TGAS);
        let gas_for_migration =
            env::prepaid_gas() - env::used_gas() - GAS_FOR_THE_REST_OF_THIS_FUNCTION;

        // Load the new contract code from input register 0
        let code = env::input().expect("Contract upgrade requires new code as input");

        // Deploy the new contract code and call migrate() on the new contract
        Promise::new(env::current_account_id())
            .deploy_contract(code)
            .function_call("migrate".to_string(), vec![], 0, gas_for_migration)
    }
}
