//! The async face of the store: two dedicated threads fed by bounded channels.

use std::path::Path;
use std::thread;

use tokio::sync::{mpsc, oneshot};

use crate::{Reader, Result, Store, StoreError};

type WriteJob = Box<dyn FnOnce(&mut Store) + Send>;
type ReadJob = Box<dyn FnOnce(&Queryable<'_>) + Send>;

/// What a read closure gets: the reader connection, or the writer when there is no separate
/// reader (in-memory stores).
#[derive(Debug)]
pub enum Queryable<'a> {
    Reader(&'a Reader),
    Writer(&'a Store),
}

impl Queryable<'_> {
    pub fn query(&self) -> crate::Queries<'_> {
        match self {
            Queryable::Reader(r) => r.query(),
            Queryable::Writer(s) => s.query(),
        }
    }
}

/// Cloneable async access to the store. Dropping the last handle stops the threads after they
/// finish queued work.
#[derive(Clone, Debug)]
pub struct StoreHandle {
    writer: mpsc::Sender<WriteJob>,
    reader: Option<mpsc::Sender<ReadJob>>,
}

const QUEUE: usize = 64;

impl StoreHandle {
    /// Opens the database at `path` (migrating it) and starts the writer and reader threads.
    pub fn open(path: &Path) -> Result<StoreHandle> {
        let store = Store::open(path)?;
        let reader = Reader::open(path)?;
        StoreHandle::spawn(store, Some(reader))
    }

    /// Starts threads for an existing store. Without a reader, reads run on the writer thread.
    pub fn spawn(store: Store, reader: Option<Reader>) -> Result<StoreHandle> {
        let spawn_err =
            |e: std::io::Error| StoreError::Corrupt(format!("cannot start a store thread: {e}"));
        let (wtx, mut wrx) = mpsc::channel::<WriteJob>(QUEUE);
        thread::Builder::new()
            .name("pulse-store-writer".into())
            .spawn(move || {
                let mut store = store;
                while let Some(job) = wrx.blocking_recv() {
                    job(&mut store);
                }
            })
            .map_err(spawn_err)?;
        let reader = match reader {
            Some(reader) => {
                let (rtx, mut rrx) = mpsc::channel::<ReadJob>(QUEUE);
                thread::Builder::new()
                    .name("pulse-store-reader".into())
                    .spawn(move || {
                        while let Some(job) = rrx.blocking_recv() {
                            job(&Queryable::Reader(&reader));
                        }
                    })
                    .map_err(spawn_err)?;
                Some(rtx)
            }
            None => None,
        };
        Ok(StoreHandle { writer: wtx, reader })
    }

    /// Runs `f` on the writer thread.
    pub async fn write<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Store) -> Result<R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let job: WriteJob = Box::new(move |s| {
            let _ = tx.send(f(s));
        });
        self.writer.send(job).await.map_err(|_| StoreError::Closed)?;
        rx.await.map_err(|_| StoreError::Closed)?
    }

    /// Runs `f` on the reader thread (or the writer thread for in-memory stores). Reads see
    /// every write that completed before they were sent.
    pub async fn read<R, F>(&self, f: F) -> Result<R>
    where
        R: Send + 'static,
        F: FnOnce(&Queryable<'_>) -> Result<R> + Send + 'static,
    {
        match &self.reader {
            Some(reader) => {
                let (tx, rx) = oneshot::channel();
                let job: ReadJob = Box::new(move |q| {
                    let _ = tx.send(f(q));
                });
                reader.send(job).await.map_err(|_| StoreError::Closed)?;
                rx.await.map_err(|_| StoreError::Closed)?
            }
            None => self.write(move |s| f(&Queryable::Writer(s))).await,
        }
    }
}
