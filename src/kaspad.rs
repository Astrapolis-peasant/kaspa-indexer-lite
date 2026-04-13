use std::time::Duration;

use kaspa_wrpc_client::client::ConnectOptions;
use kaspa_wrpc_client::prelude::*;
use kaspa_wrpc_client::{KaspaRpcClient, WrpcEncoding};

pub async fn connect(url: &str) -> KaspaRpcClient {
    let network_id = NetworkId::new(NetworkType::Mainnet);
    let client = KaspaRpcClient::new_with_args(WrpcEncoding::Borsh, Some(url), None, Some(network_id), None)
        .expect("create KaspaRpcClient");
    client
        .connect(Some(ConnectOptions {
            block_async_connect: true,
            strategy: ConnectStrategy::Fallback,
            url: None,
            connect_timeout: Some(Duration::from_secs(10)),
            retry_interval: None,
        }))
        .await
        .expect("connect to kaspad");
    client
}
