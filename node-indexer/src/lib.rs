use std::convert::TryInto;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bb8::{Pool, PooledConnection};
use futures::{Sink, SinkExt};
use nekoton::abi::TransactionId;
use nekoton::transport::models::{ExistingContract, RawContractState, RawTransaction};
use nekoton::utils::{NoFailure, TrustMe};
use tiny_adnl::{AdnlTcpClient, AdnlTcpClientConfig};
use tokio::sync::mpsc::Sender;
use tokio::sync::{Barrier, OwnedSemaphorePermit, Semaphore};
use ton::ton_node::blockid::BlockId;
use ton_api::ton;
use ton_block::{Deserializable, HashmapAugType, MsgAddressInt, ShardDescr, ShardIdent};

use crate::adnl_pool::AdnlManageConnection;
use crate::errors::{QueryError, QueryResult};
use crate::last_block::LastBlock;

mod adnl_pool;
mod errors;
mod last_block;

#[derive(Debug, Clone)]
pub struct Config {
    pub indexer_interval: Duration,
    pub adnl: AdnlTcpClientConfig,
    pub threshold: Duration,
    pub pool_size: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            indexer_interval: Duration::from_secs(1),
            adnl: default_mainnet_config(),
            threshold: Duration::from_secs(1),
            pool_size: 100,
        }
    }
}

pub fn default_mainnet_config() -> AdnlTcpClientConfig {
    let key =
        hex::decode("b8d4512fee9e9d08ee899fece99faf3bbcb151447bbb175fcc8cbe4719040ab7").unwrap();

    AdnlTcpClientConfig {
        server_address: SocketAddrV4::new(Ipv4Addr::new(54, 158, 97, 195), 3031),
        server_key: ed25519_dalek::PublicKey::from_bytes(&key).unwrap(),
        socket_read_timeout: Duration::from_secs(10),
        socket_send_timeout: Duration::from_secs(10),
    }
}
/// Maps shard id to seqno
type ShardBlocks = Arc<dashmap::DashMap<ShardIdent, i32>>;

pub struct NodeClient {
    node: NodeConnection,
    last_block: LastBlock,
    config: Config,
    shard_cache: ShardBlocks,
}

impl NodeClient {
    pub async fn new(config: Config) -> Result<Self> {
        let manager = AdnlManageConnection::new(config.adnl.clone());
        let pool = Pool::builder()
            .max_size(config.pool_size)
            .build(manager)
            .await?;

        Ok(Self {
            node: NodeConnection::new(pool, config.pool_size),
            last_block: LastBlock::new(&config.threshold),
            config,
            shard_cache: ShardBlocks::default(),
        })
    }
}

impl NodeClient {
    async fn bad_block_resolver<S>(
        self: Arc<Self>,
        mut bad_block_queue: tokio::sync::mpsc::Receiver<BlockId>,
        sink: S,
    ) where
        S: Sink<ton_block::Block> + Clone + Send + Sync + Unpin + 'static,
        <S as futures::Sink<ton_block::Block>>::Error: std::error::Error,
    {
        while let Some(id) = bad_block_queue.recv().await {
            let permit = self.node.acquire_spawn().await;
            let pool = self.node.clone();
            tokio::spawn({
                let id = id.clone();
                let mut tx = sink.clone();
                async move {
                    let result = tryhard::retry_fn(|| pool.query_block_by_seqno(id.clone()))
                        .retries(10)
                        .exponential_backoff(Duration::from_secs(1))
                        .await;
                    drop(permit);
                    match result {
                        Ok(a) => {
                            if let Err(e) = tx.send(a).await {
                                log::error!("Failed sending via channel: {}", e)
                            }
                        }
                        Err(e) => {
                            log::error!("Failed querying info about bad block: {}", e);
                        }
                    }
                }
            });
        }
    }

