//! Registry: 存储 workflow 定义 + routine 实现。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use thiserror::Error;
use uuid::Uuid;

use crate::routine::{Routine, RoutineError};
use crate::types::WorkflowDefinition;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("workflow already registered: {0}")]
    DuplicateWorkflow(String),
    #[error("workflow not found: {0}")]
    WorkflowNotFound(String),
    #[error("routine already registered: {0}")]
    DuplicateRoutine(String),
    #[error("routine not found: {0}")]
    RoutineNotFound(String),
    #[error("dag validation: {0}")]
    DagInvalid(String),
}

#[derive(Default)]
pub struct RoutineRegistry {
    inner: RwLock<HashMap<String, Arc<dyn Routine>>>,
}

impl std::fmt::Debug for RoutineRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read().expect("routine registry poisoned");
        let keys: Vec<&String> = inner.keys().collect();
        f.debug_struct("RoutineRegistry").field("routines", &keys).finish()
    }
}

impl RoutineRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, routine: Arc<dyn Routine>) -> Result<(), RoutineError> {
        let mut inner = self.inner.write().expect("routine registry poisoned");
        let key = routine.key();
        if inner.contains_key(key) {
            return Err(RoutineError::NotFound(format!(
                "duplicate routine key: {key}"
            )));
        }
        inner.insert(key.into(), routine);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<Arc<dyn Routine>> {
        let inner = self.inner.read().expect("routine registry poisoned");
        inner.get(key).cloned()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().expect("routine registry poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[must_use]
    pub fn keys(&self) -> Vec<String> {
        let inner = self.inner.read().expect("routine registry poisoned");
        let mut keys: Vec<String> = inner.keys().cloned().collect();
        keys.sort();
        keys
    }
}

#[derive(Default)]
pub struct WorkflowRegistry {
    inner: RwLock<HashMap<String, WorkflowDefinition>>,
}

impl std::fmt::Debug for WorkflowRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.read().expect("workflow registry poisoned");
        let keys: Vec<&String> = inner.keys().collect();
        f.debug_struct("WorkflowRegistry").field("workflows", &keys).finish()
    }
}

