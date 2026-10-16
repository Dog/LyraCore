include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

use bindings::{DbConnection, GameChatEventLiveTableAccess, SubscriptionHandle};
use lyracore_test_support::{actor, Standalone, POLL_TIMEOUT};
use spacetimedb_sdk::{DbContext, EventTable};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

struct Delivery {
    message: String,
    cached_count: u64,
    cached_rows: usize,
}

struct Subscribed {
    connection: DbConnection,
    pump: Option<JoinHandle<()>>,
    _subscription: SubscriptionHandle,
    deliveries: Receiver<Delivery>,
}

impl Subscribed {
    fn connect(shard: &Standalone) -> Self {
        let connection = DbConnection::builder()
            .with_uri(shard.server())
            .with_database_name(shard.shard_name())
            .with_token(Some(shard.owner_token()))
            .build()
            .expect("fixture connection");
        let (delivery, deliveries) = mpsc::channel();
        connection
            .db
            .game_chat_event_live()
            .on_insert(move |ctx, row| {
                delivery
                    .send(Delivery {
                        message: row.message.clone(),
                        cached_count: ctx.db.game_chat_event_live().count(),
                        cached_rows: ctx.db.game_chat_event_live().iter().count(),
                    })
                    .expect("fixture receiver remains connected");
            });
        let (applied, ready) = mpsc::channel();
        let failed = applied.clone();
        let subscription = connection
            .subscription_builder()
            .on_applied(move |_| {
                applied.send(Ok(())).unwrap();
            })
            .on_error(move |_, error| {
                failed.send(Err(error.to_string())).unwrap();
            })
            .subscribe(["SELECT * FROM game_chat_event_live"]);
        let pump = connection.run_threaded();
        let subscribed = Self {
            connection,
            pump: Some(pump),
            _subscription: subscription,
            deliveries,
        };
        ready
            .recv_timeout(POLL_TIMEOUT)
            .expect("subscription must apply")
            .expect("subscription must succeed");
        subscribed
    }

    fn expect_line(&self, message: &str) {
        let delivery = self
            .deliveries
            .recv_timeout(POLL_TIMEOUT)
            .expect("Speech insert");
        assert_eq!(delivery.message, message);
        assert_eq!(delivery.cached_count, 0);
        assert_eq!(delivery.cached_rows, 0);
    }
}

impl Drop for Subscribed {
    fn drop(&mut self) {
        let _ = self.connection.disconnect();
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

fn speak(shard: &Standalone, message: &str) {
    shard.assert_call(
        "gw_send_chat",
        &[
            &actor("1"),
            "0",
            "7",
            &serde_json::to_string(message).unwrap(),
        ],
    );
}

#[test]
#[ignore = "requires the SpacetimeDB 2.7.1 CLI and Wasm toolchain"]
fn native_notifications_keep_transaction_order_without_cached_rows_or_reconnect_replay() {
    let mut shard = Standalone::start("notification-events");
    shard.publish_module();
    shard.assert_call("claim_operator", &[]);
    shard.assert_call("debug_spawn_player_entity", &["1"]);
    let first = Subscribed::connect(&shard);
    speak(&shard, "first");
    speak(&shard, "second");
    first.expect_line("first");
    first.expect_line("second");
    assert!(shard
        .query_rows("SELECT * FROM game_chat_event_live")
        .is_empty());
    assert!(shard.query_rows("SELECT * FROM game_chat_event").is_empty());
    drop(first);

    speak(&shard, "while disconnected");
    let next = Subscribed::connect(&shard);
    assert!(next.deliveries.try_recv().is_err());
    speak(&shard, "after reconnect");
    next.expect_line("after reconnect");
}