    async fn blocks_producer(
        self: Arc<Self>,
        start_block: Option<BlockId>,
        new_mc_blocks_queue: tokio::sync::mpsc::Sender<ton::ton_node::blockidext::BlockIdExt>,
        pool_size: i32,
    ) -> Result<()> {
        async fn get_block_id(
            pool: &NodeConnection,
            id: BlockId,
        ) -> Result<ton_api::ton::ton_node::blockidext::BlockIdExt> {
            tryhard::retry_fn(|| async {
                let pool = pool.clone();
                let id = id.clone();
                pool.get_block_ext_id(id).await
            })
            .retries(20)
            .exponential_backoff(Duration::from_secs(1))
            .max_delay(Duration::from_secs(600))
            .await
        }
        let top_block = tryhard::retry_fn(|| self.last_block.get_last_block(&self))
            .retries(100)
            .await
            .expect("Fatal block producer error");

        let mut current_block = match start_block {
            Some(a) => get_block_id(&self.node, a)
                .await
                .expect("Fatal block producer error"),
            None => top_block.clone(),
        };

        macro_rules! get_last_block {
            () => {
                tryhard::retry_fn(|| self.last_block.get_last_block(&self))
                    .retries(20)
                    .exponential_backoff(Duration::from_secs(1))
                    .max_delay(Duration::from_secs(600))
                    .await
                    .expect("Fatal block producer error");
            };
        }

        loop {
            let blocks_diff = top_block.seqno - current_block.seqno;
            if blocks_diff != 0 {
                if let Err(e) = new_mc_blocks_queue.send(current_block.clone()).await {
                    log::error!("Failed sending mc block: {}", e);
                    return Ok(());
                }
                // 8 blocks per connection
                let query_count = std::cmp::min(pool_size * 16, blocks_diff);
                log::debug!("Query count: {}, diff: {}", query_count, blocks_diff);
                let block = get_block_id(
                    &self.node,
                    BlockId {
                        workchain: current_block.workchain,
                        shard: current_block.shard,
                        seqno: current_block.seqno + query_count,
                    },
                )
                .await
                .expect("Fatal block producer error");
                current_block = block;
            } else if current_block == top_block {
                log::info!("Synced");
                log::info!("Current mc height: {}", current_block.seqno);
                let mut block = get_last_block!();
                loop {
                    let current_block = get_last_block!();
                    if current_block.seqno == block.seqno {
                        tokio::time::sleep(self.config.indexer_interval).await;
                    } else {
                        block = current_block;
                        if let Err(e) = new_mc_blocks_queue.send(block.clone()).await {
                            log::error!("Fail sending block id: {}", e);
                        }
                    }
                }
            } else {
                log::error!("Logic has broken");
                let block = get_block_id(
                    &self.node,
                    BlockId {
                        workchain: current_block.workchain,
                        shard: current_block.shard,
                        seqno: current_block.seqno + 1,
                    },
                )
                .await
                .expect("Fatal block producer error");
                current_block = block;
                new_mc_blocks_queue.send(current_block.clone()).await?;
            }
        }
    }

    pub async fn spawn_indexer<S, McBlocks>(
        self: &Arc<Self>,
        seqno: Option<BlockId>,
        sink: S,
        mut mc_blocks: McBlocks,
    ) -> QueryResult<()>
    where
        S: Sink<ton_block::Block> + Clone + Send + Sync + Unpin + 'static,
        <S as futures::Sink<ton_block::Block>>::Error: std::error::Error,
        McBlocks: Sink<BlockId> + Clone + Send + Sync + Unpin + 'static,
        <McBlocks as futures::Sink<BlockId>>::Error: std::error::Error,
    {
        let (bad_blocks_tx, bad_blocks_rx) = tokio::sync::mpsc::channel(256);
        let indexer = Arc::downgrade(self);

        tokio::spawn(self.clone().bad_block_resolver(bad_blocks_rx, sink.clone()));

        let (masterchain_blocks_tx, mut masterchain_blocks_rx) = tokio::sync::mpsc::channel(2);

        tokio::spawn(self.clone().blocks_producer(
            seqno,
            masterchain_blocks_tx,
            self.config.pool_size as i32,
        ));
        tokio::spawn(async move {
            while let Some(block) = masterchain_blocks_rx.recv().await {
                let indexer = match indexer.upgrade() {
                    Some(indexer) => indexer,
                    None => {
                        log::error!("Indexer refs are empty. Quiting");
                        return;
                    }
                };
                let blockid = BlockId {
                    workchain: block.workchain,
                    shard: block.shard,
                    seqno: block.seqno,
                };
                log::trace!("Indexer step. Id: {}", block.seqno);
                tryhard::retry_fn(|| async {
                    indexer
                        .indexer_step(block.clone(), sink.clone(), bad_blocks_tx.clone())
                        .await
                })
                .retries(10)
                .exponential_backoff(Duration::from_secs(2))
                .await
                .expect("fatal indexer error");
                mc_blocks.send(blockid).await.expect("mc blocks broken");
            }
        });

        Ok(())
    }

