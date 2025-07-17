//------------------------------------
//------------------------------------
//---- TEST ONLY CONTRACT ------------
//------------------------------------
//------------------------------------
use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::{
    env, ext_contract, is_promise_success, log, near_bindgen, AccountId, Balance, PanicOnDefault,
    PromiseOrValue,
};

#[cfg(target_arch = "wasm32")]
use near_sdk::env::BLOCKCHAIN_INTERFACE;

mod owner;

const TGAS: u64 = 1_000_000_000_000;

//contract state
#[near_bindgen]
#[derive(BorshDeserialize, BorshSerialize, PanicOnDefault)]
pub struct TestContract {
    //test state
    pub saved_message: String,
    pub saved_i32: i32,
    //last response received
    pub last_epoch: u64,
    // dao
    pub owner_id: AccountId,
}

const ONE_NEAR: Balance = 1_000_000_000_000_000_000_000_000;
const NEAR: Balance = ONE_NEAR;

type U128String = U128;

#[ext_contract(ext_staking_pool)]
pub trait ExtStakingPool {
    fn get_account_staked_balance(&self, account_id: AccountId) -> U128String;

    fn get_account_unstaked_balance(&self, account_id: AccountId) -> U128String;

    fn get_account_total_balance(&self, account_id: AccountId) -> U128String;

    fn deposit(&mut self);

    fn deposit_and_stake(&mut self);

    fn withdraw(&mut self, amount: U128String);
    fn withdraw_all(&mut self);

    fn stake(&mut self, amount: U128String);

    fn unstake(&mut self, amount: U128String);

    fn unstake_all(&mut self);
}

#[ext_contract(ext_self_owner)]
pub trait SelfCallbacks {
    fn on_get_sp_total_balance(&mut self, big_amount: u128, #[callback] total_balance: U128String);
}

#[near_bindgen]
impl TestContract {
    #[init]
    pub fn new() -> Self {
        /* Prevent re-initializations */
        assert!(!env::state_exists(), "This contract is already initialized");
        return Self {
            saved_message: String::from("init"),
            saved_i32: 0,
            last_epoch: env::epoch_height(),
            owner_id: AccountId::new_unchecked("dao2.pool.testnet".to_string()),
        };
    }

    // ------------------------------
    // to test Sputnik V2 remote-upgrade
    // ------------------------------
    /// get version ()
    pub fn get_version(&self) -> String {
        "2.0.0 BLOCKCHAIN_INTERFACE".into()
    }

    // ------------------------------
    // Main methods
    // ------------------------------
    #[payable]
    pub fn set_message(&mut self, message: String) {
        self.saved_message = message;
    }
    #[payable]
    pub fn set_i32(&mut self, num: i32) {
        self.saved_i32 = num;
    }

    pub fn get_message(&self) -> String {
        return self.saved_message.clone();
    }

    ///Make a request to the dia-gateway smart contract
    pub fn get_epoch_height(&self) -> u64 {
        return env::epoch_height();
    }

    ///Make a request to the dia-gateway smart contract
    pub fn get_block_index(&self) -> u64 {
        return env::block_height();
    }

    // ------------------------------
    //Test u128 as argument type in a callback
    // ------------------------------
    pub fn test_callbacks(&self) -> PromiseOrValue<u128> {
        let big_amount: u128 = u128::MAX;
        //query our current balance (includes staked+unstaked+staking rewards)
        ext_staking_pool::ext(AccountId::new_unchecked("meta.pool.testnet".to_string()))
            .with_static_gas(near_sdk::Gas(10 * TGAS))
            .get_account_total_balance(AccountId::new_unchecked("lucio.testnet".to_string()))
            .then(
                ext_self_owner::ext(env::current_account_id())
                    .with_static_gas(near_sdk::Gas(10 * TGAS))
                    .on_get_sp_total_balance(big_amount),
            )
            .into()
    }
    //prev-fn continues here
    #[private]
    pub fn on_get_sp_total_balance(
        &mut self,
        big_amount: u128,
        #[callback] balance: U128String,
    ) -> U128String {
        log!(
            "is_promise_success:{} big_amount:{} big_amount(nears):{} balance:{}",
            is_promise_success(),
            big_amount,
            big_amount / NEAR,
            balance.0
        );
        return balance;
    }

    #[cfg(target_arch = "wasm32")]
    pub fn upgrade(self) {
        assert!(
            env::prepaid_gas() > 150 * TGAS,
            "set 200TGAS or more for this transaction"
        );
        log!("start env::used_gas = {}", env::used_gas());
        //assert!(env::predecessor_account_id() == self.owner_id);
        //input is code:<Vec<u8> on REGISTER 0
        //log!("bytes.length {}", code.unwrap().len());
        const BLOCKCHAIN_INTERFACE_NOT_SET_ERR: &str = "Blockchain interface not set.";
        //assert!(env::predecessor_account_id() == self.controlling_dao);
        let current_id = env::current_account_id().into_bytes();
        let method_name = "migrate".as_bytes().to_vec();
        let attached_gas_pre = env::prepaid_gas() - env::used_gas();
        log!(
            "(1) attached_gas {} env::prepaid_gas(){} - env::used_gas(){}",
            attached_gas_pre,
            env::prepaid_gas(),
            env::used_gas()
        );
        unsafe {
            BLOCKCHAIN_INTERFACE.with(|b| {
                // Load input (new contract code) into register 0
                b.borrow()
                    .as_ref()
                    .expect(BLOCKCHAIN_INTERFACE_NOT_SET_ERR)
                    .input(0);

                //prepare self-call promise
                let promise_id = b
                    .borrow()
                    .as_ref()
                    .expect(BLOCKCHAIN_INTERFACE_NOT_SET_ERR)
                    .promise_batch_create(current_id.len() as _, current_id.as_ptr() as _);

                // 1st item, upgrade code (takes data from register 0)
                // Note: this "promise preparation" CONSUMES an important amount of gas
                // because at this point the WASM code is checked and "compiled"
                // total gas cost formula is: (2 * 184765750000 + contract_size_in_bytes * (6812999 + 64572944) + 2 * 108059500000)
                // https://github.com/Narwallets/meta-pool/issues/21
                b.borrow()
                    .as_ref()
                    .expect(BLOCKCHAIN_INTERFACE_NOT_SET_ERR)
                    .promise_batch_action_deploy_contract(promise_id, u64::MAX as _, 0);

                const GAS_FOR_THE_REST_OF_THIS_FUNCTION:u64 = 10*TGAS;
                let gas_for_migration = env::prepaid_gas() - env::used_gas() - GAS_FOR_THE_REST_OF_THIS_FUNCTION;
                log!(
                    "(2) gas_for_migration:{} env::prepaid_gas(){} - env::used_gas(){} - GAS_FOR_THE_REST_OF_THIS_FUNCTION {}",
                    gas_for_migration,
                    env::prepaid_gas(),
                    env::used_gas(),
                    GAS_FOR_THE_REST_OF_THIS_FUNCTION
                );
                //2nd item, schedule a call to "migrate".- (will execute on the *new code*)
                b.borrow()
                    .as_ref()
                    .expect(BLOCKCHAIN_INTERFACE_NOT_SET_ERR)
                    .promise_batch_action_function_call(
                        promise_id,
                        method_name.len() as _,
                        method_name.as_ptr() as _,
                        0 as _,
                        0 as _,
                        0 as _,
                        gas_for_migration,
                    );
            });
        }
    }
}
