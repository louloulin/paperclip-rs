//! 分页 + 排序请求。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageSize(u32);

impl PageSize {
    pub const DEFAULT: u32 = 50;
    pub const MAX: u32 = 200;

    pub fn new(n: u32) -> Self {
        Self(n.clamp(1, Self::MAX))
    }

    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self::new(Self::DEFAULT)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PageRequest {
    pub size: PageSize,
    pub after: Option<String>,
    pub before: Option<String>,
    pub sort: SortOrder,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<String>,
    pub total: Option<u64>,
}

impl<T> Page<T> {
    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            next: None,
            total: None,
        }
    }

    pub fn from_items(items: Vec<T>) -> Self {
        Self {
            items,
            next: None,
            total: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_size_clamps() {
        assert_eq!(PageSize::new(0).as_u32(), 1);
        assert_eq!(PageSize::new(500).as_u32(), PageSize::MAX);
        assert_eq!(PageSize::new(50).as_u32(), 50);
    }

    #[test]
    fn page_default_is_desc() {
        let p = PageRequest::default();
        assert_eq!(p.sort, SortOrder::Desc);
        assert_eq!(p.size.as_u32(), 50);
    }

    #[test]
    fn empty_page() {
        let p: Page<i32> = Page::empty();
        assert!(p.items.is_empty());
        assert!(p.next.is_none());
    }
}
