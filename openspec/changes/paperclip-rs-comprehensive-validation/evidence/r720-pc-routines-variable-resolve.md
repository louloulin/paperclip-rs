# R720 — pc-routines/src/pure.rs (补足 variable collection/resolve/merge)

## 目标

补足 Node `services/routines.ts` 中 collect / resolve / merge / sanitize / assert 五个变量解析 helpers。

## 新增 helpers（5 个 + 3 个数据结构）

| Node 函数 | Rust 函数 |
|---|---|
| `stringifyRoutineVariableValue` | `stringify_routine_variable_value(raw: &Value)` |
| `sanitizeRoutineVariableInputs` | `sanitize_routine_variable_inputs(variables)` |
| `assertRoutineVariableDefinitions` | `assert_routine_variable_definitions(variables)` |
| `assertScheduleCompatibleVariables` | `assert_schedule_compatible_variables(variables)` |
| `collectProvidedRoutineVariables` | `collect_provided_routine_variables(source, payload, variables)` |
| `resolveRoutineVariableValues` | `resolve_routine_variable_values(variables, input)` |
| `mergeRoutineRunPayload` | `merge_routine_run_payload(payload, variables)` |

数据结构：RoutineVariable, RoutineVariableValue, RoutineSource, ResolveRoutineVariablesInput

## 测试结果

```
cargo test -p pc-routines --lib pure
running 27 tests
...
test result: ok. 27 passed; 0 failed
```

## 关键设计

- RoutineVariableValue 用 Rust enum 替代 Node 的 union (string | number | boolean | null)
- RoutineSource enum + as_str() 替代字符串字面量
- resolve_routine_variable_values 严格按 Node 优先级：automaticVariables > provided > defaultValue；缺失 required 报错
- merge_routine_run_payload 返回新 Value 而非 mutate，保持 pure

## 累计

pc-routines crate 业务逻辑 pure helpers：27 PASS（R713 16 + R720 11）。
