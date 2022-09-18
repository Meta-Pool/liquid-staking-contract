use crate::*;
use near_sdk::{near_bindgen, Promise, PublicKey};

#[near_bindgen]
impl MetaPool {
    // OWNER'S METHODS and getters

    /// Adds full access key with the given public key to the account once the contract is empty
    /// (has no accounts)
    /// Requires 50 TGas (2 * BASE_GAS)
    pub fn add_full_access_key(&mut self, new_public_key: Base58PublicKey) -> Promise {
        self.assert_owner_calling();

        assert!(self.accounts.len() == 0, "contract still has accounts");

        env::log(b"Adding a full access key");

        let new_public_key: PublicKey = new_public_key.into();

        Promise::new(env::current_account_id()).add_full_access_key(new_public_key)
    }

    /// Owner's method.
    /// Pauses pool staking.
    pub fn pause_staking(&mut self) {
        self.assert_operator_or_owner();
        assert!(!self.staking_paused, "The staking is already paused");
        self.staking_paused = true;
    }
    /// unPauses pool staking.
    pub fn un_pause_staking(&mut self) {
        self.assert_operator_or_owner();
        assert!(self.staking_paused, "The staking is not paused");
        self.staking_paused = false;
    }

    //---------------------------------
    // staking-pools-list (SPL) management
    //---------------------------------

    // Note: this fn may run out of gas when there are more than 3800 pools registered (current amount is 57)
    // The limit is high, but in case it is needed, recommendation is to add from_index and end_index as parameters in this function
    /// get the current list of pools
    pub fn get_staking_pool_list(&self) -> Vec<StakingPoolJSONInfo> {
        let mut result = Vec::with_capacity(self.staking_pools.len());
        for inx in 0..self.staking_pools.len() {
            let elem = &self.staking_pools[inx];
            result.push(StakingPoolJSONInfo {
                inx: inx as u16,
                account_id: elem.account_id.clone(),
                weight_basis_points: elem.weight_basis_points,
                staked: elem.staked.into(),
                unstaked: elem.unstaked.into(),
                last_asked_rewards_epoch_height: elem.last_asked_rewards_epoch_height.into(),
                unstaked_requested_epoch_height: elem.unstk_req_epoch_height.into(),
                busy_lock: elem.busy_lock,
            })
        }
        return result;
    }

    ///remove staking pool from list *if it's empty*
    pub fn remove_staking_pool(&mut self, inx: u16) {
        self.assert_operator_or_owner();

        let sp = &self.staking_pools[inx as usize];
        if !sp.is_empty() {
            panic!("sp is not empty")
        }
        self.staking_pools.remove(inx as usize);
    }

    /// add a new staking pool, checking that it is not already in the list
    /// added with weight_basis_points = 0, to preserve sum(weights)=100%
    pub fn add_staking_pool(&mut self, account_id: AccountId) {
        self.assert_operator_or_owner();
        assert!(
            account_id.ends_with(".poolv1.near") || account_id.ends_with(".testnet"),
            "invalid staking-pool contract account {}", account_id);
        // search the pools
        for sp_inx in 0..self.staking_pools.len() {
            if self.staking_pools[sp_inx].account_id == account_id {
                // found
                panic!("already in list");
            }
        }
        // not in list, add
        self.staking_pools
            .push(StakingPoolInfo::new(account_id, 0));
    }

    /// update existing staking pools list, field weight_basis_points
    /// sum(weight_basis_points) must be eq 100%
    /// can not add, remove or change order of staking pools
    #[payable]
    pub fn set_staking_pools(&mut self, list: Vec<StakingPoolArgItem>) {
        assert_one_yocto();
        self.assert_operator_or_owner();
        // make sure no additions or removals
        assert_eq!(list.len(),self.staking_pools.len());
        // process the list
        let mut total_weight = 0;
        for sp_inx in 0..list.len() {
            // assert same order
            assert_eq!(self.staking_pools[sp_inx].account_id, list[sp_inx].account_id);
            // get weight_basis_points to set
            let bp = list[sp_inx].weight_basis_points;
            // no staking pool can have 50% or more
            assert!(bp<5000);
            // if there's a change
            if self.staking_pools[sp_inx].weight_basis_points != bp {
                // check pool is not busy
                assert!(!self.staking_pools[sp_inx].busy_lock,"sp {} is busy",sp_inx);
                // set new value
                self.staking_pools[sp_inx].weight_basis_points = bp;
            }
            // keep totals
            total_weight += bp;
        }
        assert_eq!(total_weight,10000);
    }

