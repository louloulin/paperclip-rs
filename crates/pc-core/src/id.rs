//! 类型安全 ID（newtype 模式）。
//!
//! 用法：`type CompanyId = Id<Company>;`
//! 编译期防止把 [`AgentId`] 误传给期望 [`CompanyId`] 的函数。
//!
//! [`AgentId`]: crate::Id
//! [`CompanyId`]: crate::Id

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id<T: ?Sized> {
    inner: Uuid,
    #[serde(skip)]
    _marker: PhantomData<fn() -> T>,
}

impl<T: ?Sized> Id<T> {
    pub fn new() -> Self {
        Self {
            inner: Uuid::now_v7(),
            _marker: PhantomData,
        }
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self {
            inner: uuid,
            _marker: PhantomData,
        }
    }

    pub fn as_uuid(&self) -> Uuid {
        self.inner
    }
}

impl<T: ?Sized> Default for Id<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> std::fmt::Display for Id<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner)
    }
}

impl<T: ?Sized> From<Uuid> for Id<T> {
    fn from(u: Uuid) -> Self {
        Self::from_uuid(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Company;
    #[derive(Debug, PartialEq)]
    struct Agent;

    #[test]
    fn ids_are_unique_per_type() {
        let a: Id<Company> = Id::new();
        let b: Id<Agent> = Id::new();
        assert_ne!(a.as_uuid(), b.as_uuid());
    }

    #[test]
    fn id_round_trips_via_uuid() {
        let original: Id<Company> = Id::new();
        let u = original.as_uuid();
        let restored: Id<Company> = u.into();
        assert_eq!(original, restored);
    }

    #[test]
    fn id_serializes_as_string() {
        let id: Id<Company> = Id::new();
        let s = serde_json::to_string(&id).unwrap();
        assert!(s.contains('-'));
    }
}
