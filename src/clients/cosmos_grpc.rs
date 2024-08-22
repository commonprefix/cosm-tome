use async_trait::async_trait;
use cosmrs::proto::cosmos::tx::v1beta1::service_client::ServiceClient;
use cosmrs::proto::cosmos::tx::v1beta1::{
    BroadcastMode as ProtoBroadcastMode, BroadcastTxRequest, GetTxRequest, SimulateRequest,
};
use tonic::codec::ProstCodec;

use cosmrs::proto::traits::Message;

use crate::chain::fee::GasInfo;
use crate::chain::response::{ChainResponse, Code};
use crate::chain::{error::ChainError, response::ChainTxResponse};
use crate::modules::tx::model::{BroadcastMode, RawTx};

use super::client::CosmosClient;

#[derive(Clone, Debug)]
pub struct CosmosgRPC {
    grpc_endpoint: String,
}

impl CosmosgRPC {
    pub fn new(grpc_endpoint: String) -> Self {
        Self { grpc_endpoint }
    }

    // Uses underlying grpc client to make calls to any gRPC service
    // without having to use the tonic generated clients for each cosmos module
    async fn grpc_call<I, O>(
        &self,
        req: impl tonic::IntoRequest<I>,
        path: &str,
    ) -> Result<O, ChainError>
    where
        I: Message + 'static,
        O: Message + Default + 'static,
    {
        let conn = tonic::transport::Endpoint::new(self.grpc_endpoint.clone())?
            .connect()
            .await?;

        let mut client = tonic::client::Grpc::new(conn);

        client.ready().await?;

        // NOTE: `I` and `O` in ProstCodec have static lifetime bounds:
        let codec: ProstCodec<I, O> = tonic::codec::ProstCodec::default();
        let res = client
            .unary(
                req.into_request(),
                path.parse().map_err(|_| ChainError::QueryPath {
                    url: path.to_string(),
                })?,
                codec,
            )
            .await
            .map_err(ChainError::tonic_status)?;

        Ok(res.into_inner())
    }
}

#[async_trait]
impl CosmosClient for CosmosgRPC {
    async fn query<I, O>(&self, msg: I, path: &str) -> Result<O, ChainError>
    where
        I: Message + Default + tonic::IntoRequest<I> + 'static,
        O: Message + Default + 'static,
    {
        let res = self.grpc_call::<I, O>(msg, path).await?;

        Ok(res)
    }

    #[allow(deprecated)]
    async fn simulate_tx(&self, tx: &RawTx) -> Result<GasInfo, ChainError> {
        let mut client = ServiceClient::connect(self.grpc_endpoint.clone()).await?;

        let req = SimulateRequest {
            tx: None,
            tx_bytes: tx.to_bytes()?,
        };

        let gas_info = client
            .simulate(req)
            .await
            .map_err(|e| ChainError::CosmosSdk {
                res: ChainResponse {
                    code: Code::Err(e.code() as u32),
                    log: e.message().to_string(),
                    ..Default::default()
                },
            })?
            .into_inner()
            .gas_info
            .ok_or(ChainError::Simulation)?;

        Ok(gas_info.into())
    }

    async fn broadcast_tx(
        &self,
        tx: &RawTx,
        mode: BroadcastMode,
    ) -> Result<ChainTxResponse, ChainError> {
        let mut client = ServiceClient::connect(self.grpc_endpoint.clone()).await?;

        let req = BroadcastTxRequest {
            tx_bytes: tx.to_bytes()?,
            mode: mode as i32,
        };

        let res = client
            .broadcast_tx(req)
            .await
            .map_err(ChainError::tonic_status)?
            .into_inner();

        let txhash = match res.tx_response {
            Some(response) => response.txhash,
            None => {
                return Err(ChainError::InvalidResponse {
                    error: "tx_response is missing".to_string(),
                })
            }
        };

        let mut tx_res = self.get_tx(&txhash).await?;
        let mut retries = 0;
        while tx_res.res.code.is_err() && retries < 10 {
            retries += 1;
            tx_res = self.get_tx(&txhash).await?;
            println!("Retrying tx broadcast: {:?}", tx_res);
        }

        if tx_res.res.code.is_err() {
            return Err(ChainError::CosmosSdk { res: tx_res.res });
        }

        Ok(tx_res)
    }

    async fn broadcast_tx_block(&self, tx: &RawTx) -> Result<ChainTxResponse, ChainError> {
        let mut client = ServiceClient::connect(self.grpc_endpoint.clone()).await?;

        let req = BroadcastTxRequest {
            tx_bytes: tx.to_bytes()?,
            mode: ProtoBroadcastMode::Block.into(),
        };

        let res = client
            .broadcast_tx(req)
            .await
            .map_err(ChainError::tonic_status)?
            .into_inner();

        let res: ChainTxResponse = res.tx_response.unwrap().try_into()?;

        if res.res.code.is_err() {
            return Err(ChainError::CosmosSdk { res: res.res });
        }

        Ok(res)
    }

    async fn get_tx(&self, tx_hash: &String) -> Result<ChainTxResponse, ChainError> {
        let mut client = ServiceClient::connect(self.grpc_endpoint.clone()).await?;

        let req = GetTxRequest {
            hash: tx_hash.clone(),
        };

        let res = client
            .get_tx(req)
            .await
            .map_err(ChainError::tonic_status)?
            .into_inner();

        let res: ChainTxResponse = res.tx_response.unwrap().try_into()?;

        if res.res.code.is_err() {
            return Err(ChainError::CosmosSdk { res: res.res });
        }

        Ok(res)
    }
}
