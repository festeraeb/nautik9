//! Test harness for nexus-join: drive the same calls the apps make, from a terminal.
//!
//!   nexus-cli <state-dir> offer <invite-file>
//!   nexus-cli <state-dir> join <device-name> [seconds-to-watch]
//!   nexus-cli <state-dir> status [seconds-to-watch]
//!   nexus-cli <state-dir> leave [--force]
//!
//! SDK path: $NEXUS_SDK (default liblemonade_nexus_sdk.so on the loader path).

use nexus_join::{Config, Nexus};
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 3 {
        eprintln!("usage: nexus-cli <state-dir> offer|join|status|leave ...");
        std::process::exit(2);
    }
    let sdk = std::env::var("NEXUS_SDK").unwrap_or_else(|_| "liblemonade_nexus_sdk.so".into());
    let n = Nexus::open(Config { dir: PathBuf::from(&a[1]), sdk_paths: vec![PathBuf::from(sdk)], device_name: "nexus-cli".into(), poll_secs: 5 });
    let watch = |secs: u64| {
        for i in 0..=secs / 5 {
            if i > 0 {
                std::thread::sleep(Duration::from_secs(5));
            }
            let s = n.status();
            println!("{}", serde_json::to_string(&s).unwrap());
        }
    };
    match a[2].as_str() {
        "offer" => {
            let text = std::fs::read_to_string(&a[3]).expect("read invite");
            match n.offer(&text) {
                Ok(s) => println!("offer: {s}"),
                Err(e) => {
                    eprintln!("offer refused: {e}");
                    std::process::exit(1)
                }
            }
        }
        "join" => {
            match n.join(a.get(3).map(String::as_str)) {
                Ok(m) => println!("joined: {m}"),
                Err(e) => {
                    eprintln!("join failed: {e}");
                    std::process::exit(1)
                }
            }
            watch(a.get(4).and_then(|s| s.parse().ok()).unwrap_or(10));
        }
        "status" => {
            // give the restore thread time to rejoin
            std::thread::sleep(Duration::from_secs(4));
            watch(a.get(3).and_then(|s| s.parse().ok()).unwrap_or(0));
        }
        "leave" => {
            std::thread::sleep(Duration::from_secs(4));
            match n.leave(a.get(3).map(|s| s == "--force").unwrap_or(false)) {
                Ok(v) => println!("left: {v}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1)
                }
            }
        }
        x => eprintln!("unknown command {x}"),
    }
}
