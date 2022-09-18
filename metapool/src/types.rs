use near_sdk::json_types::{U128, U64};
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::AccountId;
use uint::construct_uint;

//----------------------------------------
// CONSTANTS, types and interface structs
//----------------------------------------

// this contract token symbol
pub const STNEAR: &str = "stNEAR";

// internal pseudo-account (must be an invalid near-account-id)
pub const NSLP_INTERNAL_ACCOUNT: &str = "..NSLP..";

/// useful constants
pub const NO_DEPOSIT: u128 = 0;
pub const ONE_E24: u128 = 1_000_000_000_000_000_000_000_000;
pub const NEAR: u128 = ONE_E24;
pub const ONE_NEAR: u128 = NEAR;
pub const NEAR_CENT: u128 = NEAR / 100;
pub const ONE_MILLI_NEAR: u128 = NEAR / 1_000;
pub const ONE_MICRO_NEAR: u128 = NEAR / 1_000_000;
pub const TWO_NEAR: u128 = 2 * NEAR;
pub const FIVE_NEAR: u128 = 5 * NEAR;
pub const TEN_NEAR: u128 = 10 * NEAR;
pub const K_NEAR: u128 = 1_000 * NEAR;

///if there's less than MIN_MOVEMENT NEAR to stake/unstake, wait until there's more to justify the call & tx-fees
pub const MIN_STAKE_AMOUNT: u128 = ONE_NEAR;

pub const TGAS: u64 = 1_000_000_000_000;

/// The number of epochs required for the locked balance to become unlocked.
/// NOTE: The actual number of epochs when the funds are unlocked is 3. But there is a corner case
/// when the unstaking promise can arrive at the next epoch, while the inner state is already
/// updated in the previous epoch. It will not unlock the funds for 4 epochs.
/// If all staking-pools are unstaking, the user might have to wait 2*NUM_EPOCHS_TO_UNLOCK
pub const NUM_EPOCHS_TO_UNLOCK: EpochHeight = 4; //0 for testing in guild-net, 4 for mainnet & testnet;

/// The contract keeps at least 35 NEAR in the account to avoid being transferred out to cover
/// contract code storage and some internal state.
pub const MIN_BALANCE_FOR_STORAGE: u128 = 35_000_000_000_000_000_000_000_000;
/// if the remainder falls below this amount, it's included in the current movement
pub const MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT: u128 = 5 * K_NEAR;

//cut on swap fees
pub const DEFAULT_TREASURY_SWAP_CUT_BASIS_POINTS: u16 = 2500; // 25% swap fees go to Treasury
pub const DEFAULT_OPERATOR_SWAP_CUT_BASIS_POINTS: u16 = 300; // 3% swap fees go to operator
                                                             //Fee on staking rewards
pub const DEFAULT_OPERATOR_REWARDS_FEE_BASIS_POINTS: u16 = 50; // 0.5% -- CANT BE HIGHER THAN 1000 / 10%

//Note: License forbids you to change the following 3 constants and/or the developer's distribution mechanism
pub const DEVELOPERS_ACCOUNT_ID: &str = "developers.near";
pub const DEVELOPERS_REWARDS_FEE_BASIS_POINTS: u16 = 20; // 0.2% from rewards
pub const DEVELOPERS_SWAP_CUT_BASIS_POINTS: u16 = 200; // 2% swap fees go to authors

construct_uint! {
    /// 256-bit unsigned integer.
    pub struct U256(4);
}

/// Raw type for duration in nanoseconds
pub type Duration = u64;
/// Raw type for timestamp in nanoseconds or Unix Ts in milliseconds
pub type Timestamp = u64;

/// Balance wrapped into a struct for JSON serialization as a string.
pub type U128String = U128;
pub type U64String = U64;

pub type EpochHeight = u64;

/// NEP-129 get information about this contract
/// returns JSON string according to [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
/// Rewards fee fraction structure for the staking pool contract.
#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
#[allow(non_snake_case)]
pub struct NEP129Response {
    pub dataVersion: u16,
    pub name: String,
    pub version: String,
    pub source: String,
    pub standards: Vec<String>,
    pub webAppUrl: Option<String>,
    pub developersAccountId: String,
    pub auditorAccountId: Option<String>,
}

/// Rewards fee fraction structure for the staking pool contract.
#[derive(Serialize, Deserialize, Clone)]
#[serde(crate = "near_sdk::serde")]
pub struct RewardFeeFraction {
    pub numerator: u32,
    pub denominator: u32,
}

/// staking-pool trait
/// Represents an account structure readable by humans.
#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct HumanReadableAccount {
    pub account_id: AccountId,
    /// The unstaked balance that can be withdrawn or staked.
    pub unstaked_balance: U128,
    /// The amount balance staked at the current "stake" share price.
    pub staked_balance: U128,
    /// Whether the unstaked balance is available for withdrawal now.
    pub can_withdraw: bool,
}

