use std::sync::Arc;

use ethers::{
    abi::{decode, ethabi::Bytes, ParamType, Token},
    providers::Middleware,
    types::{Log, H160, H256, I256, U256, U64},
};
use num_bigfloat::BigFloat;

use crate::{
    abi, batch_requests,
    errors::{ArithmeticError, CFMMError},
};
use serde::{Deserialize, Serialize};

pub const MIN_SQRT_RATIO: U256 = U256([4295128739, 0, 0, 0]);
pub const MAX_SQRT_RATIO: U256 = U256([6743328256752651558, 17280870778742802505, 4294805859, 0]);
pub const SWAP_EVENT_SIGNATURE: H256 = H256([
    196, 32, 121, 249, 74, 99, 80, 215, 230, 35, 95, 41, 23, 73, 36, 249, 40, 204, 42, 200, 24,
    235, 100, 254, 216, 0, 78, 17, 95, 188, 202, 103,
]);

pub const U256_TWO: U256 = U256([2, 0, 0, 0]);
pub const Q128: U256 = U256([0, 0, 1, 0]);
pub const Q224: U256 = U256([0, 0, 0, 4294967296]);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct UniswapV3Pool {
    pub address: H160,
    pub token_a: H160,
    pub token_a_decimals: u8,
    pub token_b: H160,
    pub token_b_decimals: u8,
    pub liquidity: u128,
    pub sqrt_price: U256,
    pub fee: u32,
    pub tick: i32,
    pub tick_spacing: i32,
    pub liquidity_net: i128,
}

