use {
    crate::{
        caps::{CallsiteId, ScopeId},
        our_prelude::*,
        state::*,
        Component, ComponentSpawn, Witness,
    },
    downcast_rs::*,
    futures::future::AbortHandle,
    parking_lot::Mutex,
    std::{
        any::{Any, TypeId},
        collections::HashMap,
        fmt::{Debug, Formatter, Result as FmtResult},
        hash::{Hash, Hasher},
        panic::{AssertUnwindSafe, UnwindSafe},
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Weak,
        },
        task::Waker,
    },
};

/// Provides a component with access to the persistent state store and futures executor.
#[derive(Clone, Debug)]
pub struct Scope {
    inner: Arc<InnerScope>,
}

#[derive(Clone, Debug)]
struct WeakScope {
    inner: Weak<InnerScope>,
}

impl Scope {
    pub fn id(&self) -> ScopeId {
        self.inner.id
    }

    pub fn compose_child_with_witness<C, W>(&self, child_id: ScopeId, props: C, witness: W) -> W
    where
        C: Component,
        W: Witness,
    {
        info!("composing child with witness");
        let child_scope = self.child(child_id);
        child_scope.install_witness(witness);
        self.compose_child(child_id, props);
        child_scope.remove_witness().unwrap()
    }

    pub(crate) fn root<Spawner>(spawner: Spawner, waker: Waker, exit: AbortHandle) -> Self
    where
        Spawner: ComponentSpawn + 'static,
    {
        Self {
            inner: Arc::new(InnerScope {
                id: ScopeId::root(),
                revision: Arc::new(AtomicU64::new(0)),
                spawner: Mutex::new(Box::new(spawner)),
                states: States::new(waker.clone()),
                parent: None,
                children: Default::default(),
                bind_order: Default::default(),
                records: Default::default(),
                exit,
                waker,
            }),
        }
    }

    #[doc(hidden)]
    pub fn child(&self, id: ScopeId) -> Self {
        let inner = &self.inner;

        inner
            .children
            .lock()
            .entry(id)
            .or_insert_with(|| {
                let parent = Some(self.weak());
                self.inner.bind_order.lock().push(id);

                Self {
                    inner: Arc::new(InnerScope {
                        id,
                        revision: Arc::new(AtomicU64::new(0)),
                        exit: inner.exit.clone(),
                        waker: inner.waker.clone(),
                        spawner: Mutex::new(inner.spawner.lock().child()),
                        states: States::new(inner.waker.clone()),
                        parent,
                        children: Default::default(),
                        bind_order: Default::default(),
                        records: Default::default(),
                    }),
                }
            })
            .clone()
    }

    fn weak(&self) -> WeakScope {
        WeakScope {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn top_level_exit_handle(&self) -> AbortHandle {
        self.inner.exit.clone()
    }

    fn prepare_to_compose(&self) {
        self.inner.bind_order.lock().clear();
        self.for_each_record_storage(Records::flush_before_composition);
    }

    fn finish_composition(&self) {
        // TODO garbage collect state, children, and tasks
        self.for_each_record_storage(|records| {
            span!(
                Level::TRACE,
                "iterating_record_storage",
                records = field::debug(&records)
            )
            .enter(|| {
                records.show_witnesses_after_composition(self.clone());
            })
        })
    }
}

impl Scope {
    #[inline]
    #[doc(hidden)]
    pub fn compose_child<C: Component>(&self, id: ScopeId, props: C) {
        span!(
            tokio_trace::Level::TRACE,
            "compose_child",
            id = field::debug(&id),
            name = field::display(&C::type_name()),
        )
        .enter(|| {
            let child = self.child(id);

            // TODO only run if things have changed
            {
                let child = child.clone();

                trace!("preparing child to compose");
                child.prepare_to_compose();

                trace!("composing child");
                C::compose(child, props);
            }

            trace!("child composition finished");
            child.finish_composition();
        })
    }

    #[inline]
    #[doc(hidden)]
    pub fn state<S: 'static + Any + UnwindSafe>(
        &self,
        callsite: CallsiteId,
        f: impl FnOnce() -> S,
    ) -> Guard<S> {
        self.inner.states.get_or_init(callsite, f)
    }

    #[inline]
    #[doc(hidden)]
    pub fn task<F>(&self, _callsite: CallsiteId, fut: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner
            .spawner
            .lock()
            .spawn_local(
                Box::new(AssertUnwindSafe(fut).catch_unwind().map(|r| {
                    if let Err(e) = r {
                        error!({ error = field::debug(&e) }, "user code panicked");
                    }
                }))
                .into(),
            )
            .unwrap();
    }

    #[inline]
    #[doc(hidden)]
    pub fn record<N>(&self, node: N)
    where
        N: Debug + 'static,
    {
        self.with_record_storage(|storage| {
            trace!({ node = field::debug(&node) }, "recording a node");
            storage.record = Some(node);
        });
    }

    #[inline]
    #[doc(hidden)]
    pub fn install_witness<W>(&self, witness: W)
    where
        W: Witness,
    {
        self.with_record_storage(|storage: &mut RecordStorage<W::Node>| {
            trace!("installing witness");
            storage
                .witnesses
                .insert(TypeId::of::<W>(), Box::new(witness))
        });
    }

    #[inline]
    #[doc(hidden)]
    pub fn remove_witness<W>(&self) -> Option<W>
    where
        W: Witness,
    {
        self.with_record_storage(|storage: &mut RecordStorage<W::Node>| {
            trace!("removing witness");
            storage.witnesses.remove(&TypeId::of::<W>())
        })
        .map(Downcast::into_any)
        .map(|any: Box<std::any::Any>| any.downcast().unwrap())
        .map(|boxed: Box<W>| *boxed)
    }
}

struct InnerScope {
    pub id: ScopeId,
    pub revision: Arc<AtomicU64>,
    parent: Option<WeakScope>,
    states: States,
    children: Mutex<HashMap<ScopeId, Scope>>,
    bind_order: Mutex<Vec<ScopeId>>,
    records: Mutex<HashMap<TypeId, Mutex<Box<dyn Records>>>>,
    spawner: Mutex<Box<dyn ComponentSpawn>>,
    waker: Waker,
    exit: AbortHandle,
}

impl Debug for InnerScope {
    fn fmt(&self, _f: &mut Formatter) -> FmtResult {
        unimplemented!()
    }
}

impl Drop for InnerScope {
    fn drop(&mut self) {
        trace!({ scope = field::debug(&self) }, "inner scope dropping");
    }
}

impl Hash for InnerScope {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.id.hash(hasher);
        self.revision.load(Ordering::SeqCst).hash(hasher);
    }
}