/// Struct returned from get_account_info
/// div-pool full info
/// Represents account data as as JSON compatible struct
#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct GetAccountInfoResult {
    pub account_id: AccountId,

    /// The available balance that can be withdrawn
    pub available: U128,

    /// The amount of stNEAR owned (shares owned)
    pub st_near: U128,
    ///stNEAR owned valued in NEAR
    pub valued_st_near: U128, // st_near * stNEAR_price

    //META owned (including pending rewards)
    pub meta: U128,
    //realized META (without pending rewards)
    pub realized_meta: U128,

    /// The amount unstaked waiting for withdraw
    pub unstaked: U128,

    /// The epoch height when the unstaked will be available
    pub unstaked_requested_unlock_epoch: U64,
    /// How many epochs we still have to wait until unstaked_requested_unlock_epoch (epoch_unlock - env::epoch_height )
    pub unstake_full_epochs_wait_left: u16,
    ///if env::epoch_height()>=unstaked_requested_unlock_epoch
    pub can_withdraw: bool,
    /// total amount the user holds in this contract: account.available + account.staked + current_rewards + account.unstaked
    pub total: U128,

    //-- STATISTICAL DATA --
    // User's statistical data
    // These fields works as a car's "trip meter". The user can reset them to zero.
    /// trip_start: (unix timestamp) this field is set at account creation, so it will start metering rewards
    pub trip_start: U64,
    /// How many stnear the user had at "trip_start".
    pub trip_start_stnear: U128, // OBSOLETE
    /// how much the user staked since trip start. always incremented
    pub trip_accum_stakes: U128,
    /// how much the user unstaked since trip start. always incremented
    pub trip_accum_unstakes: U128,
    /// to compute trip_rewards we start from current_stnear, undo unstakes, undo stakes and finally subtract trip_start_stnear
    /// trip_rewards = current_stnear + trip_accum_unstakes - trip_accum_stakes - trip_start_stnear;
    /// trip_rewards = current_stnear + trip_accum_unstakes - trip_accum_stakes - trip_start_stnear;
    pub trip_rewards: U128,

    //Liquidity Pool
    pub nslp_shares: U128,
    pub nslp_share_value: U128,
    pub nslp_share_bp: u16, //basis points, % user owned
}

/// Struct returned from get_contract_state
/// div-pool state info
/// Represents contact state as as JSON compatible struct
#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
/// check struct MetaPool for explanations on each field
pub struct GetContractStateResult {
    //current env::epoch_height() .- to check gainst unstake-delay end epoch
    pub env_epoch_height: U64,

    pub contract_account_balance: U128String,

    pub total_available: U128String,

    pub total_for_staking: U128String,

    pub total_actually_staked: U128String,

    pub epoch_stake_orders: U128String,
    pub epoch_unstake_orders: U128String,
    pub total_unstaked_and_waiting: U128String,

    pub total_stake_shares: U128String,

    pub st_near_price: U128String,

    pub total_unstake_claims: U128String,

    pub retrieved_for_unstake_claims: U128String, // new name
    pub reserve_for_unstake_claims: U128String, // old name, keep for backward comp

    pub total_meta: U128String,

    pub accumulated_staked_rewards: U128String,

    pub nslp_liquidity: U128String,
    pub nslp_target: U128String,
    pub nslp_stnear_balance: U128String,
    pub nslp_share_price: U128String,
    pub nslp_total_shares: U128String,

    pub nslp_current_discount_basis_points: u16,
    pub nslp_min_discount_basis_points: u16,
    pub nslp_max_discount_basis_points: u16,

    //how many accounts there are
    pub accounts_count: U64,

    //count of pools to diversify in
    pub staking_pools_count: u16,

    pub min_deposit_amount: U128String,

    pub est_meta_rewards_stakers: U128String, //stakers
    pub est_meta_rewards_lp: U128String,      //liquidity-providers
    pub est_meta_rewards_lu: U128String,      //liquid-unstakers
    pub max_meta_rewards_stakers: U128String, //stakers
    pub max_meta_rewards_lp: U128String,      //liquidity-providers
    pub max_meta_rewards_lu: U128String,      //liquid-unstakers
}

/// Struct returned from get_contract_params
/// div-pool parameters info
/// Represents contact parameters as JSON compatible struct
#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct ContractParamsJSON {
    ///NEAR/stNEAR Liquidity pool 1% fee target. If Liquidity=target, fee is 1%
    pub nslp_liquidity_target: U128String,
    ///NEAR/stNEAR Liquidity pool max fee
    pub nslp_max_discount_basis_points: u16, //10%
    ///NEAR/stNEAR Liquidity pool min fee
    pub nslp_min_discount_basis_points: u16, //0.1%

    //The next 3 values define meta rewards multipliers %. (100 => 1x, 200 => 2x, ...)
    ///for each stNEAR paid staking reward, reward stNEAR holders with g-stNEAR. default:5x. reward META = rewards * mult_pct / 100
    pub staker_meta_mult_pct: u16,
    ///for each stNEAR paid as discount, reward stNEAR sellers with g-stNEAR. default:1x. reward META = discounted * mult_pct / 100
    pub stnear_sell_meta_mult_pct: u16,
    ///for each stNEAR paid as discount, reward stNEAR sellers with g-stNEAR. default:20x. reward META = fee * mult_pct / 100
    pub lp_provider_meta_mult_pct: u16,

    /// operator_fee_basis_points. 100 basis point => 1%. E.g.: owner_fee_basis_points=50 => 0.5% owner's fee
    pub operator_rewards_fee_basis_points: u16,
    /// operator_cut_basis_points.
    pub operator_swap_cut_basis_points: u16,
    /// treasury_cut_basis_points.
    pub treasury_swap_cut_basis_points: u16,
    pub min_deposit_amount: U128String,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct RemoveLiquidityResult {
    pub near: U128String,
    pub st_near: U128String,
}

#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct LiquidUnstakeResult {
    pub near: U128String,
    pub fee: U128String,
    pub meta: U128String,
}

// get_staking_pool_list returns StakingPoolJSONInfo[]
#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct StakingPoolJSONInfo {
    pub inx: u16,
    pub account_id: String,
    pub weight_basis_points: u16,
    pub staked: U128String,
    pub unstaked: U128String,
    pub unstaked_requested_epoch_height: U64String,
    //EpochHeight where we asked the sp what were our staking rewards
    pub last_asked_rewards_epoch_height: U64String,
    pub busy_lock: bool,
}

/// struct used as parameter for set_staking_pools
#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct StakingPoolArgItem {
    pub account_id: AccountId, 
    pub weight_basis_points: u16
} 
