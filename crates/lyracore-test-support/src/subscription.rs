use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use serde_json::Value;

pub struct TableSubscription {
    child: Child,
    reader: Option<JoinHandle<()>>,
    updates: Arc<Mutex<Result<Vec<Value>, String>>>,
}

impl TableSubscription {
    pub(crate) fn start(mut command: Command, server: &str, shard: &str, queries: &[&str]) -> Self {
        let child = command
            .args([
                "subscribe",
                "-s",
                server,
                "--print-initial-update",
                "--timeout",
                "180",
                shard,
            ])
            .args(queries)
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to subscribe to fixture");
        let mut subscription = Self {
            child,
            reader: None,
            updates: Arc::new(Mutex::new(Ok(Vec::new()))),
        };
        let mut reader = BufReader::new(subscription.child.stdout.take().unwrap());
        let mut initial = String::new();
        assert!(reader.read_line(&mut initial).unwrap() > 0);
        let _: Value = serde_json::from_str(&initial).expect("initial subscription result");
        let recorded = subscription.updates.clone();
        subscription.reader = Some(thread::spawn(move || {
            for line in reader.lines() {
                let update = line.map_err(|error| error.to_string()).and_then(|line| {
                    serde_json::from_str(&line).map_err(|error| error.to_string())
                });
                match update {
                    Ok(update) => recorded.lock().unwrap().as_mut().unwrap().push(update),
                    Err(error) => {
                        *recorded.lock().unwrap() = Err(error);
                        return;
                    }
                }
            }
            *recorded.lock().unwrap() = Err("fixture subscription ended".into());
        }));
        subscription
    }

    pub fn inserts(&self, table: &str) -> Vec<Value> {
        self.rows(table, "inserts")
    }

    pub fn deletes(&self, table: &str) -> Vec<Value> {
        self.rows(table, "deletes")
    }

    fn rows(&self, table: &str, change: &str) -> Vec<Value> {
        self.updates
            .lock()
            .unwrap()
            .as_ref()
            .expect("fixture subscription failed")
            .iter()
            .flat_map(|update| update[table][change].as_array().into_iter().flatten())
            .cloned()
            .collect()
    }
}

impl Drop for TableSubscription {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
