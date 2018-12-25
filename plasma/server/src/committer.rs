use std::sync::mpsc::{channel, Sender, Receiver};
use plasma::eth_client::{ETHClient, TxMeta, TEST_PLASMA_ALWAYS_VERIFY};
use plasma::models::{AccountMap, params};
use super::storage::StorageConnection;
use super::models::*;
use web3::types::{U256, H256};
use super::config;

fn sorted_and_padded_for_deposits(accounts_updated: AccountMap) -> [u64; config::DEPOSIT_BATCH_SIZE] {
    let mut tmp = [params::SPECIAL_ACCOUNT_DEPOSIT as u64; config::DEPOSIT_BATCH_SIZE];
    let mut acc: Vec<u64> = accounts_updated.keys()
        .map(|&k| k as u64)
        .collect();
    acc.sort();

    for (i, a) in acc.into_iter().enumerate() {
        tmp[i] = a;
    }

    tmp
}

fn keys_sorted(accounts_updated: AccountMap) -> Vec<u64> {
    let mut acc: Vec<u64> = accounts_updated.keys()
        .map(|&k| k as u64)
        .collect();
    acc.sort();
    acc
} 

pub fn start_eth_sender() -> Sender<(EthOperation, TxMeta)> {
    let (tx_for_eth, rx_for_eth) = channel();
    let mut eth_client = ETHClient::new(TEST_PLASMA_ALWAYS_VERIFY);
    let current_nonce = eth_client.get_nonce(&eth_client.default_account()).unwrap();
    let storage = StorageConnection::new();

    // TODO: this is for test only, introduce a production switch (as we can not rely on debug/release mode because performance is required for circuits)
    let addr = std::env::var("SENDER_ACCOUNT").unwrap_or("e5d0efb4756bd5cdd4b5140d3d2e08ca7e6cf644".to_string());
    storage.reset_op_config(&addr, current_nonce.as_u32());

    // execute pending transactions
    let ops = storage.load_pendings_ops(current_nonce.as_u32()).expect("db must be functional");
    for pending_op in ops {
        let op = serde_json::from_value(pending_op.data).unwrap();
        tx_for_eth.send((op, TxMeta{
            addr:   pending_op.addr, 
            nonce:  pending_op.nonce as u32,
        })).expect("queue must work");
    }

    std::thread::Builder::new().name("eth_sender".to_string()).spawn(move || {
        for (op, meta) in rx_for_eth {
            println!("EthOperation requested: {:?}", &op);
            let tx = match op {
                EthOperation::Commit{block_number, new_root, block_data, accounts_updated} => {
                    match block_data {
                        EthBlockData::Transfer{total_fees, public_data} =>
                            eth_client.call("commitTransferBlock", meta, 
                                (block_number as u64, total_fees, public_data, new_root)),
                        EthBlockData::Deposit{batch_number} =>
                            // function commitDepositBlock(
                            //     uint256 batchNumber,
                            //     uint24[DEPOSIT_BATCH_SIZE] memory accoundIDs,
                            //     uint32 blockNumber, 
                            //     bytes32 newRoot
                            // ) 
                            eth_client.call("commitDepositBlock", meta,
                                (U256::from(batch_number), sorted_and_padded_for_deposits(accounts_updated), block_number as u64, new_root)),
                        EthBlockData::Exit{batch_number} =>
                            // function commitExitBlock(
                            //     uint256 batchNumber,
                            //     uint32 blockNumber, 
                            //     bytes32 publicDataCommitment,
                            //     bytes32 newRoot
                            // ) 

                            // TODO: fix zero H256, it should contait valid precommitment for an exit block
                            eth_client.call("commitExitBlock", meta,
                                (U256::from(batch_number), block_number as u64, H256::zero(), new_root)),
                    }
                },
                EthOperation::Verify{block_number, proof, block_data, accounts_updated} => {
                    match block_data {
                        EthBlockData::Transfer{total_fees: _, public_data: _} =>
                            eth_client.call("verifyTransferBlock", meta,
                                (block_number as u64, proof)),
                        EthBlockData::Deposit{batch_number} =>
                            // function verifyDepositBlock(
                            //     uint256 batchNumber, 
                            //     uint24[DEPOSIT_BATCH_SIZE] memory accoundIDs, 
                            //     uint32 blockNumber, 
                            //     uint256[8] memory proof
                            // ) 
                            eth_client.call("verifyDepositBlock", meta,
                                (U256::from(batch_number), sorted_and_padded_for_deposits(accounts_updated), block_number as u64, proof)),
                        EthBlockData::Exit{batch_number} =>
                            eth_client.call("verifyExitBlock", meta,
                                (block_number as u64, batch_number as u64, keys_sorted(accounts_updated))),
                    }
                },
                _ => unimplemented!(),
            };
            // TODO: process tx sending failure
            if tx.is_err() {
                println!("Error sending tx {}", tx.err().unwrap());

            } else {
                println!("Commitment tx hash = {}", tx.unwrap());
            }

        }
    });
    tx_for_eth
}

pub fn load_pendings_ops(eth_client: &ETHClient, tx_for_eth: &Sender<(EthOperation, TxMeta)>) {
    
    let storage = StorageConnection::new();

    // execute pending transactions
    let current_nonce = eth_client.get_nonce(&eth_client.default_account()).unwrap();
    let ops = storage.load_pendings_ops(current_nonce.as_u32()).expect("db must be functional");
    for pending_op in ops {
        let op = serde_json::from_value(pending_op.data).unwrap();
        tx_for_eth.send((op, TxMeta{
            addr:   pending_op.addr, 
            nonce:  pending_op.nonce as u32,
        })).expect("queue must work");
    }
}

pub fn run_committer(rx_for_ops: Receiver<EthOperation>, tx_for_eth: Sender<(EthOperation, TxMeta)>) {

    let storage = StorageConnection::new();
    for op in rx_for_ops {
        // persist in storage first
        let committed_op = storage.commit_op(&op).expect("db must be functional");

        // then submit to eth
        tx_for_eth.send((op, TxMeta{
            addr:   committed_op.addr, 
            nonce:  committed_op.nonce as u32,
        })).expect("queue must work");
    }
}