    //--------------------------------------------------
    /// computes unstaking delay on current situation
    pub fn compute_current_unstaking_delay(&self, amount: U128String) -> u16 {
        return self.internal_compute_current_unstaking_delay(amount.0) as u16;
    }

    //---------------------------------
    // owner & operator accounts
    //---------------------------------

    pub fn get_operator_account_id(&self) -> AccountId {
        return self.operator_account_id.clone();
    }
    pub fn set_operator_account_id(&mut self, account_id: AccountId) {
        assert!(env::is_valid_account_id(account_id.as_bytes()));
        self.assert_owner_calling();
        self.operator_account_id = account_id;
        //all key accounts must be different
        self.assert_key_accounts_are_different();
    }
    pub fn get_treasury_account_id(&self) -> AccountId {
        return self.treasury_account_id.clone();
    }
    pub fn set_treasury_account_id(&mut self, account_id: AccountId) {
        assert!(env::is_valid_account_id(account_id.as_bytes()));
        self.assert_owner_calling();
        self.treasury_account_id = account_id;
        self.assert_key_accounts_are_different();
    }
    pub fn set_owner_id(&mut self, owner_id: AccountId) {
        assert!(env::is_valid_account_id(owner_id.as_bytes()));
        self.assert_owner_calling();
        self.owner_account_id = owner_id.into();
        self.assert_key_accounts_are_different();
    }

    /// The amount of tokens that were deposited to the staking pool.
    /// NOTE: The actual balance can be larger than this known deposit balance due to staking
    /// rewards acquired on the staking pool.
    /// To refresh the amount the owner can call `refresh_staking_pool_balance`.
    pub fn get_known_deposited_balance(&self) -> U128String {
        return self.total_actually_staked.into();
    }

    /// full account info
    /// Returns JSON representation of the account for the given account ID.
    pub fn get_account_info(&self, account_id: AccountId) -> GetAccountInfoResult {
        let acc = self.internal_get_account(&account_id);
        let staked_near = self.amount_from_stake_shares(acc.stake_shares);
        // trip_rewards = current_stnear + trip_accum_unstakes - trip_accum_stakes - trip_start_stnear;
        // note: trip_start_stnear is OBSOLETE
        // let trip_rewards = (staked_near + acc.trip_accum_unstakes)
        //     .saturating_sub(acc.trip_accum_stakes + acc.trip_start_stnear);
        //Liquidity Pool share value
        let mut nslp_share_value: u128 = 0;
        let mut nslp_share_bp: u16 = 0;
        if acc.nslp_shares != 0 {
            let nslp_account = self.internal_get_nslp_account();
            nslp_share_value = acc.valued_nslp_shares(self, &nslp_account); //in NEAR
            nslp_share_bp = proportional(10_000, acc.nslp_shares, nslp_account.nslp_shares) as u16;
        }
        return GetAccountInfoResult {
            account_id,
            available: acc.available.into(),
            st_near: acc.stake_shares.into(),
            valued_st_near: staked_near.into(),
            meta: acc.total_meta(self).into(),
            realized_meta: acc.realized_meta.into(),
            unstaked: acc.unstaked.into(),
            unstaked_requested_unlock_epoch: acc.unstaked_requested_unlock_epoch.into(),
            unstake_full_epochs_wait_left: acc
                .unstaked_requested_unlock_epoch
                .saturating_sub(env::epoch_height())
                as u16,
            can_withdraw: (env::epoch_height() >= acc.unstaked_requested_unlock_epoch),
            total: (acc.available + staked_near + acc.unstaked).into(),
            // trip-meter
            trip_start: acc.trip_start.into(),
            trip_start_stnear: acc.trip_start_stnear.into(), // note: OBSOLETE/REPURPOSED
            trip_accum_stakes: (if acc.staking_meter.delta_staked >= 0 {
                acc.staking_meter.delta_staked as u128
            } else {
                0 as u128
            })
            .into(),
            trip_accum_unstakes: (if acc.staking_meter.delta_staked < 0 {
                -acc.staking_meter.delta_staked as u128
            } else {
                0 as u128
            })
            .into(),
            trip_rewards: (staked_near + acc.trip_accum_unstakes)
                .saturating_sub(acc.trip_accum_stakes)
                .into(), // extra-nears not related to stake/unstake or transfers

            nslp_shares: acc.nslp_shares.into(),
            nslp_share_value: nslp_share_value.into(),
            nslp_share_bp, //% owned as basis points
        };
    }

