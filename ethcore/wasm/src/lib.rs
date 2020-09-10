// Copyright 2015-2020 Parity Technologies (UK) Ltd.
// This file is part of Open Ethereum.

// Open Ethereum is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Open Ethereum is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Open Ethereum.  If not, see <http://www.gnu.org/licenses/>.

//! Wasm Interpreter

extern crate byteorder;
extern crate ethereum_types;
#[macro_use] extern crate log;
extern crate libc;
extern crate parity_wasm;
extern crate vm;
extern crate pwasm_utils as wasm_utils;
extern crate wasmi;
extern crate evmc_client;

#[cfg(test)]
extern crate env_logger;

mod env;
mod panic_payload;
mod parser;
mod runtime;

#[cfg(test)]
mod tests;


use vm::{GasLeft, ReturnData, ActionParams};
use wasmi::{Error as InterpreterError, Trap};

use runtime::{Runtime, RuntimeContext};

use ethereum_types::U256;
use ethereum_types::H160;

use std::collections::BTreeMap;
use evmc_client::{host::HostContext as HostInterface, load, EvmcVm, EvmcLoaderErrorCode, types::*};

/// Wrapped interpreter error
#[derive(Debug)]
pub enum Error {
	Interpreter(InterpreterError),
	Trap(Trap),
}

impl From<InterpreterError> for Error {
	fn from(e: InterpreterError) -> Self {
		Error::Interpreter(e)
	}
}

impl From<Trap> for Error {
	fn from(e: Trap) -> Self {
		Error::Trap(e)
	}
}

impl From<Error> for vm::Error {
	fn from(e: Error) -> Self {
		match e {
			Error::Interpreter(e) => vm::Error::Wasm(format!("Wasm runtime error: {:?}", e)),
			Error::Trap(e) => vm::Error::Wasm(format!("Wasm contract trap: {:?}", e)),
		}
	}
}

/// Wasm interpreter instance
pub struct WasmInterpreter {
	params: ActionParams,
}

impl WasmInterpreter {
	pub fn new(params: ActionParams) -> Self {
		WasmInterpreter { params }
	}
}

impl From<runtime::Error> for vm::Error {
	fn from(e: runtime::Error) -> Self {
		vm::Error::Wasm(format!("Wasm runtime error: {:?}", e))
	}
}

enum ExecutionOutcome {
	Suicide,
	Return,
	NotSpecial,
}

struct HostContext {
    storage: BTreeMap<Bytes32, Bytes32>,
}

impl HostContext {
    fn new() -> HostContext {
        HostContext {
            storage: BTreeMap::new(),
        }
    }
}