impl PartialEq for InnerScope {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.revision.load(Ordering::SeqCst) == other.revision.load(Ordering::SeqCst)
            && self.states == other.states
    }
}

impl Eq for InnerScope {}

unsafe impl Send for InnerScope {}

impl Scope {
    fn with_record_storage<Node, Ret>(
        &self,
        op: impl FnOnce(&mut RecordStorage<Node>) -> Ret,
    ) -> Ret
    where
        Node: Debug + 'static,
    {
        let mut storage_by_node = self.inner.records.lock();
        let storage: &mut Mutex<Box<dyn Records>> = storage_by_node
            .entry(TypeId::of::<Node>())
            .or_insert_with(|| {
                let storage: RecordStorage<Node> = RecordStorage::default();
                Mutex::new(Box::new(storage))
            });
        let storage: &mut dyn Records = &mut **storage.lock();
        let storage: &mut std::any::Any = storage.as_any_mut();
        let storage: &mut RecordStorage<Node> = storage.downcast_mut().unwrap();

        span!(
            tokio_trace::Level::TRACE,
            "with_record_storage",
            scope = field::debug(&self.id()),
            storage = field::debug(&storage),
        )
        .enter(|| {
            // not panic-safe, maybe fix?
            op(storage)
        })
    }

    fn for_each_record_storage(&self, op: impl Fn(&mut dyn Records)) {
        self.inner.records.lock().values_mut().for_each(|b| {
            let mut guard = b.lock();
            let storage = &mut **guard;
            span!(
                tokio_trace::Level::TRACE,
                "for_each_record_storage",
                storage = field::debug(&storage)
            )
            .enter(|| {
                // not panic-safe, maybe fix?
                op(storage)
            })
        })
    }

    // will panic if called on the root
    fn parent_id(&self) -> ScopeId {
        self.inner
            .parent
            .as_ref()
            .and_then(|p| p.inner.upgrade().map(|p| p.id))
            // only the root has a null parent, and we never "see" the root bc it never gets
            // any witnesses installed
            .unwrap()
    }
}

#[derive(Debug)]
struct RecordStorage<Node>
where
    Node: Debug + 'static,
{
    record: Option<Node>,
    witnesses: HashMap<TypeId, Box<dyn Witness<Node = Node>>>,
}

trait Records: Debug + Downcast + 'static {
    /// Clear recorded nodes from storage. Should be called immediately before composing in this
    /// scope.
    fn flush_before_composition(&mut self);

    /// Show the current component hierarchy and associated recordings to all installed witnesses.
    ///
    /// Probably needs a better name. Takes the current scope as an argument so that it can
    /// traverse to children. Vague name, poor API. We'll refactor this another time.
    fn show_witnesses_after_composition(&mut self, scope: Scope);
}
impl_downcast!(Records);

impl<Node> Records for RecordStorage<Node>
where
    Node: Debug + 'static,
{
    fn flush_before_composition(&mut self) {
        self.record = None;
    }

    fn show_witnesses_after_composition(&mut self, start: Scope) {
        span!(
            tokio_trace::Level::TRACE,
            "showing starting scope",
            starting = field::debug(&start.id()),
        );
        let mut to_visit: Vec<Scope> = Vec::new();

        // this code duplication is awkward but is a useful hack for now
        for witness in self.witnesses.values_mut() {
            trace!("showing starting scope");
            if let Some(record) = &self.record {
                witness.see(start.id(), start.parent_id(), record);
            }
        }

        let children = start.inner.children.lock();
        let bind_order = start.inner.bind_order.lock();
        to_visit.extend(bind_order.iter().map(|id| children[id].clone()));

        while let Some(visiting) = to_visit.pop() {
            span!(
                tokio_trace::Level::TRACE,
                "showing_scope",
                scope = field::debug(&visiting.id())
            )
            .enter(|| {
                trace!(
                    { visiting = field::debug(&visiting.id()) },
                    "visiting scope"
                );
                let parent = visiting.parent_id();

                visiting.with_record_storage(|storage| {
                    for witness in self.witnesses.values_mut() {
                        trace!("showing witness");
                        if let Some(record) = &storage.record {
                            witness.see(visiting.id(), parent, record);
                        }
                    }
                });

                let children = visiting.inner.children.lock();
                let bind_order = visiting.inner.bind_order.lock();
                to_visit.extend(bind_order.iter().map(|id| children[id].clone()));
            })
        }
    }
}

impl<Node> Default for RecordStorage<Node>
where
    Node: Debug + 'static,
{
    fn default() -> Self {
        Self {
            record: None,
            witnesses: Default::default(),
        }
    }
}
