use tokio::sync::mpsc::Sender;

mod core;
mod protocol;
mod types;

use types::*;

#[derive(Clone, Debug)]
pub struct Config {
    server_addr: String,

    is_precise: bool,

    affintiy: usize,
    cpu_scale: f64,
}

pub enum ControlMsg {
    Connect(usize),
    ReceiveTask(String, String),
    SendTask(Submission, TestCase),
}
fn main() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(process());
}

async fn process() {
    let config_str = std::fs::read_to_string("settings.toml").unwrap();

    let config_val: toml::value::Table = toml::from_str(&config_str).unwrap();

    let server_ip = config_val.get("server_ip").unwrap().as_str().unwrap();
    let server_port = config_val.get("server_port").unwrap().as_integer().unwrap();

    let is_preise = config_val.get("is_precise").unwrap().as_bool().unwrap();
    let cpu_scale = config_val.get("cpu_scale").unwrap().as_float().unwrap();
    let cores = config_val.get("affinity_core").unwrap().as_array().unwrap();

    let (tx, mut rx) = tokio::sync::mpsc::channel::<ControlMsg>(128);
    for i in cores {
        let config = Config {
            server_addr: format!("{}:{}", server_ip, server_port),
            is_precise: is_preise,
            affintiy: i.as_integer().unwrap() as usize,
            cpu_scale,
        };

        let tx = tx.clone();
        tokio::spawn(core_wrapper(config, tx));
    }

    loop {
        if let Some(data) = rx.recv().await {
            match data {
                ControlMsg::Connect(core_id) => {
                    println!("Connected - Core {}", core_id);
                }
                ControlMsg::ReceiveTask(submission, test_case) => {
                    println!("Received task: {:?}, {:?}", submission, test_case);
                }
                ControlMsg::SendTask(submission, test_case) => {
                    println!("Sent task: {:?} {:?}", submission, test_case);
                }
            }
        }
    }
}

async fn core_wrapper(config: Config, tx: Sender<ControlMsg>) {
    let core_id = config.affintiy;

    loop {
        let core = core::Core::new(config.clone(), tx.clone());
        core.run().await;

        eprintln!("Core {} exited - try reconnect in 5 secs", core_id);

        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
