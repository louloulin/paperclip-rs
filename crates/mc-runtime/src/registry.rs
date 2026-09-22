//! [`AdapterRegistry`]：`AgentType` → adapter 的并发注册表。
//!
//! 本类型**取代** `mc-http/src/state.rs::AdapterRegistryStub`（M0 脚手架里的空壳）。
//! 与旧空壳的三点差异：
//!
//! 1. 键是 [`AgentType`] 而不是任意字符串 —— 白名单外的 key 无法注册，拼错不会静默通过；
//! 2. 值是 `Arc<dyn RuntimeAdapter>` —— 拿到的句柄可以跨 task 用，注册表本身不负责生命周期；
//! 3. 注册/查询都是 `&self`（内部 `RwLock`），所以 `Arc<AdapterRegistry>` 能在装配
//!    （`RuntimeHandles`）后继续注册，不需要 `&mut`。
//!
//! 默认值是**空注册表**（不是"注册好内置 adapter"）：`mc-http` 的测试、
//! `mc-conformance` 的回放都靠空注册表起服务，绝不能因为"默认值顺手注册了 pi"
//! 而让机器上有没有 `pi` 二进制影响测试结果。要内置 adapter 请显式调用
//! [`AdapterRegistry::with_builtin_adapters`]。

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};

use crate::adapter::RuntimeAdapter;
use crate::adapters::PiLocal;
use crate::catalog::AgentType;

/// `AgentType` → adapter 的注册表。
#[derive(Default)]
pub struct AdapterRegistry {
    adapters: RwLock<HashMap<AgentType, std::sync::Arc<dyn RuntimeAdapter>>>,
}

impl std::fmt::Debug for AdapterRegistry {
    /// `dyn RuntimeAdapter` 不是 `Debug`，所以手写：只打印已注册的类型，够定位问题。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterRegistry")
            .field("kinds", &self.names())
            .finish()
    }
}

