//! The kernel's shared state.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use rookery_store::Store;
use tokio_util::sync::CancellationToken;

use crate::config::Config;
use crate::conn::Connections;
use crate::methods::Methods;

/// A cheap handle to the kernel.
#[derive(Clone)]
pub struct Kernel(Arc<Inner>);

pub struct Inner {
    pub config: Config,
    pub methods: Methods,
    pub store: Store,
    pub conns: Connections,
    /// Cancelled when the server stops.
    pub shutdown: CancellationToken,
    state: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl Deref for Kernel {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Kernel {
    pub fn new(config: Config, store: Store, methods: Methods) -> Kernel {
        Kernel(Arc::new(Inner {
            config,
            methods,
            store,
            conns: Connections::default(),
            shutdown: CancellationToken::new(),
            state: Mutex::new(HashMap::new()),
        }))
    }

    /// A facility's own state, made the first time it is asked for.
    pub fn facility<T: Default + Send + Sync + 'static>(&self) -> Arc<T> {
        let mut map = self.state.lock().unwrap();
        let any = map
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Arc::new(T::default()))
            .clone();
        any.downcast::<T>()
            .expect("a facility's state has its own type")
    }

    /// Puts a facility's state in place, before anything asks for it.
    pub fn set_facility<T: Send + Sync + 'static>(&self, value: T) {
        self.state
            .lock()
            .unwrap()
            .insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// A facility's state that was put in place with [`Kernel::set_facility`].
    pub fn get_facility<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        let map = self.state.lock().unwrap();
        map.get(&TypeId::of::<T>())
            .cloned()
            .and_then(|any| any.downcast::<T>().ok())
    }
}