    async fn indexer_step<S>(
        self: &Arc<Self>,
        mc_block: ton::ton_node::blockidext::BlockIdExt,
        sink: S,
        bad_blocks_tx: Sender<BlockId>,
    ) -> Result<()>
    where
        S: Sink<ton_block::Block> + Clone + Send + Sync + Unpin + 'static,
        <S as futures::Sink<ton_block::Block>>::Error: std::error::Error,
    {
        let block = self
            .node
            .query_block(mc_block)
            .await
            .context("Failed getting block id")?;
        let extra = block
            .extra
            .read_struct()
            .and_then(|extra| extra.read_custom())
            .map_err(|e| anyhow::anyhow!("Failed to parse block info: {:?}", e))?;

        let extra = match extra {
            Some(extra) => extra,
            None => anyhow::bail!("No extra in block"),
        };

        let mut num_of_shards = 1; // for barrier
        extra
            .shards()
            .iterate_shards(|_, _| {
                num_of_shards += 1;
                Ok(true)
            })
            .convert()
            .context("Failed iterating shards")?;

        log::trace!("Num of shards: {}", num_of_shards);
        let num_of_tasks = Arc::new(Barrier::new(num_of_shards));
        extra
            .shards()
            .iterate_shards(|shard_id, shard| {
                log::trace!("Shard id: {:?}, shard block: {}", shard_id, shard.seq_no);
                let idxr = self.clone();
                let task = idxr.process_shard(
                    shard_id,
                    shard,
                    sink.clone(),
                    num_of_tasks.clone(),
                    bad_blocks_tx.clone(),
                );
                tokio::spawn(task);
                Ok(true)
            })
            .map_err(|e| anyhow::anyhow!("Failed to iterate shards: {:?}", e))?;

        // Each shard manges it processed blocks.
        // We wait for download of all shard blocks and than we believe, that shard layer is processed.
        log::trace!("Start waiting for shards");
        num_of_tasks.wait().await;
        log::trace!("Finished waiting for shards");
        Ok(())
    }

