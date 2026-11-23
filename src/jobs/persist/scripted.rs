//! Portable controlled writer. The fixture owns each write-return gate and is
//! the only actor that advances the shared monotonic clock.
use super::*;

#[derive(Debug)]
pub(crate) struct WriteReturn {
    pub path: PathBuf,
    pub release: oneshot::Sender<()>,
}
#[derive(Debug)]
pub(crate) struct ScriptedWriter {
    pub files: Mutex<BTreeMap<PathBuf, Vec<u8>>>,
    pub virtual_files: bool,
    events: tokio::sync::mpsc::UnboundedSender<WriteReturn>,
}
impl ScriptedWriter {
    pub fn new(
        virtual_files: bool,
    ) -> (Arc<Self>, tokio::sync::mpsc::UnboundedReceiver<WriteReturn>) {
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                files: Mutex::new(BTreeMap::new()),
                virtual_files,
                events,
            }),
            receiver,
        )
    }
    pub fn publish(&self, path: &std::path::Path, bytes: &[u8]) -> Published {
        assert!(
            self.files
                .lock()
                .unwrap()
                .insert(path.to_owned(), bytes.to_vec())
                .is_none()
        );
        Published {
            committed: true,
            durability: Durability::Ok,
            cleanup: Cleanup::Done,
        }
    }
    pub fn returning(&self, path: &std::path::Path) {
        let (release, wait) = oneshot::channel();
        // A dropped harness releases the worker, including during unwinding.
        if self
            .events
            .send(WriteReturn {
                path: path.to_owned(),
                release,
            })
            .is_ok()
        {
            let _ = wait.blocking_recv();
        }
    }
}
