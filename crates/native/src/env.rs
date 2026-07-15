use stapler_mcp_core::ports::EnvPort;

pub struct NativeEnv;

impl EnvPort for NativeEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn home_dir(&self) -> Option<String> {
        std::env::var("HOME").ok()
    }
}