impl UniswapV3Pool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address: H160,
        token_a: H160,
        token_a_decimals: u8,
        token_b: H160,
        token_b_decimals: u8,
        fee: u32,
        liquidity: u128,
        sqrt_price: U256,
        tick: i32,
        tick_spacing: i32,
        liquidity_net: i128,
    ) -> UniswapV3Pool {
        UniswapV3Pool {
            address,
            token_a,
            token_a_decimals,
            token_b,
            token_b_decimals,
            fee,
            liquidity,
            sqrt_price,
            tick,
            tick_spacing,
            liquidity_net,
        }
    }

    //Creates a new instance of the pool from the pair address
    pub async fn new_from_address<M: Middleware>(
        pair_address: H160,
        middleware: Arc<M>,
    ) -> Result<Self, CFMMError<M>> {
        let mut pool = UniswapV3Pool {
            address: pair_address,
            token_a: H160::zero(),
            token_a_decimals: 0,
            token_b: H160::zero(),
            token_b_decimals: 0,
            liquidity: 0,
            sqrt_price: U256::zero(),
            tick: 0,
            tick_spacing: 0,
            fee: 0,
            liquidity_net: 0,
        };

        pool.get_pool_data(middleware.clone()).await?;

        if !pool.data_is_populated() {
            return Err(CFMMError::PoolDataError);
        }

        Ok(pool)
    }

    pub async fn new_from_event_log<M: Middleware>(
        log: Log,
        middleware: Arc<M>,
    ) -> Result<Self, CFMMError<M>> {
        let tokens = ethers::abi::decode(&[ParamType::Uint(32), ParamType::Address], &log.data)?;
        let pair_address = tokens[1].to_owned().into_address().unwrap();
        UniswapV3Pool::new_from_address(pair_address, middleware).await
    }

    pub fn new_empty_pool_from_event_log<M: Middleware>(log: Log) -> Result<Self, CFMMError<M>> {
        let tokens = ethers::abi::decode(&[ParamType::Uint(32), ParamType::Address], &log.data)?;
        let token_a = H160::from(log.topics[0]);
        let token_b = H160::from(log.topics[1]);
        let fee = tokens[0].to_owned().into_uint().unwrap().as_u32();
        let address = tokens[1].to_owned().into_address().unwrap();

        Ok(UniswapV3Pool {
            address,
            token_a,
            token_b,
            token_a_decimals: 0,
            token_b_decimals: 0,
            fee,
            liquidity: 0,
            sqrt_price: U256::zero(),
            tick_spacing: 0,
            tick: 0,
            liquidity_net: 0,
        })
    }

    pub fn fee(&self) -> u32 {
        self.fee
    }

    pub async fn get_pool_data<M: Middleware>(
        &mut self,
        middleware: Arc<M>,
    ) -> Result<(), CFMMError<M>> {
        batch_requests::uniswap_v3::get_v3_pool_data_batch_request(self, middleware.clone())
            .await?;

        Ok(())
    }

    pub fn data_is_populated(&self) -> bool {
        !(self.token_a.is_zero() || self.token_b.is_zero())
    }

    pub async fn get_tick_word<M: Middleware>(
        &self,
        tick: i32,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        let v3_pool = abi::IUniswapV3Pool::new(self.address, middleware);
        let (word_position, _) = uniswap_v3_math::tick_bitmap::position(tick);
        Ok(v3_pool.tick_bitmap(word_position).call().await?)
    }

    pub async fn get_next_word<M: Middleware>(
        &self,
        word_position: i16,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        let v3_pool = abi::IUniswapV3Pool::new(self.address, middleware);
        Ok(v3_pool.tick_bitmap(word_position).call().await?)
    }

    pub async fn get_tick_spacing<M: Middleware>(
        &self,
        middleware: Arc<M>,
    ) -> Result<i32, CFMMError<M>> {
        let v3_pool = abi::IUniswapV3Pool::new(self.address, middleware);
        Ok(v3_pool.tick_spacing().call().await?)
    }

    pub async fn get_tick<M: Middleware>(&self, middleware: Arc<M>) -> Result<i32, CFMMError<M>> {
        Ok(self.get_slot_0(middleware).await?.1)
    }

    pub async fn get_tick_info<M: Middleware>(
        &self,
        tick: i32,
        middleware: Arc<M>,
    ) -> Result<(u128, i128, U256, U256, i64, U256, u32, bool), CFMMError<M>> {
        let v3_pool = abi::IUniswapV3Pool::new(self.address, middleware.clone());

        let tick_info = v3_pool.ticks(tick).call().await?;

        Ok((
            tick_info.0,
            tick_info.1,
            tick_info.2,
            tick_info.3,
            tick_info.4,
            tick_info.5,
            tick_info.6,
            tick_info.7,
        ))
    }

    pub async fn get_liquidity_net<M: Middleware>(
        &self,
        tick: i32,
        middleware: Arc<M>,
    ) -> Result<i128, CFMMError<M>> {
        let tick_info = self.get_tick_info(tick, middleware).await?;
        Ok(tick_info.1)
    }

    pub async fn get_initialized<M: Middleware>(
        &self,
        tick: i32,
        middleware: Arc<M>,
    ) -> Result<bool, CFMMError<M>> {
        let tick_info = self.get_tick_info(tick, middleware).await?;
        Ok(tick_info.7)
    }

    pub async fn get_slot_0<M: Middleware>(
        &self,
        middleware: Arc<M>,
    ) -> Result<(U256, i32, u16, u16, u16, u8, bool), CFMMError<M>> {
        let v3_pool = abi::IUniswapV3Pool::new(self.address, middleware);
        Ok(v3_pool.slot_0().call().await?)
    }

    pub async fn get_liquidity<M: Middleware>(
        &self,
        middleware: Arc<M>,
    ) -> Result<u128, CFMMError<M>> {
        let v3_pool = abi::IUniswapV3Pool::new(self.address, middleware);
        Ok(v3_pool.liquidity().call().await?)
    }

    pub async fn get_sqrt_price<M: Middleware>(
        &self,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        Ok(self.get_slot_0(middleware).await?.0)
    }

    pub async fn sync_pool<M: Middleware>(
        &mut self,
        middleware: Arc<M>,
    ) -> Result<(), CFMMError<M>> {
        batch_requests::uniswap_v3::sync_v3_pool_batch_request(self, middleware.clone()).await?;
        Ok(())
    }

    pub async fn update_pool_from_swap_log<M: Middleware>(
        &mut self,
        swap_log: &Log,
        middleware: Arc<M>,
    ) -> Result<(), CFMMError<M>> {
        (_, _, self.sqrt_price, self.liquidity, self.tick) = self.decode_swap_log(swap_log);

        self.liquidity_net = self.get_liquidity_net(self.tick, middleware).await?;

        Ok(())
    }

    //Returns reserve0, reserve1
    pub fn decode_swap_log(&self, swap_log: &Log) -> (I256, I256, U256, u128, i32) {
        let log_data = decode(
            &[
                ParamType::Int(256),  //amount0
                ParamType::Int(256),  //amount1
                ParamType::Uint(160), //sqrtPriceX96
                ParamType::Uint(128), //liquidity
                ParamType::Int(24),
            ],
            &swap_log.data,
        )
        .expect("Could not get log data");

        let amount_0 = I256::from_raw(log_data[1].to_owned().into_int().unwrap());
        let amount_1 = I256::from_raw(log_data[1].to_owned().into_int().unwrap());
        let sqrt_price = log_data[2].to_owned().into_uint().unwrap();
        let liquidity = log_data[3].to_owned().into_uint().unwrap().as_u128();
        let tick = log_data[4].to_owned().into_uint().unwrap().as_u32() as i32;

        (amount_0, amount_1, sqrt_price, liquidity, tick)
    }

    pub async fn get_token_decimals<M: Middleware>(
        &mut self,
        middleware: Arc<M>,
    ) -> Result<(u8, u8), CFMMError<M>> {
        let token_a_decimals = abi::IErc20::new(self.token_a, middleware.clone())
            .decimals()
            .call()
            .await?;

        let token_b_decimals = abi::IErc20::new(self.token_b, middleware)
            .decimals()
            .call()
            .await?;

        Ok((token_a_decimals, token_b_decimals))
    }

    pub async fn get_fee<M: Middleware>(
        &mut self,
        middleware: Arc<M>,
    ) -> Result<u32, CFMMError<M>> {
        let fee = abi::IUniswapV3Pool::new(self.address, middleware)
            .fee()
            .call()
            .await?;

        Ok(fee)
    }

    pub async fn get_token_0<M: Middleware>(
        &self,
        middleware: Arc<M>,
    ) -> Result<H160, CFMMError<M>> {
        let v2_pair = abi::IUniswapV2Pair::new(self.address, middleware);

        let token0 = match v2_pair.token_0().call().await {
            Ok(result) => result,
            Err(contract_error) => return Err(CFMMError::ContractError(contract_error)),
        };

        Ok(token0)
    }

    pub async fn get_token_1<M: Middleware>(
        &self,
        middleware: Arc<M>,
    ) -> Result<H160, CFMMError<M>> {
        let v2_pair = abi::IUniswapV2Pair::new(self.address, middleware);

        let token1 = match v2_pair.token_1().call().await {
            Ok(result) => result,
            Err(contract_error) => return Err(CFMMError::ContractError(contract_error)),
        };

        Ok(token1)
    }
    /* Legend:
       sqrt(price) = sqrt(y/x)
       L = sqrt(x*y)
       ==> x = L^2/price
       ==> y = L^2*price
    */
    pub fn calculate_virtual_reserves(&self) -> Result<(u128, u128), ArithmeticError> {
        let price: f64 = self.calculate_price(self.token_a);

        let sqrt_price = BigFloat::from_f64(price.sqrt());
        let liquidity = BigFloat::from_u128(self.liquidity);

        //Sqrt price is stored as a Q64.96 so we need to left shift the liquidity by 96 to be represented as Q64.96
        //We cant right shift sqrt_price because it could move the value to 0, making divison by 0 to get reserve_x
        let liquidity = liquidity;

        let (reserve_0, reserve_1) = if !sqrt_price.is_zero() {
            let reserve_x = liquidity.div(&sqrt_price);
            let reserve_y = liquidity.mul(&sqrt_price);

            (reserve_x, reserve_y)
        } else {
            (BigFloat::from(0), BigFloat::from(0))
        };

        Ok((
            reserve_0
                .to_u128()
                .expect("Could not convert reserve_0 to uint128"),
            reserve_1
                .to_u128()
                .expect("Could not convert reserve_1 to uint128"),
        ))
    }

    pub fn calculate_price(&self, base_token: H160) -> f64 {
        let tick = uniswap_v3_math::tick_math::get_tick_at_sqrt_ratio(self.sqrt_price).unwrap();
        let shift = self.token_a_decimals as i8 - self.token_b_decimals as i8;
        let price = if shift < 0 {
            1.0001_f64.powi(tick) / 10_f64.powi(-shift as i32)
        } else {
            1.0001_f64.powi(tick) * 10_f64.powi(shift as i32)
        };

        if base_token == self.token_a {
            price
        } else {
            1.0 / price
        }
    }

    pub fn address(&self) -> H160 {
        self.address
    }

    pub async fn simulate_swap_mut_with_cache<M: Middleware>(
        &mut self,
        token_in: H160,
        amount_in: U256,
        num_ticks: u16,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        if amount_in.is_zero() {
            return Ok(U256::zero());
        }

        let zero_for_one = token_in == self.token_a;

        //TODO: make this a queue instead of vec and then an iterator FIXME::
        let (mut tick_data, block_number) =
            batch_requests::uniswap_v3::get_uniswap_v3_tick_data_batch_request(
                self,
                self.tick,
                zero_for_one,
                num_ticks,
                None,
                middleware.clone(),
            )
            .await?;

        let mut tick_data_iter = tick_data.iter();

        //Set sqrt_price_limit_x_96 to the max or min sqrt price in the pool depending on zero_for_one
        let sqrt_price_limit_x_96 = if zero_for_one {
            MIN_SQRT_RATIO + 1
        } else {
            MAX_SQRT_RATIO - 1
        };

        //Initialize a mutable state state struct to hold the dynamic simulated state of the pool
        let mut current_state = CurrentState {
            sqrt_price_x_96: self.sqrt_price, //Active price on the pool
            amount_calculated: I256::zero(),  //Amount of token_out that has been calculated
            amount_specified_remaining: I256::from_raw(amount_in), //Amount of token_in that has not been swapped
            tick: self.tick,                                       //Current i24 tick of the pool
            liquidity: self.liquidity, //Current available liquidity in the tick range
        };

        let mut liquidity_net = self.liquidity_net;

        while current_state.amount_specified_remaining != I256::zero()
            && current_state.sqrt_price_x_96 != sqrt_price_limit_x_96
        {
            //Initialize a new step struct to hold the dynamic state of the pool at each step
            let mut step = StepComputations {
                sqrt_price_start_x_96: current_state.sqrt_price_x_96, //Set the sqrt_price_start_x_96 to the current sqrt_price_x_96
                ..Default::default()
            };

            let next_tick_data = if let Some(tick_data) = tick_data_iter.next() {
                tick_data
            } else {
                (tick_data, _) =
                    batch_requests::uniswap_v3::get_uniswap_v3_tick_data_batch_request(
                        self,
                        current_state.tick,
                        zero_for_one,
                        num_ticks,
                        Some(block_number),
                        middleware.clone(),
                    )
                    .await?;

                tick_data_iter = tick_data.iter();

                if let Some(tick_data) = tick_data_iter.next() {
                    tick_data
                } else {
                    //This should never happen, but if it does, we should return an error because something is wrong
                    return Err(CFMMError::NoInitializedTicks);
                }
            };

            step.tick_next = next_tick_data.tick;

            // ensure that we do not overshoot the min/max tick, as the tick bitmap is not aware of these bounds
            //Note: this could be removed as we are clamping in the batch contract
            step.tick_next = step.tick_next.clamp(MIN_TICK, MAX_TICK);

            //Get the next sqrt price from the input amount
            step.sqrt_price_next_x96 =
                uniswap_v3_math::tick_math::get_sqrt_ratio_at_tick(step.tick_next)?;

            //Target spot price
            let swap_target_sqrt_ratio = if zero_for_one {
                if step.sqrt_price_next_x96 < sqrt_price_limit_x_96 {
                    sqrt_price_limit_x_96
                } else {
                    step.sqrt_price_next_x96
                }
            } else if step.sqrt_price_next_x96 > sqrt_price_limit_x_96 {
                sqrt_price_limit_x_96
            } else {
                step.sqrt_price_next_x96
            };

            //Compute swap step and update the current state
            (
                current_state.sqrt_price_x_96,
                step.amount_in,
                step.amount_out,
                step.fee_amount,
            ) = uniswap_v3_math::swap_math::compute_swap_step(
                current_state.sqrt_price_x_96,
                swap_target_sqrt_ratio,
                current_state.liquidity,
                current_state.amount_specified_remaining,
                self.fee,
            )?;

            //Decrement the amount remaining to be swapped and amount received from the step
            current_state.amount_specified_remaining = current_state
                .amount_specified_remaining
                .overflowing_sub(I256::from_raw(
                    step.amount_in.overflowing_add(step.fee_amount).0,
                ))
                .0;

            current_state.amount_calculated -= I256::from_raw(step.amount_out);

            //If the price moved all the way to the next price, recompute the liquidity change for the next iteration
            if current_state.sqrt_price_x_96 == step.sqrt_price_next_x96 {
                if next_tick_data.initialized {
                    liquidity_net = next_tick_data.liquidity_net;

                    // we are on a tick boundary, and the next tick is initialized, so we must charge a protocol fee
                    if zero_for_one {
                        liquidity_net = -liquidity_net;
                    }

                    current_state.liquidity = if liquidity_net < 0 {
                        current_state.liquidity - (-liquidity_net as u128)
                    } else {
                        current_state.liquidity + (liquidity_net as u128)
                    };
                }
                //Increment the current tick
                current_state.tick = if zero_for_one {
                    step.tick_next.wrapping_sub(1)
                } else {
                    step.tick_next
                }
                //If the current_state sqrt price is not equal to the step sqrt price, then we are not on the same tick.
                //Update the current_state.tick to the tick at the current_state.sqrt_price_x_96
            } else if current_state.sqrt_price_x_96 != step.sqrt_price_start_x_96 {
                current_state.tick = uniswap_v3_math::tick_math::get_tick_at_sqrt_ratio(
                    current_state.sqrt_price_x_96,
                )?;
            }
        }

        //Update the pool state
        self.liquidity = current_state.liquidity;
        self.sqrt_price = current_state.sqrt_price_x_96;
        self.tick = current_state.tick;
        self.liquidity_net = liquidity_net;

        Ok((-current_state.amount_calculated).into_raw())
    }

    pub async fn simulate_swap_with_cache<M: Middleware>(
        &self,
        token_in: H160,
        amount_in: U256,
        num_ticks: u16,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        if amount_in.is_zero() {
            return Ok(U256::zero());
        }

        let zero_for_one = token_in == self.token_a;

        //TODO: make this a queue instead of vec and then an iterator FIXME::
        let (mut tick_data, block_number) =
            batch_requests::uniswap_v3::get_uniswap_v3_tick_data_batch_request(
                self,
                self.tick,
                zero_for_one,
                num_ticks,
                None,
                middleware.clone(),
            )
            .await?;

        let mut tick_data_iter = tick_data.iter();

        //Set sqrt_price_limit_x_96 to the max or min sqrt price in the pool depending on zero_for_one
        let sqrt_price_limit_x_96 = if zero_for_one {
            MIN_SQRT_RATIO + 1
        } else {
            MAX_SQRT_RATIO - 1
        };

        //Initialize a mutable state state struct to hold the dynamic simulated state of the pool
        let mut current_state = CurrentState {
            sqrt_price_x_96: self.sqrt_price, //Active price on the pool
            amount_calculated: I256::zero(),  //Amount of token_out that has been calculated
            amount_specified_remaining: I256::from_raw(amount_in), //Amount of token_in that has not been swapped
            tick: self.tick,                                       //Current i24 tick of the pool
            liquidity: self.liquidity, //Current available liquidity in the tick range
        };

        while current_state.amount_specified_remaining != I256::zero()
            && current_state.sqrt_price_x_96 != sqrt_price_limit_x_96
        {
            //Initialize a new step struct to hold the dynamic state of the pool at each step
            let mut step = StepComputations {
                sqrt_price_start_x_96: current_state.sqrt_price_x_96, //Set the sqrt_price_start_x_96 to the current sqrt_price_x_96
                ..Default::default()
            };

            let next_tick_data = if let Some(tick_data) = tick_data_iter.next() {
                tick_data
            } else {
                (tick_data, _) =
                    batch_requests::uniswap_v3::get_uniswap_v3_tick_data_batch_request(
                        self,
                        current_state.tick,
                        zero_for_one,
                        num_ticks,
                        Some(block_number),
                        middleware.clone(),
                    )
                    .await?;

                tick_data_iter = tick_data.iter();

                if let Some(tick_data) = tick_data_iter.next() {
                    tick_data
                } else {
                    //This should never happen, but if it does, we should return an error because something is wrong
                    return Err(CFMMError::NoInitializedTicks);
                }
            };

            step.tick_next = next_tick_data.tick;

            // ensure that we do not overshoot the min/max tick, as the tick bitmap is not aware of these bounds
            //Note: this could be removed as we are clamping in the batch contract
            step.tick_next = step.tick_next.clamp(MIN_TICK, MAX_TICK);

            //Get the next sqrt price from the input amount
            step.sqrt_price_next_x96 =
                uniswap_v3_math::tick_math::get_sqrt_ratio_at_tick(step.tick_next)?;

            //Target spot price
            let swap_target_sqrt_ratio = if zero_for_one {
                if step.sqrt_price_next_x96 < sqrt_price_limit_x_96 {
                    sqrt_price_limit_x_96
                } else {
                    step.sqrt_price_next_x96
                }
            } else if step.sqrt_price_next_x96 > sqrt_price_limit_x_96 {
                sqrt_price_limit_x_96
            } else {
                step.sqrt_price_next_x96
            };

            //Compute swap step and update the current state
            (
                current_state.sqrt_price_x_96,
                step.amount_in,
                step.amount_out,
                step.fee_amount,
            ) = uniswap_v3_math::swap_math::compute_swap_step(
                current_state.sqrt_price_x_96,
                swap_target_sqrt_ratio,
                current_state.liquidity,
                current_state.amount_specified_remaining,
                self.fee,
            )?;

            //Decrement the amount remaining to be swapped and amount received from the step
            current_state.amount_specified_remaining = current_state
                .amount_specified_remaining
                .overflowing_sub(I256::from_raw(
                    step.amount_in.overflowing_add(step.fee_amount).0,
                ))
                .0;

            current_state.amount_calculated -= I256::from_raw(step.amount_out);

            //If the price moved all the way to the next price, recompute the liquidity change for the next iteration
            if current_state.sqrt_price_x_96 == step.sqrt_price_next_x96 {
                if next_tick_data.initialized {
                    let mut liquidity_net = next_tick_data.liquidity_net;

                    // we are on a tick boundary, and the next tick is initialized, so we must charge a protocol fee
                    if zero_for_one {
                        liquidity_net = -liquidity_net;
                    }

                    current_state.liquidity = if liquidity_net < 0 {
                        current_state.liquidity - (-liquidity_net as u128)
                    } else {
                        current_state.liquidity + (liquidity_net as u128)
                    };
                }
                //Increment the current tick
                current_state.tick = if zero_for_one {
                    step.tick_next.wrapping_sub(1)
                } else {
                    step.tick_next
                }
                //If the current_state sqrt price is not equal to the step sqrt price, then we are not on the same tick.
                //Update the current_state.tick to the tick at the current_state.sqrt_price_x_96
            } else if current_state.sqrt_price_x_96 != step.sqrt_price_start_x_96 {
                current_state.tick = uniswap_v3_math::tick_math::get_tick_at_sqrt_ratio(
                    current_state.sqrt_price_x_96,
                )?;
            }
        }

        Ok((-current_state.amount_calculated).into_raw())
    }

    pub async fn simulate_swap<M: Middleware>(
        &self,
        token_in: H160,
        amount_in: U256,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        self.simulate_swap_with_cache(token_in, amount_in, 150, middleware)
            .await
    }

    pub async fn get_word<M: Middleware>(
        &self,
        word_pos: i16,
        block_number: Option<U64>,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        if block_number.is_some() {
            //TODO: in the future, create a batch call to get this and liquidity net within the same call

            Ok(abi::IUniswapV3Pool::new(self.address, middleware.clone())
                .tick_bitmap(word_pos)
                .block(block_number.unwrap())
                .call()
                .await?)
        } else {
            //TODO: in the future, create a batch call to get this and liquidity net within the same call
            Ok(abi::IUniswapV3Pool::new(self.address, middleware.clone())
                .tick_bitmap(word_pos)
                .call()
                .await?)
        }
    }

    pub fn calculate_compressed(&self, tick: i32) -> i32 {
        if tick < 0 && tick % self.tick_spacing != 0 {
            (tick / self.tick_spacing) - 1
        } else {
            tick / self.tick_spacing
        }
    }

    pub fn calculate_word_pos_bit_pos(&self, compressed: i32) -> (i16, u8) {
        uniswap_v3_math::tick_bitmap::position(compressed)
    }

    pub async fn simulate_swap_mut<M: Middleware>(
        &mut self,
        token_in: H160,
        amount_in: U256,
        middleware: Arc<M>,
    ) -> Result<U256, CFMMError<M>> {
        self.simulate_swap_mut_with_cache(token_in, amount_in, 150, middleware)
            .await
    }

    pub fn swap_calldata(
        &self,
        recipient: H160,
        zero_for_one: bool,
        amount_specified: I256,
        sqrt_price_limit_x_96: U256,
        calldata: Vec<u8>,
    ) -> Bytes {
        let input_tokens = vec![
            Token::Address(recipient),
            Token::Bool(zero_for_one),
            Token::Int(amount_specified.into_raw()),
            Token::Uint(sqrt_price_limit_x_96),
            Token::Bytes(calldata),
        ];

        abi::IUNISWAPV3POOL_ABI
            .function("swap")
            .unwrap()
            .encode_input(&input_tokens)
            .expect("Could not encode swap calldata")
    }
}

