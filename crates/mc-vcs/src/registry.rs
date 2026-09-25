//! provider registry —— 上游 `vcs.registry` / `vcs.For` 的复刻（M8-0 anchor 落**契约**，
//! 填充归 M8-2）。
//!
//! 上游用一个包级 `map[Kind]Provider`，`register()` 在 adapter 文件的 `init()` 里调用，
//! `For(kind)` 返回 `(nil,false)`。本仓**不用全局可变状态**（进程内全局 map 会让单测互相
//! 串扰）：[`Registry`] 是一个**值**，由装配点构造并注入 —— 这与 `mc-channel` 的
//! `Registry` 同款（`docs/61` §2.1）。

use std::collections::BTreeMap;
use std::sync::Arc;

use mc_core::vcs::VcsProviderKind;

use crate::provider::Provider;

/// registry 的查询错误。
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// 该 kind 没有注册实现（上游 `For` 的 `ok=false`）。
    #[error("vcs: no provider registered for kind `{}`", .0.as_str())]
    UnknownProvider(VcsProviderKind),
}

/// 每种 kind 一个 `Provider` 实例的注册表。
///
/// 用 `BTreeMap` 而非 `HashMap`：`kinds()` 的**顺序必须确定**（诊断与测试都要它），
/// 而定序在 `docs/61` §2.1 的「确定性」小节里是硬要求。
#[derive(Default)]
pub struct Registry {
    providers: BTreeMap<VcsProviderKind, Arc<dyn Provider>>,
}

impl Registry {
    /// 空 registry（注册由各 adapter 的 `register()` 完成）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个 provider。**同 kind 重复注册 = 后者覆盖前者**（上游 map 赋值语义）。
    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.insert(provider.kind(), provider);
    }

    /// 取某个 kind 的实现；未注册返回 [`RegistryError::UnknownProvider`]。
    pub fn get(&self, kind: VcsProviderKind) -> Result<Arc<dyn Provider>, RegistryError> {
        self.providers
            .get(&kind)
            .cloned()
            .ok_or(RegistryError::UnknownProvider(kind))
    }

    /// 是否注册了该 kind。
    pub fn contains(&self, kind: VcsProviderKind) -> bool {
        self.providers.contains_key(&kind)
    }

    /// 已注册的 kind（**确定序** = 枚举声明序，靠 `VcsProviderKind` 的 `Ord`/`BTreeMap`）。
    pub fn kinds(&self) -> Vec<VcsProviderKind> {
        self.providers.keys().copied().collect()
    }

    /// 已注册的数量。
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl std::fmt::Debug for Registry {
    /// trait 对象不可打印 ⇒ 只列已注册的 kind。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("kinds", &self.kinds())
            .finish()
    }
}
