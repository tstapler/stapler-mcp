use std::time::Duration;

use stapler_mcp_core::ports::{ClockPort, SleepPort};

pub struct NativeClock;

impl ClockPort for NativeClock {
    fn now_millis(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_millis() as u64
    }
}

pub struct NativeSleeper;

impl SleepPort for NativeSleeper {
    async fn sleep(&self, dur: Duration) {
        tokio::time::sleep(dur).await;
    }
}