    /// NEP-129 get information about this contract
    /// returns JSON string according to [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
    pub fn get_contract_info(&self) -> NEP129Response {
        return NEP129Response {
            dataVersion: 1,
            name: CONTRACT_NAME.into(),
            version: CONTRACT_VERSION.into(),
            source: "https://github.com/Narwallets/meta-pool".into(),
            standards: vec!["NEP-141".into(), "NEP-145".into(), "SP".into()], //SP=>core-contracts/Staking-pool
            webAppUrl: self.web_app_url.clone(),
            developersAccountId: DEVELOPERS_ACCOUNT_ID.into(),
            auditorAccountId: self.auditor_account_id.clone(),
        };
    }

    /// sets configurable contract info [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
    // Note: params are not Option<String> so the user can not inadvertently set null to data by not including the argument
    pub fn set_contract_info(&mut self, web_app_url: String, auditor_account_id: String) {
        self.assert_owner_calling();
        self.web_app_url = if web_app_url.len() > 0 {
            Some(web_app_url)
        } else {
            None
        };
        self.auditor_account_id = if auditor_account_id.len() > 0 {
            Some(auditor_account_id)
        } else {
            None
        };
    }

    // simple fn to get st_near_price for use by cross-contract calls 
    pub fn get_st_near_price(&self) -> U128String {
        // get how many near one stNEAR is worth
        self.amount_from_stake_shares(ONE_E24).into()
    }

    /// get contract totals
    /// Returns JSON representation of the contract state
    pub fn get_contract_state(&self) -> GetContractStateResult {
        let nslp_account = self.internal_get_nslp_account();

        return GetContractStateResult {
            env_epoch_height: env::epoch_height().into(),
            contract_account_balance: self.contract_account_balance.into(),
            total_available: self.total_available.into(),
            total_for_staking: self.total_for_staking.into(),
            total_actually_staked: self.total_actually_staked.into(),
            epoch_stake_orders: self.epoch_stake_orders.into(),
            epoch_unstake_orders: self.epoch_unstake_orders.into(),
            total_unstaked_and_waiting: self.total_unstaked_and_waiting.into(),
            accumulated_staked_rewards: self.accumulated_staked_rewards.into(),
            total_unstake_claims: self.total_unstake_claims.into(),
            retrieved_for_unstake_claims: self.retrieved_for_unstake_claims.into(),
            reserve_for_unstake_claims: self.retrieved_for_unstake_claims.into(),
            total_stake_shares: self.total_stake_shares.into(), // stNEAR total supply
            st_near_price: self.amount_from_stake_shares(ONE_E24).into(), //how much nears are 1 stNEAR
            total_meta: self.total_meta.into(),
            accounts_count: self.accounts.len().into(),
            staking_pools_count: self.staking_pools.len() as u16,
            nslp_liquidity: nslp_account.available.into(),
            nslp_stnear_balance: nslp_account.stake_shares.into(), //how much stnear does the nslp have?
            nslp_target: self.nslp_liquidity_target.into(),
            nslp_share_price: self.amount_from_nslp_shares(ONE_E24, &nslp_account).into(), // price of one LP share (1e24 yocto_shares)
            nslp_total_shares: nslp_account.nslp_shares.into(), // total nspl shares. price = value/total_shares
            nslp_current_discount_basis_points: self
                .internal_get_discount_basis_points(nslp_account.available, TEN_NEAR),
            nslp_min_discount_basis_points: self.nslp_min_discount_basis_points,
            nslp_max_discount_basis_points: self.nslp_max_discount_basis_points,
            min_deposit_amount: self.min_deposit_amount.into(),
            est_meta_rewards_stakers: self.est_meta_rewards_stakers.into(),
            est_meta_rewards_lu: self.est_meta_rewards_lu.into(), //liquid-unstakers
            est_meta_rewards_lp: self.est_meta_rewards_lp.into(), //liquidity-providers
            max_meta_rewards_stakers: self.max_meta_rewards_stakers.into(),
            max_meta_rewards_lu: self.max_meta_rewards_lu.into(), //liquid-unstakers
            max_meta_rewards_lp: self.max_meta_rewards_lp.into(), //liquidity-providers
        };
    }