pub struct CurrentState {
    amount_specified_remaining: I256,
    amount_calculated: I256,
    sqrt_price_x_96: U256,
    tick: i32,
    liquidity: u128,
}

#[derive(Default)]
pub struct StepComputations {
    pub sqrt_price_start_x_96: U256,
    pub tick_next: i32,
    pub initialized: bool,
    pub sqrt_price_next_x96: U256,
    pub amount_in: U256,
    pub amount_out: U256,
    pub fee_amount: U256,
}

const MIN_TICK: i32 = -887272;
const MAX_TICK: i32 = 887272;

pub struct Tick {
    pub liquidity_gross: u128,
    pub liquidity_net: i128,
    pub fee_growth_outside_0_x_128: U256,
    pub fee_growth_outside_1_x_128: U256,
    pub tick_cumulative_outside: U256,
    pub seconds_per_liquidity_outside_x_128: U256,
    pub seconds_outside: u32,
    pub initialized: bool,
}

mod test {
    #[allow(unused)]
    use crate::abi::IUniswapV3Pool;

    #[allow(unused)]
    use super::UniswapV3Pool;
    #[allow(unused)]
    use ethers::providers::Middleware;

    #[allow(unused)]
    use ethers::{
        prelude::abigen,
        providers::{Http, Provider},
        types::{H160, U256},
    };
    #[allow(unused)]
    use std::error::Error;
    #[allow(unused)]
    use std::{str::FromStr, sync::Arc};

