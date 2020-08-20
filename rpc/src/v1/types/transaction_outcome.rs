use std::vec::Vec;
use ethereum_types::H256;

#[derive(Debug, Serialize, PartialEq, Eq, Hash, Clone)]
pub struct TransactionOutcome {
	#[serde(rename = "transactionHash")]
	pub hash: H256,
	#[serde(rename = "returnData")]
	pub output: Vec<u8>,
}
