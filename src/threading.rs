use crate::*;
use mio::{net::TcpStream, Registry, Token};
use num_cpus;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

#[allow(dead_code)]
pub struct Worker {
    handle: thread::JoinHandle<()>,
    id: usize,
}
impl Worker {
    pub fn new(
        id: usize,
        receiver: mpsc::Receiver<BoxedJob>,
        registry: Registry,
        mut global_connections: Arc<Mutex<HashMap<usize, usize>>>,
        mut storage: Storage,
        mut extensions: Extensions,
    ) -> Self {
        let mut connections = HashMap::new();

        let handle = thread::Builder::new()
            .name(format!("Worker: {}", id))
            .spawn(move || {
                // Init all map's values
                extensions.init_all();
                let mut extensions = extensions.get_maps();

                loop {
                    let job = receiver.recv().unwrap();
                    job(
                        &mut storage,
                        &mut extensions,
                        &mut connections,
                        &registry,
                        &mut global_connections,
                    );
                }
            })
            .expect("Failed to create thread!");

        Self { id, handle }
    }
}

type Job = dyn FnOnce(
        &mut Storage,
        &mut ExtensionMap,
        &mut HashMap<Token, Connection>,
        &Registry,
        &mut Arc<Mutex<HashMap<usize, usize>>>,
    ) + Send
    + 'static;
type BoxedJob = Box<Job>;

pub struct ThreadPool {
    workers: Vec<(Worker, mpsc::Sender<BoxedJob>)>,
    last_send: usize,
}
impl ThreadPool {
    pub fn new(
        size: usize,
        registry: &Registry,
        connections: &Arc<Mutex<HashMap<usize, usize>>>,
        storage: Storage,
        extensions: Extensions,
    ) -> Self {
        assert!(size > 0);

        let mut workers = Vec::with_capacity(size);
        for id in 0..size - 1 {
            let (job_sender, job_receiver) = mpsc::channel();
            workers.push((
                Worker::new(
                    id,
                    job_receiver,
                    registry.try_clone().expect("Failed to clone registry!"),
                    Arc::clone(&connections),
                    Storage::clone(&storage),
                    Extensions::clone(&extensions),
                ),
                job_sender,
            ));
        }
        // Last
        let (job_sender, job_receiver) = mpsc::channel();
        workers.push((
            Worker::new(
                size - 1,
                job_receiver,
                registry.try_clone().expect("Failed to clone registry!"),
                Arc::clone(&connections),
                storage,
                extensions,
            ),
            job_sender,
        ));
        Self {
            workers,
            last_send: 0,
        }
    }
    /// Guarantees a valid id, within range of the worker vector
    fn next_send(&mut self) -> usize {
        if self.last_send + 1 >= self.workers.len() {
            self.last_send = 0;
            0
        } else {
            self.last_send += 1;
            self.last_send
        }
    }
    pub fn execute<F>(&mut self, f: F) -> usize
    where
        F: FnOnce(
                &mut Storage,
                &mut ExtensionMap,
                &mut HashMap<Token, Connection>,
                &Registry,
                &mut Arc<Mutex<HashMap<usize, usize>>>,
            ) + Send
            + 'static,
    {
        let job = Box::new(f);
        let id = self.next_send();
        let (_worker, sender) = self.workers.get(id).unwrap();
        sender.send(job).unwrap();
        id
    }
    /// # Errors
    /// If the worker id is out of range.
    pub fn execute_on<F>(&self, worker_id: usize, f: F) -> Result<(), ()>
    where
        F: FnOnce(
                &mut Storage,
                &mut ExtensionMap,
                &mut HashMap<Token, Connection>,
                &Registry,
                &mut Arc<Mutex<HashMap<usize, usize>>>,
            ) + Send
            + 'static,
    {
        let job = Box::new(f);
        match self.workers.get(worker_id) {
            Some((_worker, sender)) => {
                sender.send(job).unwrap();
                Ok(())
            }
            None => Err(()),
        }
    }
}

pub struct HandlerPool {
    pool: ThreadPool,
    connections: Arc<Mutex<HashMap<usize, usize>>>,
}
impl HandlerPool {
    pub fn new(storage: Storage, extensions: Extensions, registry: &Registry) -> Self {
        let global_connections = Arc::new(Mutex::new(HashMap::new()));
        Self {
            pool: ThreadPool::new(
                num_cpus::get() as usize - 1,
                registry,
                &global_connections,
                storage,
                extensions,
            ),
            connections: global_connections,
        }
    }

    pub fn accept(
        &mut self,
        socket: TcpStream,
        addr: SocketAddr,
        token: Token,
        connection: ConnectionSecurity,
    ) {
        let thread_id = self.pool.execute(move |_, _, connections, registry, _| {
            println!("Accepting new connection from: {:?}", addr);

            let mut connection = match Connection::new(socket, token, connection) {
                Some(connection) => connection,
                None => {
                    eprintln!("Unimplemented, shutting down socket!");
                    return;
                }
            };

            connection.register(registry);
            connections.insert(token, connection);
        });
        // Getting the lock on global connections! Have to release it quick!
        {
            let mut connections = self.connections.lock().unwrap();
            connections.insert(token.0, thread_id);
        }
    }

    pub fn handle(&mut self, event: crate::connection::MioEvent, _time: std::time::Instant) {
        let token = event.raw_token();

        // This takes an unnoticeable fraction of a second
        // Getting the lock on global connections! Have to release it quick!
        let thread_id = {
            let connections = self.connections.lock().unwrap();
            // println!("Global connections: {}", connections.len());
            match connections.get(&token) {
                Some(id) => *id,
                None => {
                    eprintln!("Connection not found! {:?}", token);
                    return;
                }
            }
        };

        self.pool
            .execute_on(
                thread_id,
                move |cache, extensions, connections, registry, global_connections| {
                    // println!("Thread-local connections: {}", connections.len());
                    if let Some(connection) = connections.get_mut(&event.token()) {
                        let pre_processing = std::time::Instant::now();
                        connection.ready(registry, &event, cache, extensions);
                        let post_processing = pre_processing.elapsed();
                        if connection.is_closed() {
                            connections.remove(&event.token());
                            // Getting the lock on global connections! Have to release it quick!
                            {
                                let mut global_connections = global_connections.lock().unwrap();
                                global_connections.remove(&event.raw_token());
                            }
                        }
                        println!(
              "Request took: {} μs. Processing took: {} μs. Processing and global cons: {} μs.",
              _time.elapsed().as_micros(),
              post_processing.as_micros(),
              pre_processing.elapsed().as_micros(),
            );
                    } else {
                        eprintln!("Connection not found!");
                    }
                },
            )
            .expect(
                "A incorrect thread id was passed, probably by the main HashMap of connections!",
            );
    }
}
