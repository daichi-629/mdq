use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rquickjs::{CatchResultExt, Context as JsContext, Runtime};
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

impl ScriptEngine for QuickJsEngine {
    fn evaluate(&self, source: &str, bindings: &Value) -> Result<Value> {
        let runtime = Runtime::new()?;
        runtime.set_memory_limit(self.limits.memory_bytes);
        runtime.set_max_stack_size(self.limits.max_stack_bytes);
        let started = Instant::now();
        let interrupted = Arc::new(AtomicBool::new(false));
        let interrupt_flag = interrupted.clone();
        let timeout = self.limits.timeout;
        runtime.set_interrupt_handler(Some(Box::new(move || {
            let expired = started.elapsed() > timeout;
            if expired {
                interrupt_flag.store(true, Ordering::Relaxed);
            }
            expired
        })));
        let context = JsContext::full(&runtime)?;
        let value = context.with(|ctx| -> Result<Value> {
            let bindings = rquickjs_serde::to_value(ctx.clone(), bindings)?;
            ctx.globals().set("__mdq", bindings)?;
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
            let result: rquickjs::Value<'_> = ctx
                .eval(wrapped)
                .catch(&ctx)
                .map_err(|error| {
                    let msg = error.to_string();
                    let first_line = msg.lines().next().unwrap_or(&msg).to_owned();
                    anyhow::anyhow!("{first_line}")
                })?;
            rquickjs_serde::from_value(result).context("JavaScript result is not serializable")
        })?;
        if interrupted.load(Ordering::Relaxed) {
            anyhow::bail!("JavaScript execution exceeded {:?}", self.limits.timeout);
        }
        Ok(value)
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