impl HostInterface for HostContext {
	fn account_exists(&mut self, _addr: &Address) -> bool {
		println!("Host: account_exists");
		return true;
	}
	fn get_storage(&mut self, _addr: &Address, key: &Bytes32) -> Bytes32 {
		println!("Host: get_storage");
		let value = self.storage.get(key);
		let ret: Bytes32;
		match value {
			Some(value) => ret = value.to_owned(),
			None => ret = [0u8; BYTES32_LENGTH],
		}
		println!("{:?} -> {:?}", hex::encode(key), hex::encode(ret));
		return ret;
	}
	fn set_storage(&mut self, _addr: &Address, key: &Bytes32, value: &Bytes32) -> StorageStatus {
		println!("Host: set_storage");
		println!("{:?} -> {:?}", hex::encode(key), hex::encode(value));
		self.storage.insert(key.to_owned(), value.to_owned());
		return StorageStatus::EVMC_STORAGE_MODIFIED;
	}
	fn get_balance(&mut self, _addr: &Address) -> Bytes32 {
		println!("Host: get_balance");
		return [0u8; BYTES32_LENGTH];
	}
	fn get_code_size(&mut self, _addr: &Address) -> usize {
		println!("Host: get_code_size");
		return 0;
	}
	fn get_code_hash(&mut self, _addr: &Address) -> Bytes32 {
		println!("Host: get_code_hash");
		return [0u8; BYTES32_LENGTH];
	}
	fn copy_code(
		&mut self,
		_addr: &Address,
		_offset: &usize,
		_buffer_data: &*mut u8,
		_buffer_size: &usize,
	) -> usize {
		println!("Host: copy_code");
		return 0;
	}
	fn selfdestruct(&mut self, _addr: &Address, _beneficiary: &Address) {
		println!("Host: selfdestruct");
	}
	fn get_tx_context(&mut self) -> (Bytes32, Address, Address, i64, i64, i64, Bytes32) {
		println!("Host: get_tx_context");
		return (
			[0u8; BYTES32_LENGTH],
			[0u8; ADDRESS_LENGTH],
			[0u8; ADDRESS_LENGTH],
			0,
			0,
			0,
			[0u8; BYTES32_LENGTH],
		);
	}
	fn get_block_hash(&mut self, _number: i64) -> Bytes32 {
		println!("Host: get_block_hash");
		return [0u8; BYTES32_LENGTH];
	}
	fn emit_log(&mut self, _addr: &Address, _topics: &Vec<Bytes32>, _data: &[u8]) {
		println!("Host: emit_log");
	}
	fn call(
		&mut self,
		_kind: CallKind,
		_destination: &Address,
		_sender: &Address,
		_value: &Bytes32,
		_input: &[u8],
		_gas: i64,
		_depth: i32,
		_is_static: bool,
	) -> (Vec<u8>, i64, Address, StatusCode) {
		println!("Host: call");
		return (
			vec![0u8; BYTES32_LENGTH],
			_gas,
			[0u8; ADDRESS_LENGTH],
			StatusCode::EVMC_SUCCESS,
		);
	}
}

impl Drop for HostContext {
    fn drop(&mut self) {
        println!("Dump storage:");
        for (key, value) in &self.storage {
            println!("{:?} -> {:?}", hex::encode(key), hex::encode(value));
        }
    }
}

fn exec(code: &Vec<u8>, input: &Vec<u8>, depth: i32, gas: U256, value: U256, destination: H160, sender: H160) -> (Vec<u8>, i64) {
    let lib_path = "/libssvm-evmc.so";
	let (vm, result) = load(lib_path);
	println!("result {:?}", result);
	println!("Instantiate: {:?}", (vm.get_name(), vm.get_version()));
	
	let mut value_arr = [0u8; 32];
	value.to_little_endian(&mut value_arr);

    let host_context = HostContext::new();
    let (output, gas_left, status_code) = vm.execute(
        Box::new(host_context),
        Revision::EVMC_BYZANTIUM,
        CallKind::EVMC_CALL,
        false,
        depth,
        gas.as_u64() as i64,
        destination.as_fixed_bytes(),
        sender.as_fixed_bytes(),
        &input[..],
        &value_arr,
        &code[..],
        &[0u8; 32],
    );
    println!("Output:  {:?}", hex::encode(output));
    println!("GasLeft: {:?}", gas_left);
    println!("Status:  {:?}", status_code);
    vm.destroy();

    (output.to_vec(), gas_left)
}

