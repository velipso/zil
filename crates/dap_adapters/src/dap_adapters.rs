mod gdb;

#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;

use dap::{
    DapRegistry,
    adapters::{
        DapDelegate, DebugAdapter, DebugAdapterBinary, DebugAdapterName,
    },
};
use gdb::GdbDebugAdapter;
use gpui::{App, BorrowAppContext};
use serde_json::json;

pub fn init(cx: &mut App) {
    cx.update_default_global(|registry: &mut DapRegistry, _cx| {
        registry.add_adapter(Arc::from(GdbDebugAdapter));

        #[cfg(any(test, feature = "test-support"))]
        {
            registry.add_adapter(Arc::from(dap::FakeAdapter {}));
        }
    })
}

#[cfg(test)]
mod test_mocks {
    use super::*;

    pub(crate) struct MockDelegate {
        worktree_root: PathBuf,
    }

    impl MockDelegate {
        pub(crate) fn new() -> Arc<dyn adapters::DapDelegate> {
            Arc::new(Self {
                worktree_root: PathBuf::from("/tmp/test"),
            })
        }
    }

    #[async_trait::async_trait]
    impl adapters::DapDelegate for MockDelegate {
        fn worktree_id(&self) -> settings::WorktreeId {
            settings::WorktreeId::from_usize(0)
        }

        fn worktree_root_path(&self) -> &std::path::Path {
            &self.worktree_root
        }

        fn http_client(&self) -> Arc<dyn http_client::HttpClient> {
            unimplemented!("Not needed for tests")
        }

        fn toolchain_store(&self) -> Arc<dyn language::LanguageToolchainStore> {
            unimplemented!("Not needed for tests")
        }

        fn fs(&self) -> Arc<dyn fs::Fs> {
            unimplemented!("Not needed for tests")
        }

        fn output_to_console(&self, _msg: String) {}

        async fn which(&self, _command: &std::ffi::OsStr) -> Option<PathBuf> {
            None
        }

        async fn read_text_file(&self, _path: &util::rel_path::RelPath) -> Result<String> {
            Ok(String::new())
        }

        async fn shell_env(&self) -> collections::HashMap<String, String> {
            collections::HashMap::default()
        }

        fn is_headless(&self) -> bool {
            false
        }
    }
}
