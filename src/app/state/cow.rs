//! Runtime-only copy-on-write storage for speculative application versions.
//!
//! The shared allocation is deliberately absent from snapshots and state
//! commitments: callers observe exactly the wrapped value.  Mutable access
//! detaches the current version before returning a reference.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Shared<T>(Arc<T>);

impl<T> Shared<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Arc::new(value))
    }

    /// Replace this version without cloning the previously shared value.
    pub(crate) fn replace(&mut self, value: T) {
        self.0 = Arc::new(value);
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<'a, T> IntoIterator for &'a Shared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.deref().into_iter()
    }
}

impl<T: PartialEq> PartialEq<T> for Shared<T> {
    fn eq(&self, other: &T) -> bool {
        self.deref() == other
    }
}

#[cfg(test)]
impl<T> Shared<T> {
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