impl WasmInterpreter {
	pub fn run(self: Box<WasmInterpreter>, ext: &mut dyn vm::Ext) -> vm::Result<GasLeft> {
		match self.params.code.as_ref() {
			Some(code_ref) => {
				let input = vec![0u8; 0];
				let input_ref: &Vec<u8>;
				match self.params.data.as_ref() {
					Some(data) => {
						input_ref = data;
					},
					None => {
						input_ref = &input;
					}
				}
				let (result, gas_left) = exec(code_ref, input_ref, ext.depth() as i32, self.params.gas, self.params.value.value(), self.params.origin, self.params.sender);
				let len = result.len();
				Ok(GasLeft::NeedsReturn {
					gas_left: ethereum_types::U256::from(gas_left),
					data: ReturnData::new(
						result,
						0,
						len,
					),
					apply_state: true,
				})
			},
			None => Ok(GasLeft::Known(U256::zero()))
		}
		
		// original code
		/*
		let (module, data) = parser::payload(&self.params, ext.schedule().wasm())?;

		let loaded_module = wasmi::Module::from_parity_wasm_module(module).map_err(Error::Interpreter)?;

		let instantiation_resolver = env::ImportResolver::with_limit(16, ext.schedule().wasm());

		let module_instance = wasmi::ModuleInstance::new(
			&loaded_module,
			&wasmi::ImportsBuilder::new().with_resolver("env", &instantiation_resolver)
		).map_err(Error::Interpreter)?;

		let adjusted_gas = self.params.gas * U256::from(ext.schedule().wasm().opcodes_div) /
			U256::from(ext.schedule().wasm().opcodes_mul);

		if adjusted_gas > ::std::u64::MAX.into()
		{
			return Err(vm::Error::Wasm("Wasm interpreter cannot run contracts with gas (wasm adjusted) >= 2^64".to_owned()));
		}

		let initial_memory = instantiation_resolver.memory_size().map_err(Error::Interpreter)?;
		trace!(target: "wasm", "Contract requested {:?} pages of initial memory", initial_memory);

		let (gas_left, result) = {
			let mut runtime = Runtime::with_params(
				ext,
				instantiation_resolver.memory_ref(),
				// cannot overflow, checked above
				adjusted_gas.low_u64(),
				data.to_vec(),
				RuntimeContext {
					address: self.params.address,
					sender: self.params.sender,
					origin: self.params.origin,
					code_address: self.params.code_address,
					code_version: self.params.code_version,
					value: self.params.value.value(),
				},
			);

			// cannot overflow if static_region < 2^16,
			// initial_memory ∈ [0..2^32)
			// total_charge <- static_region * 2^32 * 2^16
			// total_charge ∈ [0..2^64) if static_region ∈ [0..2^16)
			// qed
			assert!(runtime.schedule().wasm().initial_mem < 1 << 16);
			runtime.charge(|s| initial_memory as u64 * s.wasm().initial_mem as u64)?;

			let module_instance = module_instance.run_start(&mut runtime).map_err(Error::Trap)?;

			let invoke_result = module_instance.invoke_export("call", &[], &mut runtime);

			let mut execution_outcome = ExecutionOutcome::NotSpecial;
			if let Err(InterpreterError::Trap(ref trap)) = invoke_result {
				if let wasmi::TrapKind::Host(ref boxed) = *trap.kind() {
					let ref runtime_err = boxed.downcast_ref::<runtime::Error>()
						.expect("Host errors other than runtime::Error never produced; qed");

					match **runtime_err {
						runtime::Error::Suicide => { execution_outcome = ExecutionOutcome::Suicide; },
						runtime::Error::Return => { execution_outcome = ExecutionOutcome::Return; },
						_ => {}
					}
				}
			}

			if let (ExecutionOutcome::NotSpecial, Err(e)) = (execution_outcome, invoke_result) {
				trace!(target: "wasm", "Error executing contract: {:?}", e);
				return Err(vm::Error::from(Error::from(e)));
			}

			(
				runtime.gas_left().expect("Cannot fail since it was not updated since last charge"),
				runtime.into_result(),
			)
		};

		let gas_left =
			U256::from(gas_left) * U256::from(ext.schedule().wasm().opcodes_mul)
				/ U256::from(ext.schedule().wasm().opcodes_div);

		if result.is_empty() {
			trace!(target: "wasm", "Contract execution result is empty.");
			Ok(GasLeft::Known(gas_left))
		} else {
			let len = result.len();
			Ok(GasLeft::NeedsReturn {
				gas_left: gas_left,
				data: ReturnData::new(
					result,
					0,
					len,
				),
				apply_state: true,
			})
		}
		*/
	}
}

impl vm::Exec for WasmInterpreter {
	fn exec(self: Box<WasmInterpreter>, ext: &mut dyn vm::Ext) -> vm::ExecTrapResult<GasLeft> {
		Ok(self.run(ext))
	}
}