    abigen!(
        IQuoter,
    r#"[
        function quoteExactInputSingle(address tokenIn, address tokenOut,uint24 fee, uint256 amountIn, uint160 sqrtPriceLimitX96) external returns (uint256 amountOut)
    ]"#;);

    #[tokio::test]
    async fn test_simulate_swap_0() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let pool = UniswapV3Pool::new_from_address(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        )
        .await
        .unwrap();

        let quoter = IQuoter::new(
            H160::from_str("0xb27308f9f90d607463bb33ea1bebb41c27ce5ab6").unwrap(),
            middleware.clone(),
        );

        let amount_in = U256::from_dec_str("100000000").unwrap(); // 100 USDC

        let current_block = middleware.get_block_number().await.unwrap();
        let amount_out = pool
            .simulate_swap(pool.token_a, amount_in, middleware.clone())
            .await
            .unwrap();

        let expected_amount_out = quoter
            .quote_exact_input_single(
                pool.token_a,
                pool.token_b,
                pool.fee,
                amount_in,
                U256::zero(),
            )
            .block(current_block)
            .call()
            .await
            .unwrap();

        assert_eq!(amount_out, expected_amount_out);
    }

    #[tokio::test]
    async fn test_simulate_swap_1() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let pool = UniswapV3Pool::new_from_address(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        )
        .await
        .unwrap();

        let quoter = IQuoter::new(
            H160::from_str("0xb27308f9f90d607463bb33ea1bebb41c27ce5ab6").unwrap(),
            middleware.clone(),
        );

        let amount_in_1 = U256::from_dec_str("10000000000").unwrap(); // 10_000 USDC

        let current_block = middleware.get_block_number().await.unwrap();
        let amount_out_1 = pool
            .simulate_swap(pool.token_a, amount_in_1, middleware.clone())
            .await
            .unwrap();

        let expected_amount_out_1 = quoter
            .quote_exact_input_single(
                pool.token_a,
                pool.token_b,
                pool.fee,
                amount_in_1,
                U256::zero(),
            )
            .block(current_block)
            .call()
            .await
            .unwrap();

        assert_eq!(amount_out_1, expected_amount_out_1);
    }

    #[tokio::test]
    async fn test_simulate_swap_2() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let pool = UniswapV3Pool::new_from_address(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        )
        .await
        .unwrap();

        let quoter = IQuoter::new(
            H160::from_str("0xb27308f9f90d607463bb33ea1bebb41c27ce5ab6").unwrap(),
            middleware.clone(),
        );

        let amount_in_2 = U256::from_dec_str("10000000000000").unwrap(); // 10_000_000 USDC

        let current_block = middleware.get_block_number().await.unwrap();
        let amount_out_2 = pool
            .simulate_swap(pool.token_a, amount_in_2, middleware.clone())
            .await
            .unwrap();

        let expected_amount_out_2 = quoter
            .quote_exact_input_single(
                pool.token_a,
                pool.token_b,
                pool.fee,
                amount_in_2,
                U256::zero(),
            )
            .block(current_block)
            .call()
            .await
            .unwrap();

        assert_eq!(amount_out_2, expected_amount_out_2);
    }

    #[tokio::test]
    async fn test_simulate_swap_3() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let pool = UniswapV3Pool::new_from_address(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        )
        .await
        .unwrap();

        let quoter = IQuoter::new(
            H160::from_str("0xb27308f9f90d607463bb33ea1bebb41c27ce5ab6").unwrap(),
            middleware.clone(),
        );

        let amount_in_3 = U256::from_dec_str("100000000000000").unwrap(); // 100_000_000 USDC

        dbg!(pool.tick);
        dbg!(pool.tick_spacing);

        let current_block = middleware.get_block_number().await.unwrap();
        let amount_out_3 = pool
            .simulate_swap(pool.token_a, amount_in_3, middleware.clone())
            .await
            .unwrap();

        let expected_amount_out_3 = quoter
            .quote_exact_input_single(
                pool.token_a,
                pool.token_b,
                pool.fee,
                amount_in_3,
                U256::zero(),
            )
            .block(current_block)
            .call()
            .await
            .unwrap();

        assert_eq!(amount_out_3, expected_amount_out_3);
    }

    #[tokio::test]
    async fn test_get_new_from_address() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let pool = UniswapV3Pool::new_from_address(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        )
        .await
        .unwrap();

        assert_eq!(
            pool.address,
            H160::from_str("0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640").unwrap()
        );
        assert_eq!(
            pool.token_a,
            H160::from_str("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").unwrap()
        );
        assert_eq!(pool.token_a_decimals, 6);
        assert_eq!(
            pool.token_b,
            H160::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").unwrap()
        );
        assert_eq!(pool.token_b_decimals, 18);
        assert_eq!(pool.fee, 500);
        assert!(pool.tick != 0);
        assert_eq!(pool.tick_spacing, 10);
    }

    #[tokio::test]
    async fn test_get_pool_data() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let mut pool = UniswapV3Pool {
            address: H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            ..Default::default()
        };

        pool.get_pool_data(middleware).await.unwrap();

        assert_eq!(
            pool.address,
            H160::from_str("0x88e6a0c2ddd26feeb64f039a2c41296fcb3f5640").unwrap()
        );
        assert_eq!(
            pool.token_a,
            H160::from_str("0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48").unwrap()
        );
        assert_eq!(pool.token_a_decimals, 6);
        assert_eq!(
            pool.token_b,
            H160::from_str("0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2").unwrap()
        );
        assert_eq!(pool.token_b_decimals, 18);
        assert_eq!(pool.fee, 500);
        assert!(pool.tick != 0);
        assert_eq!(pool.tick_spacing, 10);
    }

    #[tokio::test]
    async fn test_sync_pool() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let mut pool = UniswapV3Pool {
            address: H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            ..Default::default()
        };

        pool.sync_pool(middleware).await.unwrap();

        //TODO: need to assert values
    }

    #[tokio::test]
    async fn test_calculate_virtual_reserves() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let mut pool = UniswapV3Pool {
            address: H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            ..Default::default()
        };

        pool.get_pool_data(middleware.clone()).await.unwrap();

        let pool_at_block = IUniswapV3Pool::new(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        );

        let sqrt_price = pool_at_block
            .slot_0()
            .block(16515398)
            .call()
            .await
            .unwrap()
            .0;
        let liquidity = pool_at_block
            .liquidity()
            .block(16515398)
            .call()
            .await
            .unwrap();

        pool.sqrt_price = sqrt_price;
        pool.liquidity = liquidity;

        dbg!(pool.sqrt_price);
        dbg!(pool.liquidity);

        let (r_0, r_1) = pool
            .calculate_virtual_reserves()
            .expect("Could not calculate virtual reserves");

        assert_eq!(1067543429906214084651, r_0);
        assert_eq!(649198362624067396, r_1);
    }

    #[tokio::test]
    async fn test_calculate_price() {
        let rpc_endpoint = std::env::var("ETHEREUM_MAINNET_ENDPOINT")
            .expect("Could not get ETHEREUM_MAINNET_ENDPOINT");
        let middleware = Arc::new(Provider::<Http>::try_from(rpc_endpoint).unwrap());

        let mut pool = UniswapV3Pool {
            address: H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            ..Default::default()
        };

        pool.get_pool_data(middleware.clone()).await.unwrap();

        let block_pool = IUniswapV3Pool::new(
            H160::from_str("0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640").unwrap(),
            middleware.clone(),
        );

        let sqrt_price = block_pool.slot_0().block(16515398).call().await.unwrap().0;
        pool.sqrt_price = sqrt_price;

        let float_price_a = pool.calculate_price(pool.token_a);

        let float_price_b = pool.calculate_price(pool.token_b);

        dbg!(pool);

        println!("Price A: {float_price_a}");
        println!("Price B: {float_price_b}");
    }
}