    async fn process_shard<S>(
        self: Arc<Self>,
        shard_id: ShardIdent,
        shard: ShardDescr,
        sink: S,
        barrier: Arc<Barrier>,
        bad_blocks_tx: Sender<BlockId>,
    ) where
        S: Sink<ton_block::Block> + Clone + Send + Sync + Unpin + 'static,
        <S as futures::Sink<ton_block::Block>>::Error: std::error::Error,
    {
        let workchain = shard_id.workchain_id();
        let shard_id_numeric = shard_id.shard_prefix_with_tag() as i64;
        let current_seqno = shard.seq_no as i32;

        let last_known_block = *self
            .shard_cache
            .entry(shard_id)
            .or_insert(current_seqno)
            .value();
        log::trace!(
            "{:016x} Last known block {}. Current {}",
            shard_id_numeric,
            last_known_block,
            current_seqno
        );
        let processed_num = (current_seqno - last_known_block) as usize;

        log::trace!(
            "Processing blocks {} in shard {:016x}.",
            processed_num,
            shard_id_numeric,
        );

        // +1 because of waiting in final barrier
        let num_of_tasks = Arc::new(tokio::sync::Barrier::new(processed_num + 1));
        log::trace!("{:016x} size: {}", shard_id_numeric, processed_num + 1);

        let mut connection = self.node.get_connection_or_die().await.deref().clone();
        for (num, seq_no) in (last_known_block..current_seqno).enumerate() {
            let permit = self.node.acquire_spawn().await;
            // We are downloading 8 blocks per 1 connection
            connection = if num % 8 == 0 {
                self.node.get_connection_or_die().await.deref().clone()
            } else {
                connection
            };
            let task_connection = connection.clone();
            log::trace!("{:016x} Spawning", shard_id_numeric);
            let num_of_tasks = num_of_tasks.clone();
            let mut sink = sink.clone();
            let bad_blocks_tx = bad_blocks_tx.clone();
            let task = async move {
                let id = BlockId {
                    workchain,
                    shard: shard_id_numeric,
                    seqno: seq_no,
                };
                log::trace!("{:016x} {} Start", shard_id_numeric, seq_no);
                let block =
                    NodeConnection::query_block_by_seqno_inner(task_connection, id.clone()).await;
                match block {
                    Ok(a) => sink.send(a).await.expect("Blocks channel is broken"),
                    Err(_e) => {
                        bad_blocks_tx
                            .send(id)
                            .await
                            .expect("Bad blocks resolver is broken");
                    }
                }
                drop(permit);
                log::trace!("{:016x} {} Done", shard_id_numeric, seq_no);
                tokio::spawn(async move { num_of_tasks.wait().await });
            };
            tokio::spawn(task);
        }
        log::trace!("{:016x} Start waiting for tasks", shard_id_numeric);
        //  Waiting local spawned tasks
        num_of_tasks.wait().await;
        log::trace!("{:016x} Finish waiting for tasks", shard_id_numeric);
        self.shard_cache.insert(shard_id, current_seqno);
        // Notifying that we have processed all blocks.
        barrier.wait().await;
    }

    /// Return all transactions  for `contract_address`. Latest transaction first
    pub async fn get_all_transactions(
        &self,
        contract_address: MsgAddressInt,
    ) -> Result<Vec<RawTransaction>> {
        let mut all_transactions = Vec::with_capacity(16);
        let mut tx_id = None;
        loop {
            let mut res = match self
                .get_transactions(contract_address.clone(), tx_id, 16)
                .await
            {
                Ok(a) => a,
                Err(e) => {
                    log::error!("Failed getting transactions: {}", e);
                    return Ok(all_transactions);
                }
            };

            if res.is_empty() {
                log::debug!("Empty answer, no more transactions");
                break;
            }
            log::debug!("Got {} transactions", res.len());
            // Checked on previous step
            let hash = res.last().as_ref().trust_me().data.prev_trans_hash;
            let lt = res.last().as_ref().trust_me().data.prev_trans_lt;

            log::debug!("Getting txs before {}, lt: {}", hex::encode(&hash), lt);
            let id = TransactionId { lt, hash };
            tx_id = Some(id);
            all_transactions.append(&mut res);
        }
        Ok(all_transactions)
    }

    pub async fn get_contract_state(
        &self,
        contract_address: MsgAddressInt,
    ) -> Result<nekoton::transport::models::RawContractState> {
        use nekoton::abi::{GenTimings, LastTransactionId};

        let last_block = self.last_block.get_last_block(&self).await?;
        let id = contract_address
            .address()
            .get_bytestring(0)
            .as_slice()
            .try_into()?;
        let get_state = ton::rpc::lite_server::GetAccountState {
            id: last_block,
            account: ton::lite_server::accountid::AccountId {
                workchain: contract_address.workchain_id(),
                id: ton::int256(id),
            },
        };
        let response = self.node.query(get_state).await?.only();
        let state = match ton_block::Account::construct_from_bytes(&response.state.0) {
            Ok(ton_block::Account::Account(account)) => {
                let q_roots =
                    ton_types::deserialize_cells_tree(&mut std::io::Cursor::new(&response.proof.0))
                        .map_err(|_| anyhow::anyhow!("InvalidAccountStateProof"))?;
                if q_roots.len() != 2 {
                    anyhow::bail!("InvalidAccountStateProof")
                }

                let merkle_proof = ton_block::MerkleProof::construct_from_cell(q_roots[1].clone())
                    .map_err(|_| anyhow::anyhow!("InvalidAccountStateProof"))?;
                let proof_root = merkle_proof.proof.virtualize(1);

                let ss = ton_block::ShardStateUnsplit::construct_from(&mut proof_root.into())
                    .map_err(|_| anyhow::anyhow!("InvalidAccountStateProof"))?;

                let shard_info = ss
                    .read_accounts()
                    .and_then(|accounts| {
                        accounts.get(&ton_types::UInt256::from(
                            // contract_address.get_address().get_bytestring(0),
                            id,
                        ))
                    })
                    .map_err(|_| anyhow::anyhow!("InvalidAccountStateProof"))?;

                if let Some(shard_info) = shard_info {
                    RawContractState::Exists(ExistingContract {
                        account,
                        timings: GenTimings::Known {
                            gen_lt: ss.gen_lt(),
                            gen_utime: (chrono::Utc::now().timestamp() - 10) as u32, // TEMP!!!!!, replace with ss.gen_time(),
                        },
                        last_transaction_id: LastTransactionId::Exact(TransactionId {
                            lt: shard_info.last_trans_lt(),
                            hash: *shard_info.last_trans_hash(),
                        }),
                    })
                } else {
                    RawContractState::NotExists
                }
            }
            _ => RawContractState::NotExists,
        };
        Ok(state)
    }

