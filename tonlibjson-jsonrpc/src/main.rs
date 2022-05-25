use std::sync::Arc;
use anyhow::anyhow;
use axum::{Json, Router, routing::post};
use futures::future::Either::{Left, Right};
use serde_json::{json, Value};
use serde::{Deserialize, Serialize};
use tokio_stream::StreamExt;
use tonlibjson_tokio::{AsyncClient, BlockIdExt, ClientBuilder, InternalTransactionId, MasterchainInfo, RawTransaction, ShortTxId};

#[derive(Deserialize, Debug)]
struct LookupBlockParams {
    workchain: i64,
    shard: String,
    seqno: Option<u64>,
    lt: Option<i64>,
    unixtime: Option<u64>
}

#[derive(Deserialize, Debug)]
struct ShardsParams {
    seqno: u64
}

#[derive(Deserialize, Debug)]
struct BlockHeaderParams {
    workchain: i64,
    shard: String,
    seqno: u64,
    root_hash: Option<String>,
    file_hash: Option<String>
}

#[derive(Deserialize, Debug)]
struct BlockTransactionsParams {
    workchain: i64,
    shard: String,
    seqno: u64,
    root_hash: Option<String>,
    file_hash: Option<String>,
    after_lt: Option<i64>,
    after_hash: Option<String>,
    count: Option<u8>
}

#[derive(Deserialize, Debug)]
struct AddressParams {
    address: String
}

#[derive(Deserialize, Debug)]
struct TransactionsParams {
    address: String,
    limit: Option<u16>,
    lt: Option<String>,
    hash: Option<String>,
    to_lt: Option<String>,
    archival: Option<bool>
}

#[derive(Deserialize, Debug)]
struct SendBocParams {
    boc: String
}

#[derive(Debug, Deserialize)]
#[serde(tag = "method")]
enum Method {
    #[serde(rename = "lookupBlock")]
    LookupBlock { params: LookupBlockParams },
    #[serde(rename = "shards")]
    Shards { params: ShardsParams },
    #[serde(rename = "getBlockHeader")]
    BlockHeader { params: BlockHeaderParams },
    #[serde(rename = "getBlockTransactions")]
    BlockTransactions { params: BlockTransactionsParams },
    #[serde(rename = "getAddressInformation")]
    AddressInformation { params: AddressParams },
    #[serde(rename = "getExtendedAddressInformation")]
    ExtendedAddressInformation { params: AddressParams },
    #[serde(rename = "getTransactions")]
    Transactions { params: TransactionsParams },
    #[serde(rename = "sendBoc")]
    SendBoc { params: SendBocParams },
    #[serde(rename = "getMasterchainInfo")]
    MasterchainInfo
}

#[derive(Debug, Deserialize)]
struct JsonRequest {
    jsonrpc: String,
    id: u64,
    #[serde(flatten)]
    method: Method
}

#[derive(Debug, Serialize)]
struct JsonError {
    code: i32,
    message: String
}

#[derive(Debug, Serialize)]
struct JsonResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    jsonrpc: String,
    id: u64
}

impl JsonResponse {
    fn new(id: u64, result: Value) -> Self {
        return Self {
            ok: true,
            result: Some(result),
            error: None,
            jsonrpc: "2.0".to_string(),
            id: id
        }
    }

    fn error(id: u64, e: anyhow::Error) -> Self {
        return Self {
            ok: false,
            result: None,
            error: Some(JsonError { code: -32603, message: e.to_string() }),
            jsonrpc: "2.0".to_string(),
            id
        }
    }
}

struct RpcServer {
    client: AsyncClient
}

type RpcResponse<T> = anyhow::Result<T>;

impl RpcServer {
    async fn master_chain_info(&self) -> RpcResponse<MasterchainInfo> {
        self.client.get_masterchain_info().await
    }

    async fn lookup_block(&self, params: LookupBlockParams) -> RpcResponse<Value> {
        let workchain = params.workchain;
        let shard = params.shard.parse::<i64>()?;

        match (params.seqno, params.lt, params.unixtime) {
            (Some(seqno), None, None) if seqno > 0 => self.client.look_up_block_by_seqno(workchain, shard, seqno).await,
            (None, Some(lt), None) if lt > 0 => self.client.look_up_block_by_lt(workchain, shard, lt).await,
            (None, None, Some(_)) => Err(anyhow!("unixtime is not supported")),
            _ => Err(anyhow!("seqno or lt or unixtime must be provided"))
        }
    }

