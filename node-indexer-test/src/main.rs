use std::alloc::System;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use ton_abi::Event;

use node_indexer::{Config, NodeClient};

#[global_allocator]
static GLOBAL: System = System;

const DEX_ABI: &str = r#"
    {
	"ABI version": 2,
	"header": ["pubkey", "time", "expire"],
	"functions": [
		{
			"name": "constructor",
			"inputs": [
			],
			"outputs": [
			]
		},
		{
			"name": "resetGas",
			"inputs": [
				{"name":"receiver","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "getRoot",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"dex_root","type":"address"}
			]
		},
		{
			"name": "getTokenRoots",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"left","type":"address"},
				{"name":"right","type":"address"},
				{"name":"lp","type":"address"}
			]
		},
		{
			"name": "getTokenWallets",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"left","type":"address"},
				{"name":"right","type":"address"},
				{"name":"lp","type":"address"}
			]
		},
		{
			"name": "getVersion",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"version","type":"uint32"}
			]
		},
		{
			"name": "getVault",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"dex_vault","type":"address"}
			]
		},
		{
			"name": "getVaultWallets",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"left","type":"address"},
				{"name":"right","type":"address"}
			]
		},
		{
			"name": "setFeeParams",
			"inputs": [
				{"name":"numerator","type":"uint16"},
				{"name":"denominator","type":"uint16"}
			],
			"outputs": [
			]
		},
		{
			"name": "getFeeParams",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"numerator","type":"uint16"},
				{"name":"denominator","type":"uint16"}
			]
		},
		{
			"name": "isActive",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"name":"value0","type":"bool"}
			]
		},
		{
			"name": "getBalances",
			"inputs": [
				{"name":"_answer_id","type":"uint32"}
			],
			"outputs": [
				{"components":[{"name":"lp_supply","type":"uint128"},{"name":"left_balance","type":"uint128"},{"name":"right_balance","type":"uint128"}],"name":"value0","type":"tuple"}
			]
		},
		{
			"name": "buildExchangePayload",
			"inputs": [
				{"name":"id","type":"uint64"},
				{"name":"deploy_wallet_grams","type":"uint128"},
				{"name":"expected_amount","type":"uint128"}
			],
			"outputs": [
				{"name":"value0","type":"cell"}
			]
		},
		{
			"name": "buildDepositLiquidityPayload",
			"inputs": [
				{"name":"id","type":"uint64"},
				{"name":"deploy_wallet_grams","type":"uint128"}
			],
			"outputs": [
				{"name":"value0","type":"cell"}
			]
		},
		{
			"name": "buildWithdrawLiquidityPayload",
			"inputs": [
				{"name":"id","type":"uint64"},
				{"name":"deploy_wallet_grams","type":"uint128"}
			],
			"outputs": [
				{"name":"value0","type":"cell"}
			]
		},
		{
			"name": "tokensReceivedCallback",
			"inputs": [
				{"name":"token_wallet","type":"address"},
				{"name":"token_root","type":"address"},
				{"name":"tokens_amount","type":"uint128"},
				{"name":"sender_public_key","type":"uint256"},
				{"name":"sender_address","type":"address"},
				{"name":"sender_wallet","type":"address"},
				{"name":"original_gas_to","type":"address"},
				{"name":"value7","type":"uint128"},
				{"name":"payload","type":"cell"}
			],
			"outputs": [
			]
		},
		{
			"name": "expectedDepositLiquidity",
			"inputs": [
				{"name":"_answer_id","type":"uint32"},
				{"name":"left_amount","type":"uint128"},
				{"name":"right_amount","type":"uint128"},
				{"name":"auto_change","type":"bool"}
			],
			"outputs": [
				{"components":[{"name":"step_1_left_deposit","type":"uint128"},{"name":"step_1_right_deposit","type":"uint128"},{"name":"step_1_lp_reward","type":"uint128"},{"name":"step_2_left_to_right","type":"bool"},{"name":"step_2_right_to_left","type":"bool"},{"name":"step_2_spent","type":"uint128"},{"name":"step_2_fee","type":"uint128"},{"name":"step_2_received","type":"uint128"},{"name":"step_3_left_deposit","type":"uint128"},{"name":"step_3_right_deposit","type":"uint128"},{"name":"step_3_lp_reward","type":"uint128"}],"name":"value0","type":"tuple"}
			]
		},
		{
			"name": "depositLiquidity",
			"inputs": [
				{"name":"call_id","type":"uint64"},
				{"name":"left_amount","type":"uint128"},
				{"name":"right_amount","type":"uint128"},
				{"name":"expected_lp_root","type":"address"},
				{"name":"auto_change","type":"bool"},
				{"name":"account_owner","type":"address"},
				{"name":"value6","type":"uint32"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "expectedWithdrawLiquidity",
			"inputs": [
				{"name":"_answer_id","type":"uint32"},
				{"name":"lp_amount","type":"uint128"}
			],
			"outputs": [
				{"name":"expected_left_amount","type":"uint128"},
				{"name":"expected_right_amount","type":"uint128"}
			]
		},
		{
			"name": "withdrawLiquidity",
			"inputs": [
				{"name":"call_id","type":"uint64"},
				{"name":"lp_amount","type":"uint128"},
				{"name":"expected_lp_root","type":"address"},
				{"name":"account_owner","type":"address"},
				{"name":"value4","type":"uint32"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "expectedExchange",
			"inputs": [
				{"name":"_answer_id","type":"uint32"},
				{"name":"amount","type":"uint128"},
				{"name":"spent_token_root","type":"address"}
			],
			"outputs": [
				{"name":"expected_amount","type":"uint128"},
				{"name":"expected_fee","type":"uint128"}
			]
		},
		{
			"name": "expectedSpendAmount",
			"inputs": [
				{"name":"_answer_id","type":"uint32"},
				{"name":"receive_amount","type":"uint128"},
				{"name":"receive_token_root","type":"address"}
			],
			"outputs": [
				{"name":"expected_amount","type":"uint128"},
				{"name":"expected_fee","type":"uint128"}
			]
		},
		{
			"name": "exchange",
			"inputs": [
				{"name":"call_id","type":"uint64"},
				{"name":"spent_amount","type":"uint128"},
				{"name":"spent_token_root","type":"address"},
				{"name":"receive_token_root","type":"address"},
				{"name":"expected_amount","type":"uint128"},
				{"name":"account_owner","type":"address"},
				{"name":"value6","type":"uint32"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "checkPair",
			"inputs": [
				{"name":"call_id","type":"uint64"},
				{"name":"account_owner","type":"address"},
				{"name":"value2","type":"uint32"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "upgrade",
			"inputs": [
				{"name":"code","type":"cell"},
				{"name":"new_version","type":"uint32"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "afterInitialize",
			"inputs": [
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "liquidityTokenRootDeployed",
			"inputs": [
				{"name":"lp_root_","type":"address"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "liquidityTokenRootNotDeployed",
			"inputs": [
				{"name":"value0","type":"address"},
				{"name":"send_gas_to","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "expectedWalletAddressCallback",
			"inputs": [
				{"name":"wallet","type":"address"},
				{"name":"wallet_public_key","type":"uint256"},
				{"name":"owner_address","type":"address"}
			],
			"outputs": [
			]
		},
		{
			"name": "platform_code",
			"inputs": [
			],
			"outputs": [
				{"name":"platform_code","type":"cell"}
			]
		},
		{
			"name": "lp_wallet",
			"inputs": [
			],
			"outputs": [
				{"name":"lp_wallet","type":"address"}
			]
		},
		{
			"name": "left_wallet",
			"inputs": [
			],
			"outputs": [
				{"name":"left_wallet","type":"address"}
			]
		},
		{
			"name": "right_wallet",
			"inputs": [
			],
			"outputs": [
				{"name":"right_wallet","type":"address"}
			]
		},
		{
			"name": "vault_left_wallet",
			"inputs": [
			],
			"outputs": [
				{"name":"vault_left_wallet","type":"address"}
			]
		},
		{
			"name": "vault_right_wallet",
			"inputs": [
			],
			"outputs": [
				{"name":"vault_right_wallet","type":"address"}
			]
		},
		{
			"name": "lp_root",
			"inputs": [
			],
			"outputs": [
				{"name":"lp_root","type":"address"}
			]
		},
		{
			"name": "lp_supply",
			"inputs": [
			],
			"outputs": [
				{"name":"lp_supply","type":"uint128"}
			]
		},
		{
			"name": "left_balance",
			"inputs": [
			],
			"outputs": [
				{"name":"left_balance","type":"uint128"}
			]
		},
		{
			"name": "right_balance",
			"inputs": [
			],
			"outputs": [
				{"name":"right_balance","type":"uint128"}
			]
		}
	],
	"data": [
	],
	"events": [
		{
			"name": "PairCodeUpgraded",
			"inputs": [
				{"name":"version","type":"uint32"}
			],
			"outputs": [
			]
		},
		{
			"name": "FeesParamsUpdated",
			"inputs": [
				{"name":"numerator","type":"uint16"},
				{"name":"denominator","type":"uint16"}
			],
			"outputs": [
			]
		},
		{
			"name": "DepositLiquidity",
			"inputs": [
				{"name":"left","type":"uint128"},
				{"name":"right","type":"uint128"},
				{"name":"lp","type":"uint128"}
			],
			"outputs": [
			]
		},
		{
			"name": "WithdrawLiquidity",
			"inputs": [
				{"name":"lp","type":"uint128"},
				{"name":"left","type":"uint128"},
				{"name":"right","type":"uint128"}
			],
			"outputs": [
			]
		},
		{
			"name": "ExchangeLeftToRight",
			"inputs": [
				{"name":"left","type":"uint128"},
				{"name":"fee","type":"uint128"},
				{"name":"right","type":"uint128"}
			],
			"outputs": [
			]
		},
		{
			"name": "ExchangeRightToLeft",
			"inputs": [
				{"name":"right","type":"uint128"},
				{"name":"fee","type":"uint128"},
				{"name":"left","type":"uint128"}
			],
			"outputs": [
			]
		}
	]
}
    "#;

fn prep_event() -> [Event; 4] {
    let contract = ton_abi::Contract::load(std::io::Cursor::new(DEX_ABI)).unwrap();
    let mem = contract.events();
    let id1 = mem.get("DepositLiquidity").unwrap();
    let parse_ev1 = contract.event_by_id(id1.id).unwrap();
    let id2 = mem.get("WithdrawLiquidity").unwrap();
    let parse_ev2 = contract.event_by_id(id2.id).unwrap();
    let id3 = mem.get("ExchangeLeftToRight").unwrap();
    let parse_ev3 = contract.event_by_id(id3.id).unwrap();
    let id4 = mem.get("ExchangeRightToLeft").unwrap();
    let parse_ev4 = contract.event_by_id(id4.id).unwrap();
    [
        parse_ev1.clone(),
        parse_ev2.clone(),
        parse_ev3.clone(),
        parse_ev4.clone(),
    ]
}

fn main() {
    env_logger::init();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name_fn(|| {
            static ATOMIC_ID: AtomicUsize = AtomicUsize::new(0);
            let id = ATOMIC_ID.fetch_add(1, Ordering::SeqCst);
            format!("my-pool-{}", id)
        })
        .build()
        .unwrap();

    log::info!("Started");
    rt.block_on(async move {
        let mut config = Config::default();
        let pool_size = 2;
        config.pool_size = pool_size;

        let node = Arc::new(NodeClient::new(config).await.unwrap());
        log::info!("here");

        let (tx, rx) = futures::channel::mpsc::channel((pool_size * 4 * 16 * 4) as usize);
        let (tx_block, mut rx_block) = futures::channel::mpsc::unbounded();
        node.spawn_indexer(
            Some(ton_api::ton::ton_node::blockid::BlockId {
                workchain: -1,
                shard: u64::from_str_radix("8000000000000000", 16).unwrap() as i64,
                seqno: 8202177,
            }),
            tx,
            tx_block,
        )
        .await
        .unwrap();
        // let abi = prep_event();
        // //
        // let all = node
        //     .get_all_transactions(
        //         MsgAddressInt::from_str(
        //             "0:943bad2e74894aa28ae8ddbe673be09a0f3818fd170d12b4ea8ef1ea8051e940",
        //         )
        //         .unwrap(),
        //     )
        //     .await
        //     .unwrap();
        //
        // for tx in all {
        //     let name = tx.hash.as_slice();
        //     let name = "data/".to_string() + &hex::encode(name);
        //     let mut file = std::fs::OpenOptions::new()
        //         .create(true)
        //         .write(true)
        //         .open(&name)
        //         .unwrap();
        //     file.write_all(&serialize_toc(&tx.data.serialize().unwrap()).unwrap())
        //         .unwrap()
        // }
        // return;
        // let parsed: Result<Vec<_>> = all
        //     .into_iter()
        //     .map(|x| {
        //         {
        //             ExtractInput {
        //                 transaction: &x.data,
        //                 what_to_extract: &abi,
        //             }
        //         }
        //         .process()
        //     })
        //     .collect();
        // dbg!(parsed
        //     .unwrap()
        //     .into_iter()
        //     .for_each(|x| println!("{:?}", x.output)));
        tokio::spawn(async move {
            while let Some(a) = rx_block.next().await {
                drop(a);
            }
        });
        let evs = prep_event();
        let mut rx = rx.enumerate();
        // tokio::time::sleep(Duration::from_secs(30)).await;
        // println!("Long sleep");

        let mut seqnos = HashMap::<_, Vec<_>>::new();

        while let Some((a, b)) = rx.next().await {
            let res = indexer_lib::extract_from_block(&b, &evs).unwrap().len();
            if res != 0 {
                println!("{}", res);
            }
            let info = b.info.read_struct().unwrap();
            seqnos.entry(*info.shard()).or_default().push(info.seq_no());
            if a > 100000 {
                break;
            }
            println!("{}", a);
        }

        // let mut pre = &block_seqnos[0];
        // for a in &block_seqnos {
        //     if (pre - a) > 1 {
        //         dbg!(block_seqnos)
        //     }
        // }
        for (k, mut v) in seqnos {
            v.sort_unstable();

            let start = v.first().unwrap();
            let end = v.last().unwrap();
            let gen = *start..=*end;
            let mut flag = false;
            for (seq, expected) in v.clone().iter().zip(gen) {
                if *seq != expected {
                    flag = true;
                }
            }
            if flag {
                println!("{:?}", k);
                dbg!(v);
            }
        }
    })
}