    pub async fn get_transactions(
        &self,
        address: MsgAddressInt,
        from: Option<TransactionId>,
        count: u8,
    ) -> Result<Vec<RawTransaction>> {
        async fn get_transactions_inner(
            client: &NodeClient,
            address: MsgAddressInt,
            from: Option<TransactionId>,
            count: u8,
        ) -> Result<Option<Vec<u8>>> {
            let from = match from {
                Some(id) => id,
                None => match client.get_contract_state(address.clone()).await? {
                    RawContractState::Exists(contract) => {
                        contract.last_transaction_id.to_transaction_id()
                    }
                    RawContractState::NotExists => return Ok(None),
                },
            };

            let response = client
                .node
                .query(ton::rpc::lite_server::GetTransactions {
                    count: count as i32,
                    account: ton::lite_server::accountid::AccountId {
                        workchain: address.workchain_id() as i32,
                        id: ton::int256(
                            ton_types::UInt256::from_be_bytes(&address.address().get_bytestring(0)).into(),
                        ),
                    },
                    lt: from.lt as i64,
                    hash: from.hash.into(),
                })
                .await?;

            Ok(Some(response.transactions().0.clone()))
        }
        let data = match get_transactions_inner(self, address, from, count).await? {
            None => return Ok(Vec::new()),
            Some(a) => a,
        };
        let transactions = match ton_types::deserialize_cells_tree(&mut std::io::Cursor::new(data))
        {
            Ok(a) => a,
            Err(e) => {
                log::error!("Failed deserilizing transactions list: {}", e);
                return Ok(Vec::new());
            }
        };

        let mut result = Vec::with_capacity(transactions.len());
        for item in transactions {
            result.push(RawTransaction {
                hash: item.repr_hash(),
                data: ton_block::Transaction::construct_from_cell(item)
                    .map_err(|_| anyhow::anyhow!("Invalid transaction"))?,
            });
        }
        Ok(result)
    }

    pub async fn run_local(
        &self,
        contract_address: MsgAddressInt,
        function: &ton_abi::Function,
        input: &[ton_abi::Token],
    ) -> Result<nekoton::abi::ExecutionOutput> {
        use nekoton::abi::FunctionExt;

        let state = self.get_contract_state(contract_address).await?;
        let state = match state {
            RawContractState::NotExists => {
                anyhow::bail!("Account doesn't exist")
            }
            RawContractState::Exists(a) => a,
        };
        function.clone().run_local(
            state.account,
            state.timings,
            &state.last_transaction_id,
            input,
        )
    }
}

#[derive(Clone)]
struct NodeConnection {
    connection: Pool<AdnlManageConnection>,
    spawn_limiter: Arc<Semaphore>,
}

impl NodeConnection {
    fn new(pool: Pool<AdnlManageConnection>, pool_size: u32) -> Self {
        Self {
            connection: pool,
            spawn_limiter: Arc::new(Semaphore::new((pool_size * 1024) as usize)),
        }
    }

