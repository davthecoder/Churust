//! Type-keyed shared application state (a minimal DI registry).

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

/// A type-keyed registry holding at most one shared value per type — a minimal
/// dependency-injection container.
///
/// Each value is keyed by its [`TypeId`] and stored behind an [`Arc`], so reads
/// hand out cheap shared handles. This backs [`AppBuilder::state`] and the
/// [`State`](crate::State) extractor; you rarely construct one directly.
///
/// ```
/// use churust_core::StateMap;
///
/// let mut state = StateMap::new();
/// state.insert(42u32);
/// state.insert(String::from("hi"));
/// assert_eq!(*state.get::<u32>().unwrap(), 42);
/// assert_eq!(state.get::<String>().unwrap().as_str(), "hi");
/// assert!(state.get::<bool>().is_none());
/// ```
///
/// [`AppBuilder::state`]: crate::AppBuilder::state
#[derive(Clone, Default)]
pub struct StateMap {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl StateMap {
    /// Create an empty registry. Equivalent to [`StateMap::default`].
    ///
    /// ```
    /// use churust_core::StateMap;
    /// assert!(StateMap::new().get::<u32>().is_none());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) the single value held for type `T`.
    ///
    /// ```
    /// use churust_core::StateMap;
    /// let mut state = StateMap::new();
    /// state.insert(1u8);
    /// state.insert(2u8); // replaces the previous u8
    /// assert_eq!(*state.get::<u8>().unwrap(), 2);
    /// ```
    pub fn insert<T: Send + Sync + 'static>(&mut self, value: T) {
        self.map.insert(TypeId::of::<T>(), Arc::new(value));
    }

    /// Get a shared [`Arc`] handle to the value for type `T`, or `None` if no
    /// value of that type has been inserted.
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .and_then(|arc| arc.clone().downcast::<T>().ok())
    }
}

impl std::fmt::Debug for StateMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateMap")
            .field("len", &self.map.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_retrieves_by_type() {
        let mut m = StateMap::new();
        m.insert(7u32);
        m.insert(String::from("hello"));
        assert_eq!(*m.get::<u32>().unwrap(), 7);
        assert_eq!(m.get::<String>().unwrap().as_str(), "hello");
        assert!(m.get::<i64>().is_none());
    }
}
