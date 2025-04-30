use bitcoin::Network;
use bitcoin::secp256k1::PublicKey;
use chrono::Utc;
use corepc_node::get_available_port;
use corepc_node::{Conf, Node};
use ldk_sample::config::LdkUserInfo;
use ldk_sample::node_api::Node as LdkNode;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use tempfile::TempDir;
use tempfile::tempdir;

#[tokio::main]
async fn main() {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            writeln!(
                buf,
                "{} [{}] {}:{} - {}",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"), // Timestamp
                record.level(),                                   // Log level
                record.target(),                                  // Module path
                record.line().unwrap_or(0),                       // Line number
                record.args()                                     // Log message
            )
        })
        .filter(None, log::LevelFilter::Debug) // Default level
        .init();

    log::info!("Starting application");
    let bitcoind = setup_bitcoind().await;
    log::info!("Bitcoind setup complete");

    let ldk_data_dir = setup_test_dirs("ldk-sample-tests");
    log::info!("Ldk data dir: {:?}", ldk_data_dir);

    let (ldk1, ldk2) = start_ldk_nodes(&bitcoind, ldk_data_dir, "ldk-sample-tests").await;
    log::info!("Ldk nodes started");

    let (ldk1_pubkey, ldk2_pubkey) = connect_network(&ldk1, &ldk2, &bitcoind).await;
    log::info!("node1: {:?}", ldk1_pubkey);
    log::info!("node2: {:?}", ldk2_pubkey);
}

async fn start_ldk_nodes(
    bitcoind: &BitcoindNode,
    ldk_data_dir: PathBuf,
    test_name: &str,
) -> (LdkNode, LdkNode) {
    let connect_params = bitcoind.node.params.get_cookie_values().unwrap();

    let port = get_available_port().unwrap();
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    let cookie_values = connect_params.unwrap();

    let ldk1_config = LdkUserInfo {
        bitcoind_rpc_username: cookie_values.user.clone(),
        bitcoind_rpc_password: cookie_values.password.clone(),
        bitcoind_rpc_host: String::from("localhost"),
        bitcoind_rpc_port: bitcoind.node.params.rpc_socket.port(),
        ldk_data_dir: ldk_data_dir.clone(),
        ldk_announced_listen_addr: vec![addr.into()],
        ldk_peer_listening_port: port,
        ldk_announced_node_name: [0; 32],
        network: Network::Regtest,
        log_level: lightning::util::logger::Level::Trace,
        node_num: 1,
    };

    let port = get_available_port().unwrap();
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port);
    let ldk2_config = LdkUserInfo {
        bitcoind_rpc_username: cookie_values.user.clone(),
        bitcoind_rpc_password: cookie_values.password.clone(),
        bitcoind_rpc_host: String::from("localhost"),
        bitcoind_rpc_port: bitcoind.node.params.rpc_socket.port(),
        ldk_data_dir: ldk_data_dir.clone(),
        ldk_announced_listen_addr: vec![addr.into()],
        ldk_peer_listening_port: port,
        ldk_announced_node_name: [0; 32],
        network: Network::Regtest,
        log_level: lightning::util::logger::Level::Trace,
        node_num: 2,
    };

    let ldk1 = ldk_sample::start_ldk(ldk1_config, test_name).await;
    let ldk2 = ldk_sample::start_ldk(ldk2_config, test_name).await;

    (ldk1, ldk2)
}

fn setup_test_dirs(test_name: &str) -> PathBuf {
    let current_dir = std::env::current_dir().unwrap();
    let ldk_sample_dir = current_dir.join("ldk-sample-tests");
    let now_timestamp = Utc::now();
    let timestamp = now_timestamp.format("%d-%m-%Y-%H%M");
    let itest_dir = ldk_sample_dir.join(format!("test-{test_name}-{timestamp}"));
    let ldk_data_dir = itest_dir.join("ldk-data");

    std::fs::create_dir_all(ldk_sample_dir.clone()).unwrap();
    std::fs::create_dir_all(ldk_data_dir.clone()).unwrap();

    ldk_data_dir
}

// BitcoindNode holds the tools we need to interact with a Bitcoind node.
pub struct BitcoindNode {
    pub node: Node,
    _data_dir: TempDir,
    _zmq_block_port: u16,
    _zmq_tx_port: u16,
}

async fn setup_bitcoind() -> BitcoindNode {
    let data_dir = tempdir().unwrap();
    let data_dir_path = data_dir.path().to_path_buf();
    let mut conf = Conf::default();
    let zmq_block_port = get_available_port().unwrap();
    let zmq_tx_port = get_available_port().unwrap();
    log::debug!(
        "Using ZMQ ports: Block={}, Tx={}",
        zmq_block_port,
        zmq_tx_port
    );
    let zmq_block_port_arg = &format!("-zmqpubrawblock=tcp://127.0.0.1:{zmq_block_port}");
    let zmq_tx_port_arg = &format!("-zmqpubrawtx=tcp://127.0.0.1:{zmq_tx_port}");
    conf.tmpdir = Some(data_dir_path);
    conf.args = vec!["-regtest", zmq_block_port_arg, zmq_tx_port_arg];
    let bitcoind = match corepc_node::downloaded_exe_path() {
        Ok(_path) => {
            log::info!("Using downloaded bitcoind");
            Node::from_downloaded_with_conf(&conf).unwrap()
        }
        Err(_e) => {
            log::info!("Using system bitcoind");
            let exe = corepc_node::exe_path().unwrap();
            Node::with_conf(exe, &conf).unwrap()
        }
    };
    // Mine 101 blocks in our little regtest network so that the funds are spendable.
    // (See https://bitcoin.stackexchange.com/questions/1991/what-is-the-block-maturation-time)
    let address = bitcoind.client.new_address().unwrap();
    bitcoind.client.generate_to_address(101, &address).unwrap();

    BitcoindNode {
        node: bitcoind,
        _data_dir: data_dir,
        _zmq_block_port: zmq_block_port,
        _zmq_tx_port: zmq_tx_port,
    }
}

async fn connect_network(
    ldk1: &LdkNode,
    ldk2: &LdkNode,
    bitcoind: &BitcoindNode,
) -> (PublicKey, PublicKey) {
    // Here we'll produce a little network of channels:
    //
    // ldk1 <- ldk2
    //
    // ldk1 will be the offer creator, which will build a blinded route from ldk2 to ldk1.
    let (ldk1_pubkey, addr) = ldk1.get_node_info();
    let (ldk2_pubkey, addr_2) = ldk2.get_node_info();

    log::info!("Connecting ldk1 to ldk2");

    ldk1.connect_to_peer(ldk2_pubkey, addr_2).await.unwrap();

    let ldk2_fund_addr = ldk2.bitcoind_client.get_new_address().await;

    // We need to convert funding addresses to the form that the bitcoincore_rpc library recognizes.
    let ldk2_addr_string = ldk2_fund_addr.to_string();
    let ldk2_addr = bitcoincore_rpc::bitcoin::Address::from_str(&ldk2_addr_string)
        .unwrap()
        .require_network(bitcoincore_rpc::bitcoin::Network::Regtest)
        .unwrap();

    // Fund both of these nodes, open the channels, and synchronize the network.
    bitcoind
        .node
        .client
        .generate_to_address(6, &ldk2_addr)
        .unwrap();

    ldk2.open_channel(ldk1_pubkey, addr, 200000, 10000000, true)
        .await
        .unwrap();

    bitcoind
        .node
        .client
        .generate_to_address(20, &ldk2_addr)
        .unwrap();

    (ldk1_pubkey, ldk2_pubkey)
}
