use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rquickjs::{CatchResultExt, Context as JsContext, Ctx, Runtime};
use serde_json::Value;

pub trait ScriptEngine: Send + Sync {
    fn evaluate(&self, source: &str, bindings: &Value) -> Result<Value>;
}

#[derive(Clone, Debug)]
pub struct ScriptLimits {
    pub memory_bytes: usize,
    pub max_stack_bytes: usize,
    pub timeout: Duration,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            max_stack_bytes: 512 * 1024,
            timeout: Duration::from_millis(500),
        }
    }
}

#[derive(Default)]
pub struct QuickJsEngine {
    limits: ScriptLimits,
}

#[derive(Clone)]
pub(crate) struct ScriptHostControl {
    deadline: Arc<Mutex<Instant>>,
    paused_timeout: Arc<Mutex<Option<Duration>>>,
}

impl ScriptHostControl {
    pub(crate) fn begin_host_phase(&self) {
        let now = Instant::now();
        let Ok(mut deadline) = self.deadline.lock() else {
            return;
        };
        let remaining = deadline.saturating_duration_since(now);
        if let Ok(mut paused) = self.paused_timeout.lock() {
            *paused = Some(remaining);
            *deadline = now + Duration::from_secs(24 * 60 * 60);
        }
    }

    pub(crate) fn end_host_phase(&self) {
        if let Ok(mut paused) = self.paused_timeout.lock()
            && let Some(remaining) = paused.take()
            && let Ok(mut deadline) = self.deadline.lock()
        {
            *deadline = Instant::now() + remaining;
        }
    }
}

impl QuickJsEngine {
    pub(crate) fn evaluate_with_setup<F>(
        &self,
        source: &str,
        bindings: &Value,
        host_memory_bytes: usize,
        setup: F,
    ) -> Result<Value>
    where
        F: for<'js> FnOnce(&ScriptHostControl, Ctx<'js>) -> rquickjs::Result<()>,
    {
        let runtime = Runtime::new()?;
        // QuickJS counts host-provided bindings against its heap limit. Give
        // serialization a size-based provisional allowance, then replace it
        // with the measured host-data baseline plus the script memory budget.
        let serialized_bytes = serde_json::to_vec(bindings)?.len();
        let provisional_limit = self
            .limits
            .memory_bytes
            .saturating_add(host_memory_bytes)
            .saturating_add(serialized_bytes.saturating_mul(8));
        runtime.set_memory_limit(provisional_limit);
        runtime.set_max_stack_size(self.limits.max_stack_bytes);
        let interrupted = Arc::new(AtomicBool::new(false));
        let deadline = Arc::new(Mutex::new(Instant::now() + self.limits.timeout));
        let host_control = ScriptHostControl {
            deadline: deadline.clone(),
            paused_timeout: Arc::new(Mutex::new(None)),
        };
        let context = JsContext::full(&runtime)?;
        context.with(|ctx| -> Result<()> {
            let bindings = rquickjs_serde::to_value(ctx.clone(), bindings)?;
            ctx.globals().set("__mdq", bindings)?;
            setup(&host_control, ctx)?;
            Ok(())
        })?;
        runtime.run_gc();
        let host_memory = usize::try_from(runtime.memory_usage().malloc_size).unwrap_or(0);
        runtime.set_memory_limit(
            host_memory
                .saturating_add(host_memory_bytes)
                .saturating_add(self.limits.memory_bytes),
        );
        if let Ok(mut deadline) = deadline.lock() {
            *deadline = Instant::now() + self.limits.timeout;
        }

        // Host data preparation can be substantial for a large vault. The
        // timeout is intended to constrain user JavaScript, not serialization
        // of the read-only bindings before that JavaScript starts.
        let interrupt_flag = interrupted.clone();
        let execution_active = Arc::new(AtomicBool::new(true));
        let active_flag = execution_active.clone();
        let interrupt_deadline = deadline;
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let expired = active_flag.load(Ordering::Relaxed)
                && interrupt_deadline
                    .lock()
                    .is_ok_and(|deadline| Instant::now() > *deadline);
            if expired {
                interrupt_flag.store(true, Ordering::Relaxed);
            }
            expired
        })));
        let value = context.with(|ctx| -> Result<Value> {
            let wrapped = format!(
                r#"
                "use strict";
                globalThis.eval = undefined;
                const app = undefined;
                const require = undefined;
                const process = undefined;
                const fetch = undefined;
                const XMLHttpRequest = undefined;
                const WebSocket = undefined;
                const document = undefined;
                const window = undefined;
                (() => {{ {source} }})()
                "#
            );
            let result = ctx.eval(wrapped).catch(&ctx).map_err(|error| {
                let msg = error.to_string();
                let first_line = msg.lines().next().unwrap_or(&msg).to_owned();
                anyhow::anyhow!("{first_line}")
            });
            execution_active.store(false, Ordering::Relaxed);
            let result: rquickjs::Value<'_> = result?;
            rquickjs_serde::from_value(result).context("JavaScript result is not serializable")
        });
        if interrupted.load(Ordering::Relaxed) {
            anyhow::bail!("JavaScript execution exceeded {:?}", self.limits.timeout);
        }
        value
    }
}

impl ScriptEngine for QuickJsEngine {
    fn evaluate(&self, source: &str, bindings: &Value) -> Result<Value> {
        self.evaluate_with_setup(source, bindings, 0, |_, _| Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluates_with_read_only_host_data() {
        let result = QuickJsEngine::default()
            .evaluate("return __mdq.value * 2;", &json!({"value": 3}))
            .unwrap();
        assert_eq!(result, json!(6));
    }

    #[test]
    fn binding_serialization_does_not_consume_the_execution_timeout() {
        let engine = QuickJsEngine {
            limits: ScriptLimits {
                timeout: Duration::from_millis(10),
                ..ScriptLimits::default()
            },
        };
        let bindings = json!({"vault_data": "x".repeat(16 * 1024 * 1024)});

        let result = engine.evaluate("return 3;", &bindings).unwrap();

        assert_eq!(result, json!(3));
    }

    #[test]
    fn host_bindings_do_not_consume_the_script_memory_budget() {
        let engine = QuickJsEngine {
            limits: ScriptLimits {
                memory_bytes: 1024 * 1024,
                ..ScriptLimits::default()
            },
        };
        let bindings = json!({"vault_data": "x".repeat(2 * 1024 * 1024)});

        let result = engine
            .evaluate("return __mdq.vault_data.length;", &bindings)
            .unwrap();

        assert_eq!(result, json!(2 * 1024 * 1024));
    }

    #[test]
    fn does_not_expose_node_or_obsidian_hosts() {
        let result = QuickJsEngine::default()
            .evaluate(
                "return [typeof app, typeof require, typeof process, typeof fetch];",
                &json!({}),
            )
            .unwrap();
        assert_eq!(
            result,
            json!(["undefined", "undefined", "undefined", "undefined"])
        );
    }

    #[test]
    fn eval_is_blocked_in_sandbox() {
        let err = QuickJsEngine::default()
            .evaluate("return eval('1+1');", &json!({}))
            .unwrap_err();
        assert!(
            err.to_string().contains("not a function") || err.to_string().contains("undefined"),
            "expected eval to be blocked, got: {err}"
        );
    }
}