impl AdapterRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册本片已实现的**内置 adapter**（当前只有 pi → [`PiLocal`]）。
    ///
    /// 构造 `PiLocal` 不会探测/执行 `pi`（探测是 [`RuntimeAdapter::probe_version`] 的事），
    /// 所以机器上没有 `pi` 也能安全装配 —— 真正的可用性由 probe 结果决定。
    pub fn with_builtin_adapters() -> Self {
        let registry = Self::new();
        registry.register(std::sync::Arc::new(PiLocal::default()));
        registry
    }

    /// 注册（同类型覆盖），返回被替换掉的旧 adapter。
    pub fn register(
        &self,
        adapter: std::sync::Arc<dyn RuntimeAdapter>,
    ) -> Option<std::sync::Arc<dyn RuntimeAdapter>> {
        let kind = adapter.kind();
        self.write().insert(kind, adapter)
    }

    /// 按类型取 adapter。
    pub fn get(&self, kind: AgentType) -> Option<std::sync::Arc<dyn RuntimeAdapter>> {
        self.read().get(&kind).cloned()
    }

    /// 是否注册了该类型。
    pub fn contains(&self, kind: AgentType) -> bool {
        self.read().contains_key(&kind)
    }

    /// 已注册的数量。
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    /// 已注册的类型，**按上游白名单顺序**返回（稳定输出，便于比对/落日志）。
    pub fn kinds(&self) -> Vec<AgentType> {
        let map = self.read();
        AgentType::ALL
            .into_iter()
            .filter(|kind| map.contains_key(kind))
            .collect()
    }

    /// 已注册的类型名（= provider key）。
    pub fn names(&self) -> Vec<String> {
        self.kinds().iter().map(|k| k.as_str().to_owned()).collect()
    }

    /// 清空（测试用；生产装配后不应调）。
    pub fn clear(&self) {
        self.write().clear();
    }

    /// 读锁：中毒（某个持锁线程 panic）时仍然取回数据 —— 注册表是纯数据，
    /// 没有需要靠 panic 传播的中间态，让后续 run 因为一次 panic 全线失败更糟。
    fn read(
        &self,
    ) -> std::sync::RwLockReadGuard<'_, HashMap<AgentType, std::sync::Arc<dyn RuntimeAdapter>>>
    {
        self.adapters.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(
        &self,
    ) -> std::sync::RwLockWriteGuard<'_, HashMap<AgentType, std::sync::Arc<dyn RuntimeAdapter>>>
    {
        self.adapters
            .write()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::adapter::{
        AdapterCapabilities, AdapterError, CancelOutcome, EventDecoder, LaunchRequest,
        ProtocolFamily, RunHandle, RunId, VersionProbe,
    };

    /// 只为注册表测试存在的哑 adapter：不 spawn 任何东西。
    struct StubAdapter {
        kind: AgentType,
    }

    struct NullDecoder;

    impl EventDecoder for NullDecoder {
        fn push_line(&mut self, _line: &str) -> Vec<crate::adapter::RuntimeEvent> {
            Vec::new()
        }

        fn finish(&mut self) -> Vec<crate::adapter::RuntimeEvent> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl RuntimeAdapter for StubAdapter {
        fn kind(&self) -> AgentType {
            self.kind
        }

        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                protocol: ProtocolFamily::Opaque,
                streaming: false,
                thinking: false,
                tool_events: false,
                usage_reporting: false,
                resume: false,
                version_probe: false,
                launch_header: self.kind.launch_header().to_owned(),
            }
        }

        async fn launch(&self, _request: LaunchRequest) -> Result<RunHandle, AdapterError> {
            unreachable!("stub adapter never launches")
        }

        async fn cancel(&self, _run_id: &RunId) -> Result<CancelOutcome, AdapterError> {
            Ok(CancelOutcome::NotRunning)
        }

        async fn probe_version(&self) -> Result<VersionProbe, AdapterError> {
            unreachable!("stub adapter never probes")
        }

        fn decoder(&self) -> Box<dyn EventDecoder> {
            Box::new(NullDecoder)
        }
    }

    fn stub(kind: AgentType) -> Arc<dyn RuntimeAdapter> {
        Arc::new(StubAdapter { kind })
    }

    #[test]
    fn default_registry_is_empty() {
        // 空默认值是刻意的契约（见模块文档）：测试/回放不依赖机器上有没有 pi。
        let registry = AdapterRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.get(AgentType::Pi).is_none());
        assert_eq!(registry.names(), Vec::<String>::new());
    }

    #[test]
    fn builtin_registry_holds_pi_without_probing() {
        let registry = AdapterRegistry::with_builtin_adapters();
        assert_eq!(registry.names(), vec!["pi".to_owned()]);
        let adapter = registry.get(AgentType::Pi).expect("pi adapter");
        assert_eq!(adapter.kind(), AgentType::Pi);
    }

    #[test]
    fn register_replaces_and_returns_previous() {
        let registry = AdapterRegistry::new();
        assert!(registry.register(stub(AgentType::Pi)).is_none());
        let previous = registry.register(stub(AgentType::Pi));
        assert!(previous.is_some());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn kinds_follow_upstream_whitelist_order() {
        let registry = AdapterRegistry::new();
        registry.register(stub(AgentType::Zeroclaw));
        registry.register(stub(AgentType::Claude));
        registry.register(stub(AgentType::Pi));
        assert_eq!(
            registry.kinds(),
            vec![AgentType::Claude, AgentType::Pi, AgentType::Zeroclaw]
        );
        assert_eq!(
            registry.names(),
            vec!["claude".to_owned(), "pi".to_owned(), "zeroclaw".to_owned()]
        );
    }

    #[test]
    fn concurrent_registration_of_all_25_types() {
        // 25 个线程同时注册 25 个不同类型：一个都不能丢、不能互踩。
        let registry = Arc::new(AdapterRegistry::new());
        let mut handles = Vec::new();
        for kind in AgentType::ALL {
            let registry = Arc::clone(&registry);
            handles.push(std::thread::spawn(move || {
                assert!(registry.register(stub(kind)).is_none());
            }));
        }
        for handle in handles {
            handle.join().expect("registration thread");
        }
        assert_eq!(registry.len(), 25);
        assert_eq!(registry.kinds(), AgentType::ALL.to_vec());
        for kind in AgentType::ALL {
            assert!(registry.contains(kind), "{kind} 丢了");
        }
    }

    #[test]
    fn clear_empties_registry() {
        let registry = AdapterRegistry::with_builtin_adapters();
        assert!(!registry.is_empty());
        registry.clear();
        assert!(registry.is_empty());
    }
}