    pub async fn acquire_spawn(&self) -> OwnedSemaphorePermit {
        self.spawn_limiter
            .clone()
            .acquire_owned()
            .await
            .expect("We are not closing")
    }

    pub async fn get_connection_or_die(&self) -> PooledConnection<'_, AdnlManageConnection> {
        loop {
            match self.get_connection().await {
                Ok(a) => break a,
                Err(e) => {
                    log::error!("Failed getting connection: {}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };
        }
    }

    pub async fn get_connection(&self) -> QueryResult<PooledConnection<'_, AdnlManageConnection>> {
        let con = self
            .connection
            .get()
            .await
            .map_err(|_| QueryError::ConnectionError)?;
        Ok(con)
    }
    pub async fn query<T>(&self, query: T) -> QueryResult<T::Reply>
    where
        T: ton_api::Function,
    {
        let con = self.get_connection().await?;
        Self::query_inner(con.deref().clone(), query).await
    }

    async fn query_inner<T>(connection: Arc<AdnlTcpClient>, query: T) -> QueryResult<T::Reply>
    where
        T: ton_api::Function,
    {
        let query_bytes = query
            .boxed_serialized_bytes()
            .map_err(|_| QueryError::FailedToSerialize)?;

        let response = connection
            .query(&ton::TLObject::new(ton::rpc::lite_server::Query {
                data: query_bytes.into(),
            }))
            .await
            .map_err(|_| QueryError::ConnectionError)?;
        match response.downcast::<T::Reply>() {
            Ok(reply) => Ok(reply),
            Err(error) => match error.downcast::<ton::lite_server::Error>() {
                Ok(error) => Err(QueryError::LiteServer(error)),
                Err(_) => Err(QueryError::Unknown),
            },
        }
    }

    pub async fn query_block(
        &self,
        id: ton::ton_node::blockidext::BlockIdExt,
    ) -> QueryResult<ton_block::Block> {
        let block = self.query(ton::rpc::lite_server::GetBlock { id }).await?;
        let block = ton_block::Block::construct_from_bytes(&block.only().data.0)
            .map_err(|_| QueryError::InvalidBlock)?;
        Ok(block)
    }

    async fn query_block_inner(
        connection: Arc<AdnlTcpClient>,
        id: ton::ton_node::blockidext::BlockIdExt,
    ) -> QueryResult<ton_block::Block> {
        let block = Self::query_inner(connection, ton::rpc::lite_server::GetBlock { id }).await?;
        let block = ton_block::Block::construct_from_bytes(&block.only().data.0)
            .map_err(|_| QueryError::InvalidBlock)?;
        Ok(block)
    }

    async fn query_block_by_seqno_inner(
        connection: Arc<AdnlTcpClient>,
        id: ton::ton_node::blockid::BlockId,
    ) -> QueryResult<ton_block::Block> {
        let block_id = Self::query_inner(
            connection.clone(),
            ton::rpc::lite_server::LookupBlock {
                mode: 0x1,
                id,
                lt: None,
                utime: None,
            },
        )
        .await?;
        Self::query_block_inner(connection, block_id.only().id).await
    }

    pub async fn query_block_by_seqno(
        &self,
        id: ton::ton_node::blockid::BlockId,
    ) -> QueryResult<ton_block::Block> {
        let con = self.get_connection().await?;
        Self::query_block_by_seqno_inner(con.deref().clone(), id).await
    }

    async fn get_block_ext_id_inner(
        connection: Arc<AdnlTcpClient>,
        id: BlockId,
    ) -> Result<ton_api::ton::ton_node::blockidext::BlockIdExt> {
        Ok(Self::query_inner(
            connection,
            ton::rpc::lite_server::LookupBlock {
                mode: 0x1,
                id,
                lt: None,
                utime: None,
            },
        )
        .await?
        .id()
        .clone())
    }

    async fn get_block_ext_id(
        &self,
        id: BlockId,
    ) -> Result<ton_api::ton::ton_node::blockidext::BlockIdExt> {
        let con = self.get_connection().await?;
        Self::get_block_ext_id_inner(con.deref().clone(), id).await
    }
}