    /// Returns JSON representation of contract parameters
    pub fn get_contract_params(&self) -> ContractParamsJSON {
        return ContractParamsJSON {
            nslp_liquidity_target: self.nslp_liquidity_target.into(),
            nslp_max_discount_basis_points: self.nslp_max_discount_basis_points,
            nslp_min_discount_basis_points: self.nslp_min_discount_basis_points,

            staker_meta_mult_pct: self.staker_meta_mult_pct,
            stnear_sell_meta_mult_pct: self.stnear_sell_meta_mult_pct,
            lp_provider_meta_mult_pct: self.lp_provider_meta_mult_pct,
            operator_rewards_fee_basis_points: self.operator_rewards_fee_basis_points,
            operator_swap_cut_basis_points: self.operator_swap_cut_basis_points,
            treasury_swap_cut_basis_points: self.treasury_swap_cut_basis_points,

            min_deposit_amount: self.min_deposit_amount.into(),
        };
    }

    /// Sets contract parameters
    pub fn set_contract_params(&mut self, params: ContractParamsJSON) {
        self.assert_operator_or_owner();
        assert!(params.nslp_max_discount_basis_points > params.nslp_min_discount_basis_points);

        self.nslp_liquidity_target = params.nslp_liquidity_target.0;
        self.nslp_max_discount_basis_points = params.nslp_max_discount_basis_points;
        self.nslp_min_discount_basis_points = params.nslp_min_discount_basis_points;

        self.staker_meta_mult_pct = params.staker_meta_mult_pct;
        self.stnear_sell_meta_mult_pct = params.stnear_sell_meta_mult_pct;
        self.lp_provider_meta_mult_pct = params.lp_provider_meta_mult_pct;
        self.operator_rewards_fee_basis_points = params.operator_rewards_fee_basis_points;
        self.operator_swap_cut_basis_points = params.operator_swap_cut_basis_points;
        self.treasury_swap_cut_basis_points = params.treasury_swap_cut_basis_points;

        self.min_deposit_amount = params.min_deposit_amount.0;
    }

    /// Sets contract parameters
    pub fn set_reward_multipliers(
        &mut self,
        stakers_pct: u16,
        lp_pct: u16,
        liquid_unstake_pct: u16,
    ) {
        self.assert_operator_or_owner();
        self.staker_meta_mult_pct = stakers_pct;
        self.stnear_sell_meta_mult_pct = liquid_unstake_pct;
        self.lp_provider_meta_mult_pct = lp_pct;
    }

    /// Sets contract parameters
    pub fn set_max_meta_rewards(&mut self, stakers: u32, lu: u32, lp: u32) {
        self.assert_operator_or_owner();
        self.max_meta_rewards_stakers = stakers as u128 * ONE_NEAR; //stakers
        self.max_meta_rewards_lu = lu as u128 * ONE_NEAR; //liquid-unstakers
        self.max_meta_rewards_lp = lp as u128 * ONE_NEAR; //liquidity-providers
    }

    /// get sp (staking-pool) info
    /// Returns JSON representation of sp recorded state
    pub fn get_sp_info(&self, inx: u16) -> StakingPoolJSONInfo {
        assert!((inx as usize) < self.staking_pools.len());
        let sp = &self.staking_pools[inx as usize];

        return StakingPoolJSONInfo {
            inx,
            account_id: sp.account_id.clone(),
            weight_basis_points: sp.weight_basis_points.clone(),
            staked: sp.staked.into(),
            unstaked: sp.unstaked.into(),
            unstaked_requested_epoch_height: sp.unstk_req_epoch_height.into(),
            last_asked_rewards_epoch_height: sp.last_asked_rewards_epoch_height.into(),
            busy_lock: sp.busy_lock,
        };
    }
}