impl WorkflowRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &self,
        def: WorkflowDefinition,
    ) -> Result<(), RegistryError> {
        let key = def.key().to_string();
        if let WorkflowDefinition::Pipeline(p) = &def {
            // Validate DAG: no cycles, no unknown deps
            validate_pipeline_dag(&p.steps)?;
        }
        let mut inner = self.inner.write().expect("workflow registry poisoned");
        if inner.contains_key(&key) {
            return Err(RegistryError::DuplicateWorkflow(key));
        }
        inner.insert(key, def);
        Ok(())
    }

    pub fn unregister(&self, key: &str) -> Option<WorkflowDefinition> {
        let mut inner = self.inner.write().expect("workflow registry poisoned");
        inner.remove(key)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<WorkflowDefinition> {
        let inner = self.inner.read().expect("workflow registry poisoned");
        inner.get(key).cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<WorkflowDefinition> {
        let inner = self.inner.read().expect("workflow registry poisoned");
        let mut defs: Vec<WorkflowDefinition> = inner.values().cloned().collect();
        defs.sort_by(|a, b| a.key().cmp(b.key()));
        defs
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().expect("workflow registry poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Validates a pipeline DAG: no cycles, no self-deps.
pub fn validate_pipeline_dag(
    steps: &[crate::types::PipelineStep],
) -> Result<(), RegistryError> {
    let mut ids: HashMap<Uuid, &crate::types::PipelineStep> = HashMap::new();
    for s in steps {
        if ids.contains_key(&s.id) {
            return Err(RegistryError::DagInvalid(format!(
                "duplicate step id {}",
                s.id
            )));
        }
        ids.insert(s.id, s);
    }
    for s in steps {
        for d in &s.depends_on {
            if *d == s.id {
                return Err(RegistryError::DagInvalid(format!(
                    "step {} depends on itself",
                    s.id
                )));
            }
            if !ids.contains_key(d) {
                return Err(RegistryError::DagInvalid(format!(
                    "step {} depends on unknown step {}",
                    s.id, d
                )));
            }
        }
    }
    // Cycle detection via Kahn's algorithm.
    let mut indeg: HashMap<Uuid, usize> = ids.keys().map(|k| (*k, 0)).collect();
    for s in steps {
        for d in &s.depends_on {
            indeg.entry(*d).and_modify(|v| *v += 1);
            // Note: edge goes from dep -> s, so we don't increment s.indeg
        }
    }
    let mut queue: std::collections::VecDeque<Uuid> = indeg
        .iter()
        .filter(|(_, v)| **v == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut popped = 0usize;
    while let Some(k) = queue.pop_front() {
        popped += 1;
        if let Some(s) = ids.get(&k) {
            for s2 in steps {
                if s2.depends_on.contains(&k) {
                    if let Some(v) = indeg.get_mut(&s2.id) {
                        *v -= 1;
                        if *v == 0 {
                            queue.push_back(s2.id);
                        }
                    }
                }
            }
        }
    }
    if popped != ids.len() {
        return Err(RegistryError::DagInvalid("cycle detected".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PipelineDefinition, PipelineStep, RoutineDefinition, RoutineKind};

    fn routine(key: &str) -> WorkflowDefinition {
        WorkflowDefinition::Routine(RoutineDefinition {
            id: Uuid::new_v4(),
            key: key.into(),
            label: key.into(),
            description: None,
            kind: RoutineKind::Script,
            config_schema: serde_json::Value::Null,
        })
    }

    #[test]
    fn register_and_lookup_routine() {
        let reg = WorkflowRegistry::new();
        reg.register(routine("check_inbox")).unwrap();
        assert!(reg.get("check_inbox").is_some());
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn duplicate_routine_rejected() {
        let reg = WorkflowRegistry::new();
        reg.register(routine("dup")).unwrap();
        assert!(reg.register(routine("dup")).is_err());
    }

    #[test]
    fn unregister_removes_definition() {
        let reg = WorkflowRegistry::new();
        reg.register(routine("temp")).unwrap();
        assert!(reg.unregister("temp").is_some());
        assert!(reg.get("temp").is_none());
    }

    #[test]
    fn dag_no_cycles_ok() {
        let s1 = PipelineStep::new("a", "r1");
        let s2 = PipelineStep::new("b", "r2").depends_on(vec![s1.id]);
        let s3 = PipelineStep::new("c", "r3").depends_on(vec![s2.id]);
        let pipe = PipelineDefinition {
            id: Uuid::new_v4(),
            key: "p".into(),
            label: "P".into(),
            description: None,
            steps: vec![s1, s2, s3],
            dag_error: None,
        };
        assert!(validate_pipeline_dag(&pipe.steps).is_ok());
    }

    #[test]
    fn dag_cycle_detected() {
        let s1 = PipelineStep::new("a", "r1");
        let s2 = PipelineStep::new("b", "r2").depends_on(vec![s1.id]);
        let s3 = PipelineStep::new("c", "r3").depends_on(vec![s2.id]);
        // Create cycle: s1 depends on s3
        let mut s1_with_dep = s1.clone();
        s1_with_dep.depends_on = vec![s3.id];

        let pipe = PipelineDefinition {
            id: Uuid::new_v4(),
            key: "p".into(),
            label: "P".into(),
            description: None,
            steps: vec![s1_with_dep, s2, s3],
            dag_error: None,
        };
        assert!(validate_pipeline_dag(&pipe.steps).is_err());
    }

    #[test]
    fn dag_self_dep_rejected() {
        let mut s = PipelineStep::new("a", "r1");
        s.depends_on.push(s.id);
        let err = validate_pipeline_dag(&[s]).unwrap_err();
        assert!(matches!(err, RegistryError::DagInvalid(_)));
    }

    #[test]
    fn dag_unknown_dep_rejected() {
        let s = PipelineStep::new("a", "r1");
        let bogus = PipelineStep::new("b", "r2").depends_on(vec![Uuid::new_v4()]);
        let pipe = vec![s, bogus];
        assert!(validate_pipeline_dag(&pipe).is_err());
    }
}