    async fn shards(&self, params: ShardsParams) -> RpcResponse<Value> {
        self.client.get_shards(params.seqno).await
    }

    async fn get_block_header(&self, params: BlockHeaderParams) -> RpcResponse<Value> {
        let shard = params.shard.parse::<i64>()?;

        self.client.get_block_header(
            params.workchain,
            shard,
            params.seqno
        ).await
    }

    async fn get_block_transactions(&self, params: BlockTransactionsParams) -> RpcResponse<Value> {
        let shard = params.shard.parse::<i64>()?;
        let count = params.count.unwrap_or(200);

        let block_json = self.client.look_up_block_by_seqno(params.workchain, shard, params.seqno).await?;

        let block = serde_json::from_value::<BlockIdExt>(block_json)?;

        let stream = self.client.get_tx_stream(block.clone()).await;
        let tx: Vec<ShortTxId> = stream
            .map(|tx: ShortTxId| {
                println!("{}", &tx.account);
                ShortTxId {
                    account: format!("{}:{}", block.workchain, base64_to_hex(&tx.account).unwrap()),
                    hash: tx.hash,
                    lt: tx.lt,
                    mode: tx.mode
                }
            })
            .collect()
            .await;


        Ok(json!({
                "@type": "blocks.transactions",
                "id": block,
                "incomplete": false,
                "req_count": count,
                "transactions": &tx
            }))
    }

    async fn get_address_information(&self, params: AddressParams) -> RpcResponse<Value> {
        self.client.raw_get_account_state(&params.address).await
    }

    async fn get_extended_address_information(&self, params: AddressParams) -> RpcResponse<Value> {
        self.client.get_account_state(&params.address).await
    }

    async fn get_transactions(&self, params: TransactionsParams) -> RpcResponse<Value> {
        let address = params.address;
        let count = params.limit.unwrap_or(10);
        let max_lt = params.to_lt.and_then(|x| x.parse::<i64>().ok());
        let lt = params.lt;
        let hash = params.hash;

        let stream = match (lt, hash) {
            (Some(lt), Some(hash)) => Left(
                self.client.get_account_tx_stream_from(address, InternalTransactionId {hash, lt})
            ),
            _ => Right(self.client.get_account_tx_stream(address).await)
        };
        let stream = match max_lt {
            Some(to_lt) => Left(stream.take_while(move |tx: &RawTransaction|
                tx.transaction_id.lt.parse::<i64>().unwrap() > to_lt
            )),
            _ => Right(stream)
        };

        let txs: Vec<RawTransaction> = stream
            .take(count as usize)
            .collect()
            .await;

        Ok(serde_json::to_value(txs)?)
    }

    async fn send_boc(&self, params: SendBocParams) -> RpcResponse<Value> {
        let boc = base64::decode(params.boc)?;
        let b64 = base64::encode(boc);

        self.client.send_message(&b64).await
    }
}

async fn dispatch_method(Json(payload): Json<JsonRequest>, rpc: Arc<RpcServer>) -> Json<JsonResponse> {
    println!("{:?}", payload);

    let result = match payload.method {
        Method::MasterchainInfo => rpc.master_chain_info().await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::LookupBlock { params } => rpc.lookup_block(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::Shards { params } => rpc.shards(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::BlockHeader { params } => rpc.get_block_header(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::BlockTransactions { params } => rpc.get_block_transactions(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::AddressInformation { params } => rpc.get_address_information(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::ExtendedAddressInformation { params } => rpc.get_extended_address_information(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::Transactions { params } => rpc.get_transactions(params).await.and_then(|x| Ok(serde_json::to_value(x)?)),
        Method::SendBoc { params } => rpc.send_boc(params).await.and_then(|x| Ok(serde_json::to_value(x)?))
    };

    Json(
        match result {
            Ok(v) => JsonResponse::new(payload.id, v),
            Err(e) => JsonResponse::error(payload.id, e)
        }
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = ClientBuilder::from_file("./liteserver_config.json")
        .unwrap()
        // .disable_logging()
        .build()
        .await?;

    client.synchronize().await?;

    let rpc = Arc::new(RpcServer {
        client
    });

    let app = Router::new().route("/", post({
        let rpc = Arc::clone(&rpc);
        move |body| dispatch_method(body, Arc::clone(&rpc))
    }));

    axum::Server::bind(&"0.0.0.0:3030".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();

    Ok(())
}

fn base64_to_hex(b: &str) -> anyhow::Result<String> {
    let bytes = base64::decode(b)?;
    let hex = hex::encode(bytes);

    return Ok(hex)
}