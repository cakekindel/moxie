use super::{
    cache_cell::CacheCell,
    dep_node::{DepNode, Dependent},
    Gc, Liveness,
};
use hashbrown::{
    hash_map::{DefaultHashBuilder, RawEntryMut},
    HashMap,
};

use std::{
    any::type_name,
    borrow::Borrow,
    fmt::{Debug, Formatter, Result as FmtResult},
    hash::{BuildHasher, Hash, Hasher},
    marker::PhantomData,
};

/// The result of failing to find a `key` in a cache with matching input. Passed
/// back to [`Namespace::store`] to initialize a value in the cache.
pub struct KeyMiss<'k, K: ?Sized, H> {
    inner: Result<Hashed<&'k K, H>, &'k K>,
    dependent: Dependent,
    node: Option<DepNode>,
}

impl<'k, K: ?Sized, H> KeyMiss<'k, K, H> {
    fn hashed(h: Hashed<&'k K, H>, node: Option<DepNode>, dependent: Dependent) -> Self {
        Self { inner: Ok(h), node, dependent }
    }

    pub(crate) fn just_key(k: &'k K, dependent: Dependent) -> Self {
        let node = DepNode::new();
        node.root(dependent);
        let dependent = node.as_dependent();
        Self { inner: Err(k), dependent, node: Some(node) }
    }

    pub(crate) fn dependent(&self) -> Dependent {
        self.dependent.clone()
    }
}

/// A query key that was hashed as part of an initial lookup and which can be
/// used to store fresh values back to the cache.
#[derive(Clone, Copy, Debug)]
struct Hashed<K, H> {
    key: K,
    hash: u64,
    hasher: PhantomData<H>,
}

/// A namespace stores all cached values for a particular query type.
pub(crate) struct Namespace<Scope, Input, Output, H = DefaultHashBuilder> {
    inner: HashMap<Scope, CacheCell<Input, Output>, H>,
}

impl<Scope, Input, Output, H> Default for Namespace<Scope, Input, Output, H>
where
    H: Default,
{
    fn default() -> Self {
        Self { inner: Default::default() }
    }
}

impl<Scope, Input, Output, H> Namespace<Scope, Input, Output, H>
where
    Scope: Eq + Hash + 'static,
    Input: 'static,
    Output: 'static,
    H: BuildHasher,
{
    fn hashed<'k, Key>(&self, key: &'k Key) -> Hashed<&'k Key, H>
    where
        Key: Hash + ?Sized,
    {
        let mut hasher = self.inner.hasher().build_hasher();
        key.hash(&mut hasher);
        Hashed { key, hash: hasher.finish(), hasher: PhantomData }
    }

    fn entry<'k, Key>(
        &self,
        hashed: &Hashed<&'k Key, H>,
    ) -> Option<(&Scope, &CacheCell<Input, Output>)>
    where
        Key: Eq + ?Sized,
        Scope: Borrow<Key>,
    {
        self.inner.raw_entry().from_hash(hashed.hash, |q| q.borrow().eq(hashed.key))
    }

    fn entry_mut<'k, Key>(
        &mut self,
        hashed: &Hashed<&'k Key, H>,
    ) -> RawEntryMut<Scope, CacheCell<Input, Output>, H>
    where
        Key: Eq + ?Sized,
        Scope: Borrow<Key>,
    {
        self.inner.raw_entry_mut().from_hash(hashed.hash, |q| q.borrow().eq(hashed.key))
    }

    pub fn get<'k, Key, Arg>(
        &self,
        key: &'k Key,
        input: &Arg,
        dependent: Dependent,
    ) -> Result<&Output, KeyMiss<'k, Key, H>>
    where
        Key: Eq + Hash + ?Sized,
        Scope: Borrow<Key>,
        Arg: PartialEq<Input> + ?Sized,
        Input: Borrow<Arg>,
    {
        let hashed = self.hashed(key);
        if let Some((_, cell)) = self.entry(&hashed) {
            cell.get(input, dependent).map_err(|d| KeyMiss::hashed(hashed, None, d))
        } else {
            let node = DepNode::new();
            node.root(dependent);
            let new_dep = node.as_dependent();
            Err(KeyMiss::hashed(hashed, Some(node), new_dep))
        }
    }

    pub fn store<Key>(&mut self, miss: KeyMiss<'_, Key, H>, input: Input, output: Output)
    where
        Key: Eq + Hash + ToOwned<Owned = Scope> + ?Sized,
        Scope: Borrow<Key>,
    {
        let dependent = miss.dependent;
        let hashed = miss.inner.unwrap_or_else(|k| self.hashed(k));
        match self.entry_mut(&hashed) {
            RawEntryMut::Occupied(occ) => {
                debug_assert!(miss.node.is_none(), "mustn't create nodes that aren't used");
                occ.into_mut().store(input, output, dependent);
            }
            RawEntryMut::Vacant(vac) => {
                vac.insert(
                    hashed.key.to_owned(),
                    CacheCell::new(
                        input,
                        output,
                        miss.node.expect("if no cell present, we must have created a fresh node"),
                    ),
                );
            }
        }
    }
}

impl<Scope, Input, Output, H> Gc for Namespace<Scope, Input, Output, H>
where
    Scope: Eq + Hash + 'static,
    Input: 'static,
    Output: 'static,
    H: 'static,
{
    fn mark(&mut self) {
        self.inner.values_mut().for_each(CacheCell::update_liveness);
    }

    fn sweep(&mut self) {
        self.inner.retain(|_, c| {
            let is_live = matches!(c.liveness(), Liveness::Live);
            c.mark_dead();
            is_live
        });
    }
}

impl<Scope, Input, Output, H> Debug for Namespace<Scope, Input, Output, H>
where
    Scope: Eq + Hash + 'static,
    Input: 'static,
    Output: 'static,
{
    // someday specialization might save us from these lame debug impls?
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        f.debug_map()
            .entry(&"scope", &type_name::<Scope>())
            .entry(&"input", &type_name::<Input>())
            .entry(&"output", &type_name::<Output>())
            .finish()
    }
}
