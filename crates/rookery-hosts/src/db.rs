//! The database for code that waits for its answers: rookery-core's store and its locks, which are
//! synchronous. A thread of its own runs a runtime with a small pool, so a caller on any thread, inside
//! the server's runtime or not, can wait for a query without blocking a worker that the query needs.

use std::sync::mpsc;

use futures_util::future::BoxFuture;
use sqlx::PgPool;

type Task = Box<dyn FnOnce(PgPool) -> BoxFuture<'static, ()> + Send>;

/// A handle to the thread; cheap to clone.
#[derive(Clone)]
pub struct Db {
    tasks: mpsc::Sender<Task>,
}

impl Db {
    /// Starts the thread with a pool of its own on `database_url`.
    pub fn start(database_url: &str) -> Result<Db, String> {
        let (tasks, todo) = mpsc::channel::<Task>();
        let (ready, started) = mpsc::channel::<Result<(), String>>();
        let url = database_url.to_owned();
        std::thread::Builder::new()
            .name("rookery-hosts-db".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        let _ = ready.send(Err(e.to_string()));
                        return;
                    }
                };
                let pool = runtime.block_on(async {
                    let options: sqlx::postgres::PgConnectOptions = url.parse()?;
                    sqlx::postgres::PgPoolOptions::new()
                        .max_connections(4)
                        .acquire_timeout(std::time::Duration::from_secs(10))
                        .connect_with(
                            options
                                .options([("timezone", "UTC")])
                                .application_name("rookery-hosts"),
                        )
                        .await
                });
                let pool = match pool {
                    Ok(pool) => pool,
                    Err(e) => {
                        let _ = ready.send(Err(format!("opening the database: {e}")));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                // Ends when every handle is dropped.
                while let Ok(task) = todo.recv() {
                    runtime.block_on(task(pool.clone()));
                }
                runtime.block_on(pool.close());
            })
            .map_err(|e| e.to_string())?;
        started
            .recv()
            .map_err(|_| "the database thread ended".to_owned())??;
        Ok(Db { tasks })
    }

    /// Runs `work` on the thread and waits for its answer.
    pub fn run<T, F>(&self, work: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(PgPool) -> BoxFuture<'static, T> + Send + 'static,
    {
        let (answer, answered) = mpsc::sync_channel(1);
        let task: Task = Box::new(move |pool| {
            Box::pin(async move {
                let _ = answer.send(work(pool).await);
            })
        });
        self.tasks
            .send(task)
            .expect("the database thread runs as long as a handle does");
        answered
            .recv()
            .expect("the database thread answers every task")
    }
}
